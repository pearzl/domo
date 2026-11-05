//! In-process mock agent used by tests and by projects without a real CLI.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::AsyncWriteExt;

use crate::models::{
    CONFIRM_AUTO, JOB_ROLE_ACCEPTANCE, JOB_ROLE_ANALYSIS, JOB_ROLE_DIRECT, JOB_ROLE_EXECUTOR,
    JOB_ROLE_REPLAN_REVIEW, JOB_ROLE_SUMMARY, JOB_ROLE_VERIFIER,
};

use super::adapter::{AgentAdapter, AgentConfig, AgentRun, AgentSpec, TurnContext};

async fn run_mock(spec: &AgentSpec) -> Result<AgentRun> {
    let delay_ms = std::env::var("DOMO_MOCK_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(120);
    let delay_ms = if spec.role == JOB_ROLE_REPLAN_REVIEW {
        std::env::var("DOMO_MOCK_REPLAN_REVIEW_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(delay_ms)
    } else if spec.role == JOB_ROLE_EXECUTOR {
        std::env::var("DOMO_MOCK_EXECUTOR_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(delay_ms)
    } else {
        delay_ms
    };
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;

    let context = read_context(&spec.job_dir)?;
    let streamed = write_mock_stream(spec, &context).await?;
    if let Some(reason) = mock_transport_failure(&spec.role, &context) {
        tokio::fs::write(spec.job_dir.join("stderr.log"), &reason).await?;
        return Ok(AgentRun {
            stdout: String::new(),
            stderr: reason,
            exit_code: Some(1),
            timed_out: false,
            stalled: false,
            session_id: None,
            usage: None,
        });
    }
    let result = match mock_role_failure(&spec.role, &context) {
        Some(result) => result,
        None => match spec.role.as_str() {
            JOB_ROLE_ANALYSIS => mock_analysis(&context),
            JOB_ROLE_DIRECT => mock_direct(&context, &spec.cwd)?,
            JOB_ROLE_EXECUTOR => mock_execute(&context, &spec.cwd)?,
            JOB_ROLE_VERIFIER => mock_verify(&context, &spec.cwd)?,
            JOB_ROLE_ACCEPTANCE => mock_project_acceptance(&context, &spec.cwd)?,
            JOB_ROLE_SUMMARY => mock_summary(&context),
            JOB_ROLE_REPLAN_REVIEW => {
                let reviewer = context["replan_review"]["reviewer"]
                    .as_str()
                    .unwrap_or("planner");
                let decision = std::env::var(if reviewer == "parent" {
                    "DOMO_MOCK_PARENT_REPLAN_DECISION"
                } else {
                    "DOMO_MOCK_PLANNER_REPLAN_DECISION"
                })
                .unwrap_or_else(|_| "plan".into());
                if decision == "plan" {
                    if reviewer == "parent" {
                        let mut tasks = vec![
                            serde_json::json!({"key": "T1.3", "title": "替代子任务", "description": "完成父任务范围内的剩余交付", "acceptance": ["产物存在"], "deps": []}),
                        ];
                        if std::env::var("DOMO_MOCK_PARENT_REPLAN_DEPENDENCIES").as_deref()
                            == Ok("1")
                        {
                            tasks.push(serde_json::json!({"key": "T1.4", "title": "后继子任务", "description": "读取前置子任务成果并完成交付", "acceptance": ["产物存在"], "deps": ["T1.3"]}));
                        }
                        serde_json::json!({"kind": "plan", "goal": context["project"]["goal"], "summary": "mock 父任务局部替代方案", "skip_confirmation": true,
                            "tasks": tasks})
                    } else {
                        mock_replan_plan(
                            &context,
                            context["replan_review"]["request"]["reason"]
                                .as_str()
                                .unwrap_or("任务申请"),
                        )
                    }
                } else {
                    serde_json::json!({"kind": "replan_review", "decision": decision,
                        "reason": format!("mock {reviewer} 审批决策"), "guidance": "核对固定输入与权限；缺少资源时等待补齐，不降低验收标准"})
                }
            }
            other => return Err(anyhow!("mock 不支持角色 {other}")),
        },
    };
    let mut result = attach_mock_usage(result, &context);
    if matches!(
        result["kind"].as_str(),
        Some("done" | "derive" | "direct_done")
    ) {
        let files = if result["kind"] == "direct_done" {
            vec!["domo_mock/direct.txt".to_string()]
        } else if result["kind"] == "derive" {
            Vec::new()
        } else {
            vec![format!(
                "domo_mock/{}.txt",
                context["task"]["key"].as_str().unwrap_or("T1")
            )]
        };
        result["commit_files"] = serde_json::json!(files);
    }
    if spec.role == JOB_ROLE_ACCEPTANCE && result.get("criteria").is_some() {
        result["contract_id"] = context["contract"]["id"].clone();
        result["artifact_version"] = context["artifact_version"].clone();
    }
    if spec.role == JOB_ROLE_VERIFIER && result.get("pass").is_some() {
        let pass = result["pass"].as_bool().unwrap_or(false);
        let criteria = context["task"]["acceptance"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let key = context["task"]["key"].as_str().unwrap_or_default();
        let path = format!("domo_mock/{key}.txt");
        let exists = spec.cwd.join(&path).is_file();
        result["evidence"] = serde_json::json!({"contract_id": context["contract"]["id"],
            "artifact_version": context["artifact_version"], "artifacts": [path], "risks": [], "human_checks": [],
            "criteria": criteria.iter().map(|criterion| serde_json::json!({"criterion": criterion,
                "passed": pass, "checks": [{"method": format!("检查 {path} 是否存在"),
                    "result": format!("文件存在：{exists}；mock 验收结论：{}", result["reason"])}]})).collect::<Vec<_>>()});
    }
    let path = spec.job_dir.join("result.json");
    tokio::fs::write(&path, serde_json::to_vec_pretty(&result)?).await?;
    let stdout = format!("mock job {} finished\n", spec.job_id);
    if streamed {
        append_stdout(&spec.job_dir, &stdout).await?;
    } else {
        let _ = tokio::fs::write(spec.job_dir.join("stdout.log"), &stdout).await;
    }
    Ok(AgentRun {
        stdout,
        stderr: String::new(),
        exit_code: Some(0),
        timed_out: false,
        stalled: false,
        session_id: None,
        usage: None,
        reported_error: None,
    })
}

/// Mock streaming is opt-in through `DOMO_MOCK_STREAM=1`, optionally narrowed
/// by `DOMO_MOCK_STREAM_GOAL`. Each segment is flushed to stdout.log so the
/// output endpoint can observe growth while the job is still running.
async fn write_mock_stream(spec: &AgentSpec, context: &serde_json::Value) -> Result<bool> {
    let enabled = std::env::var("DOMO_MOCK_STREAM")
        .map(|value| value == "1")
        .unwrap_or(false);
    if !enabled {
        return Ok(false);
    }
    let goal = context
        .get("project")
        .and_then(|value| value.get("goal"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let matches_goal = std::env::var("DOMO_MOCK_STREAM_GOAL")
        .ok()
        .map(|marker| goal.contains(&marker))
        .unwrap_or(true);
    if !matches_goal {
        return Ok(false);
    }
    let interval = mock_env_i64("DOMO_MOCK_STREAM_INTERVAL_MS", 250).max(1) as u64;
    let lines = mock_env_i64("DOMO_MOCK_STREAM_LINES", 8).max(1) as u64;
    append_stdout(
        &spec.job_dir,
        &format!("mock job {} streaming\n", spec.job_id),
    )
    .await?;
    for index in 1..=lines {
        tokio::time::sleep(Duration::from_millis(interval)).await;
        append_stdout(
            &spec.job_dir,
            &format!("mock job {} segment {index}\n", spec.job_id),
        )
        .await?;
    }
    Ok(true)
}

async fn append_stdout(job_dir: &Path, text: &str) -> Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(job_dir.join("stdout.log"))
        .await?;
    file.write_all(text.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}

fn read_context(job_dir: &Path) -> Result<serde_json::Value> {
    let raw = std::fs::read_to_string(job_dir.join("context.json"))
        .with_context(|| format!("读取 {} 失败", job_dir.display()))?;
    Ok(serde_json::from_str(&raw)?)
}

/// Mock usage is opt-in so existing mock flows keep results without a usage
/// object. A goal containing `[nousage]` exercises the unknown path.
fn attach_mock_usage(
    mut result: serde_json::Value,
    context: &serde_json::Value,
) -> serde_json::Value {
    let enabled = std::env::var("DOMO_MOCK_USAGE")
        .map(|value| value == "1")
        .unwrap_or(false);
    if !enabled {
        return result;
    }
    let goal = context
        .get("project")
        .and_then(|value| value.get("goal"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_lowercase();
    if goal.contains("[nousage]") {
        return result;
    }
    let input = mock_env_i64("DOMO_MOCK_INPUT_TOKENS", 1200);
    let output = mock_env_i64("DOMO_MOCK_OUTPUT_TOKENS", 800);
    let cost = mock_env_f64("DOMO_MOCK_COST", 0.0125);
    let mut usage = serde_json::json!({
        "input_tokens": input,
        "output_tokens": output,
        "cost": cost,
    });
    if let Ok(raw) = std::env::var("DOMO_MOCK_USAGE_DURATION_MS") {
        if let Ok(value) = raw.parse::<i64>() {
            usage["duration_ms"] = serde_json::json!(value);
        }
    }
    if let Some(object) = result.as_object_mut() {
        object.insert("usage".into(), usage);
    }
    result
}

fn mock_env_i64(key: &str, fallback: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(fallback)
}

fn mock_env_f64(key: &str, fallback: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(fallback)
}

fn mock_analysis(context: &serde_json::Value) -> serde_json::Value {
    let goal = context
        .get("project")
        .and_then(|value| value.get("goal"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let proposed_goal = std::env::var("DOMO_MOCK_ANALYSIS_GOAL_OVERRIDE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| goal.to_string());
    let mode = context
        .get("project")
        .and_then(|value| value.get("confirm_mode"))
        .and_then(|value| value.as_str())
        .unwrap_or("manual");
    let has_planner_message = context
        .get("messages")
        .and_then(|value| value.as_array())
        .map(|messages| {
            messages
                .iter()
                .any(|message| message.get("role").and_then(|v| v.as_str()) == Some("planner"))
        })
        .unwrap_or(false);
    if goal.contains("[ask]") && !has_planner_message {
        return serde_json::json!({
            "kind": "questions",
            "summary": "需要确认目标",
            "questions": ["希望优先实现哪个使用场景？"]
        });
    }
    if let Some(reason) = context
        .get("project")
        .and_then(|value| value.get("replan_reason"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let mut result = mock_replan_plan(context, reason);
        if std::env::var("DOMO_MOCK_ANALYSIS_GOAL_OVERRIDE").is_ok() {
            result["goal"] = serde_json::json!(proposed_goal);
        }
        return result;
    }
    let skip = goal.contains("[skipforce]") || mode == CONFIRM_AUTO;
    let force_plan = std::env::var("DOMO_MOCK_ANALYSIS_FORCE_PLAN").is_ok_and(|value| value == "1");
    if goal.contains("[direct]") && !force_plan {
        return serde_json::json!({
            "kind": "direct",
            "summary": "这个目标足够简单，可以直接完成",
            "goal": proposed_goal,
            "plan": ["创建目标文件", "检查文件内容"],
            "verification": ["读取文件确认内容"],
            "skip_confirmation": skip
        });
    }
    let (summary, tasks) = if goal.contains("[single]") {
        (
            "mock 分析：单个任务",
            serde_json::json!([
                {
                    "key": "T1",
                    "title": "单任务",
                    "description": "在 domo_mock/T1.txt 写入单任务的内容。",
                    "acceptance": ["domo_mock/T1.txt 存在且包含 T1"],
                    "deps": []
                }
            ]),
        )
    } else if goal.contains("[parallel]") {
        (
            "mock 分析：三个可并行的实现任务",
            serde_json::json!([
                {
                    "key": "T1",
                    "title": "并行任务一",
                    "description": "在 domo_mock/T1.txt 写入并行任务一的内容。",
                    "acceptance": ["domo_mock/T1.txt 存在且包含 T1"],
                    "deps": []
                },
                {
                    "key": "T2",
                    "title": "并行任务二",
                    "description": "在 domo_mock/T2.txt 写入并行任务二的内容。",
                    "acceptance": ["domo_mock/T2.txt 存在且包含 T2"],
                    "deps": []
                },
                {
                    "key": "T3",
                    "title": "并行任务三",
                    "description": "在 domo_mock/T3.txt 写入并行任务三的内容。",
                    "acceptance": ["domo_mock/T3.txt 存在且包含 T3"],
                    "deps": []
                }
            ]),
        )
    } else {
        (
            "mock 分析：两个有依赖的实现任务",
            serde_json::json!([
                {
                    "key": "T1",
                    "title": "写入基础文件",
                    "description": "在 domo_mock/T1.txt 写入任务一的内容。",
                    "acceptance": ["domo_mock/T1.txt 存在且包含 T1"],
                    "deps": []
                },
                {
                    "key": "T2",
                    "title": "基于前置结果写入文件",
                    "description": "确认 domo_mock/T1.txt 存在，然后写入 domo_mock/T2.txt。",
                    "acceptance": ["domo_mock/T1.txt 与 domo_mock/T2.txt 均存在"],
                    "deps": ["T1"]
                }
            ]),
        )
    };
    serde_json::json!({
        "kind": "plan",
        "summary": summary,
        "goal": proposed_goal,
        "assumptions": ["使用 mock Agent"],
        "acceptance": ["产物符合目标描述"],
        "tasks": tasks,
        "skip_confirmation": skip
    })
}

fn mock_direct(context: &serde_json::Value, cwd: &Path) -> Result<serde_json::Value> {
    let goal = context
        .get("project")
        .and_then(|value| value.get("goal"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let dir = cwd.join("domo_mock");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("direct.txt"), "mock direct completion\n")?;
    Ok(serde_json::json!({
        "kind": "direct_done",
        "summary": "已直接完成目标",
        "report": format!(
            "# Mock 直接完成报告\n\n目标：{}\n\n已完成 `domo_mock/direct.txt` 并通过自检。",
            goal
        )
    }))
}

fn mock_replan_plan(context: &serde_json::Value, reason: &str) -> serde_json::Value {
    let goal = context
        .get("project")
        .and_then(|value| value.get("goal"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if goal.contains("[replan-direct]") || goal.contains("[replan-direct-mismatch]") {
        let draft_goal = if goal.contains("[replan-direct-mismatch]") {
            "规划者擅自修改的目标"
        } else {
            goal
        };
        return serde_json::json!({
            "kind": "direct",
            "summary": format!("mock 直接重规划：{reason}"),
            "goal": draft_goal,
            "plan": ["直接完成重规划工作"],
            "verification": ["核对直接完成产物"],
            "skip_confirmation": true
        });
    }
    let completed: HashSet<String> = context
        .get("completed_tasks")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("key").and_then(|value| value.as_str()))
                .map(|value| value.to_string())
                .collect()
        })
        .unwrap_or_default();
    let canceled: Vec<String> = context
        .get("canceled_tasks")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("key").and_then(|value| value.as_str()))
                .filter(|key| !completed.contains(*key))
                .map(|value| value.to_string())
                .collect()
        })
        .unwrap_or_default();
    let collide = std::env::var("DOMO_MOCK_REPLAN_COLLIDE")
        .map(|value| value == "1")
        .unwrap_or(false)
        && context
            .get("project")
            .and_then(|value| value.get("goal"))
            .and_then(|value| value.as_str())
            .map(|goal| goal.contains("[replan-collide]"))
            .unwrap_or(false);
    let (first_key, second_key) = if collide {
        let mut completed_keys: Vec<String> = completed.iter().cloned().collect();
        completed_keys.sort();
        let first_key = completed_keys
            .first()
            .cloned()
            .unwrap_or_else(|| next_mock_key(&completed, &HashSet::new()));
        let second_key = completed_keys
            .iter()
            .find(|key| **key != first_key)
            .cloned()
            .unwrap_or_else(|| next_mock_key(&completed, &HashSet::new()));
        (first_key, second_key)
    } else {
        let first_key = canceled
            .first()
            .cloned()
            .unwrap_or_else(|| next_mock_key(&completed, &HashSet::new()));
        let mut taken = completed.clone();
        taken.insert(first_key.clone());
        let second_key = next_mock_key(&taken, &HashSet::new());
        (first_key, second_key)
    };
    let skip = context
        .get("project")
        .and_then(|value| value.get("confirm_mode"))
        .and_then(|value| value.as_str())
        == Some(CONFIRM_AUTO);
    serde_json::json!({
        "kind": "plan",
        "summary": format!("mock 重规划：{reason}"),
        "goal": if goal.contains("[replan-goal-mismatch]") { "规划者擅自修改的目标" } else { goal },
        "skip_confirmation": skip,
        "tasks": [
            {
                "key": first_key,
                "title": "重规划后的剩余工作",
                "description": format!("在 domo_mock/{first_key}.txt 写入重规划后的内容。"),
                "acceptance": [format!("domo_mock/{first_key}.txt 存在")],
                "deps": []
            },
            {
                "key": second_key,
                "title": "重规划后的后继工作",
                "description": format!("确认 domo_mock/{first_key}.txt 存在，然后写入 domo_mock/{second_key}.txt。"),
                "acceptance": [format!("domo_mock/{first_key}.txt 与 domo_mock/{second_key}.txt 均存在")],
                "deps": [first_key]
            }
        ]
    })
}

fn next_mock_key(taken: &HashSet<String>, extra: &HashSet<String>) -> String {
    let mut index = 1;
    loop {
        let key = format!("T{index}");
        if !taken.contains(&key) && !extra.contains(&key) {
            return key;
        }
        index += 1;
    }
}

fn mock_switch_matches(context: &serde_json::Value, switch_env: &str, key: &str) -> bool {
    let enabled = std::env::var(switch_env)
        .map(|value| value == "1")
        .unwrap_or(false);
    if !enabled {
        return false;
    }
    let base = switch_env.strip_suffix("_ONCE").unwrap_or(switch_env);
    let goal = context
        .get("project")
        .and_then(|value| value.get("goal"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let matches_goal = std::env::var(format!("{base}_GOAL"))
        .ok()
        .map(|marker| goal.contains(&marker))
        .unwrap_or(true);
    let matches_key = std::env::var(format!("{base}_KEY"))
        .ok()
        .map(|marker| marker == key)
        .unwrap_or(true);
    let agent_name = context
        .get("agent")
        .and_then(|value| value.get("name"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let matches_agent = std::env::var(format!("{base}_AGENT"))
        .ok()
        .map(|marker| marker == agent_name)
        .unwrap_or(true);
    matches_goal && matches_key && matches_agent
}

/// Invalid project results exercise the bounded protocol-correction path.
fn mock_role_failure(role: &str, context: &serde_json::Value) -> Option<serde_json::Value> {
    if !matches!(
        role,
        JOB_ROLE_ANALYSIS | JOB_ROLE_DIRECT | JOB_ROLE_ACCEPTANCE | JOB_ROLE_SUMMARY
    ) {
        return None;
    }
    if role == JOB_ROLE_DIRECT
        && std::env::var("DOMO_MOCK_DIRECT_FAIL_GOAL").is_ok_and(|marker| {
            context
                .get("project")
                .and_then(|value| value.get("goal"))
                .and_then(|value| value.as_str())
                .is_some_and(|goal| goal.contains(&marker))
        })
    {
        return Some(serde_json::json!({"kind": "invalid_direct_result"}));
    }
    None
}

/// Infrastructure fixtures fail the process; valid business failure results
/// do not supply evidence about provider health.
fn mock_transport_failure(role: &str, context: &serde_json::Value) -> Option<String> {
    let (prefix, key) = match role {
        JOB_ROLE_EXECUTOR => ("DOMO_MOCK", context["task"]["key"].as_str().unwrap_or("T1")),
        JOB_ROLE_ANALYSIS | JOB_ROLE_DIRECT | JOB_ROLE_ACCEPTANCE | JOB_ROLE_SUMMARY => {
            ("DOMO_MOCK_PLANNER", "")
        }
        _ => return None,
    };
    if mock_flag_matches(context, &format!("{prefix}_RATE_LIMIT"), key) {
        let secs = std::env::var(format!("{prefix}_RATE_LIMIT_RETRY"))
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(900);
        return Some(format!(
            "HTTP 429 too many requests; retry after {secs} seconds"
        ));
    }
    if mock_flag_matches(context, &format!("{prefix}_TRANSIENT"), key) {
        return Some("provider temporarily unavailable (503)".into());
    }
    if mock_flag_matches(context, &format!("{prefix}_PERMANENT"), key) {
        return Some("永久错误：Agent 配置无效".into());
    }
    None
}

fn mock_flag_matches(context: &serde_json::Value, flag_env: &str, key: &str) -> bool {
    let enabled = std::env::var(flag_env)
        .map(|value| value == "1")
        .unwrap_or(false);
    if !enabled {
        return false;
    }
    let goal = context
        .get("project")
        .and_then(|value| value.get("goal"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let matches_goal = std::env::var(format!("{flag_env}_GOAL"))
        .ok()
        .map(|marker| goal.contains(&marker))
        .unwrap_or(true);
    let matches_key = std::env::var(format!("{flag_env}_KEY"))
        .ok()
        .map(|marker| marker == key)
        .unwrap_or(true);
    let agent_name = context
        .get("agent")
        .and_then(|value| value.get("name"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let matches_agent = std::env::var(format!("{flag_env}_AGENT"))
        .ok()
        .map(|marker| marker == agent_name)
        .unwrap_or(true);
    matches_goal && matches_key && matches_agent
}

fn take_once_marker(context: &serde_json::Value, cwd: &Path, name: &str) -> Result<bool> {
    let job_dir = context
        .get("job_dir")
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_path_buf());
    let marker = job_dir
        .parent()
        .map(|parent| parent.join(name))
        .unwrap_or_else(|| job_dir.join(name));
    if marker.exists() {
        return Ok(false);
    }
    std::fs::write(&marker, b"1")?;
    Ok(true)
}

fn mock_execute(context: &serde_json::Value, cwd: &Path) -> Result<serde_json::Value> {
    let task = context.get("task").cloned().unwrap_or_default();
    let key = task.get("key").and_then(|v| v.as_str()).unwrap_or("T1");
    let fail_once = std::env::var("DOMO_MOCK_FAIL_ONCE")
        .map(|value| value == "1")
        .unwrap_or(false);
    if fail_once && key == "T1" {
        let job_dir = context
            .get("job_dir")
            .and_then(|value| value.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.to_path_buf());
        let marker = job_dir
            .parent()
            .map(|parent| parent.join(".mock-fail-once"))
            .unwrap_or_else(|| job_dir.join(".mock-fail-once"));
        if !marker.exists() {
            std::fs::write(&marker, b"1")?;
            return Ok(serde_json::json!({
                "kind": "failed",
                "reason": "mock 首次执行失败"
            }));
        }
    }
    if mock_switch_matches(context, "DOMO_MOCK_FAIL_ALWAYS", key) {
        return Ok(serde_json::json!({
            "kind": "failed",
            "reason": "mock 执行者持续失败"
        }));
    }
    let replan_once = std::env::var("DOMO_MOCK_REPLAN_ONCE")
        .map(|value| value == "1")
        .unwrap_or(false);
    if replan_once {
        let goal = context
            .get("project")
            .and_then(|value| value.get("goal"))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let matches_goal = std::env::var("DOMO_MOCK_REPLAN_GOAL")
            .ok()
            .map(|marker| goal.contains(&marker))
            .unwrap_or(true);
        let matches_key = std::env::var("DOMO_MOCK_REPLAN_KEY")
            .ok()
            .map(|marker| marker == key)
            .unwrap_or(true);
        if matches_goal && matches_key {
            let job_dir = context
                .get("job_dir")
                .and_then(|value| value.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| cwd.to_path_buf());
            let marker = job_dir
                .parent()
                .map(|parent| parent.join(".mock-replan-once"))
                .unwrap_or_else(|| job_dir.join(".mock-replan-once"));
            if !marker.exists() {
                std::fs::write(&marker, b"1")?;
                return Ok(serde_json::json!({
                    "kind": "replan",
                    "reason": "mock 执行者发现原计划与仓库现状冲突，请求重新规划"
                }));
            }
        }
    }
    if mock_switch_matches(context, "DOMO_MOCK_DERIVE_ONCE", key)
        && take_once_marker(context, cwd, ".mock-derive-once")?
    {
        return Ok(serde_json::json!({
            "kind": "derive",
            "summary": "mock 执行者拆分任务，依赖关系由测试场景指定",
            "tasks": [
                {
                    "key": "T1.1",
                    "title": "并行子任务一",
                    "description": "在 domo_mock/T1.1.txt 写入派生任务一的内容。",
                    "acceptance": ["domo_mock/T1.1.txt 存在且包含 T1.1"],
                    "deps": []
                },
                {
                    "key": "T1.2",
                    "title": "并行子任务二",
                    "description": "在 domo_mock/T1.2.txt 写入派生任务二的内容。",
                    "acceptance": ["domo_mock/T1.2.txt 存在且包含 T1.2"],
                    "deps": if std::env::var("DOMO_MOCK_DERIVE_DEPENDENCIES").as_deref() == Ok("1") { vec!["T1.1"] } else { vec![] }
                }
            ]
        }));
    }
    if mock_switch_matches(context, "DOMO_MOCK_INVALID_DERIVE_ONCE", key)
        && take_once_marker(context, cwd, ".mock-invalid-derive-once")?
    {
        return Ok(serde_json::json!({
            "kind": "derive",
            "summary": "mock 非法派生请求",
            "tasks": [
                {
                    "key": "T1.9",
                    "title": "非法子任务",
                    "description": "派生任务不允许依赖父任务。",
                    "acceptance": ["非法请求应被拒绝"],
                    "deps": ["T1"]
                }
            ]
        }));
    }
    let deps: Vec<String> = task
        .get("deps")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let dir = cwd.join("domo_mock");
    std::fs::create_dir_all(&dir)?;
    for dep in &deps {
        if !dir.join(format!("{dep}.txt")).is_file() {
            return Ok(serde_json::json!({
                "kind": "failed",
                "reason": format!("前置任务 {dep} 的产物不存在")
            }));
        }
    }
    let mut content = format!("mock task {key}\n");
    for dep in &deps {
        content.push_str(&format!("dep {dep} was merged\n"));
    }
    std::fs::write(dir.join(format!("{key}.txt")), content)?;
    Ok(serde_json::json!({
        "kind": "done",
        "summary": format!("已写入 domo_mock/{key}.txt"),
        "details": "mock executor"
    }))
}

fn mock_verify(context: &serde_json::Value, cwd: &Path) -> Result<serde_json::Value> {
    let task = context.get("task").cloned().unwrap_or_default();
    let key = task.get("key").and_then(|v| v.as_str()).unwrap_or("T1");
    let reject_once = std::env::var("DOMO_MOCK_REJECT_ONCE")
        .map(|value| value == "1")
        .unwrap_or(false);
    if reject_once && key == "T1" {
        let job_dir = context
            .get("job_dir")
            .and_then(|value| value.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.to_path_buf());
        let marker = job_dir
            .parent()
            .map(|parent| parent.join(".mock-reject-once"))
            .unwrap_or_else(|| job_dir.join(".mock-reject-once"));
        if !marker.exists() {
            std::fs::write(&marker, b"1")?;
            return Ok(serde_json::json!({
                "pass": false,
                "reason": "mock 首次验收拒绝",
                "issues": ["请确认产物内容后再次提交"]
            }));
        }
    }
    let deps: Vec<String> = task
        .get("deps")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut missing = Vec::new();
    for name in std::iter::once(key.to_string()).chain(deps) {
        if !cwd.join("domo_mock").join(format!("{name}.txt")).is_file() {
            missing.push(format!("domo_mock/{name}.txt 不存在"));
        }
    }
    if missing.is_empty() {
        Ok(serde_json::json!({"pass": true, "reason": "mock 产物齐全", "issues": []}))
    } else {
        Ok(serde_json::json!({"pass": false, "reason": "mock 产物缺失", "issues": missing}))
    }
}

fn mock_project_acceptance(context: &serde_json::Value, cwd: &Path) -> Result<serde_json::Value> {
    let criteria: Vec<String> = context
        .get("acceptance_criteria")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let criteria = if criteria.is_empty() {
        vec!["项目目标已达成".to_string()]
    } else {
        criteria
    };
    let always_fail = std::env::var("DOMO_MOCK_ACCEPT_ALWAYS_FAIL")
        .map(|value| value == "1")
        .unwrap_or(false);
    let human_only = std::env::var("DOMO_MOCK_ACCEPT_HUMAN_ONLY")
        .map(|value| value == "1")
        .unwrap_or(false);
    let fail_once = std::env::var("DOMO_MOCK_ACCEPT_FAIL_ONCE")
        .map(|value| value == "1")
        .unwrap_or(false)
        && take_once_marker(context, cwd, ".mock-accept-fail-once")?;
    let fail = !human_only && (always_fail || fail_once);
    let items = criteria
        .iter()
        .enumerate()
        .map(|(index, criterion)| {
            let failed = fail && index == 0;
            let verdict = if failed {
                "unmet"
            } else if human_only {
                "human_required"
            } else {
                "met"
            };
            serde_json::json!({
                "id": format!("A{}", index + 1),
                "source": "proposal_acceptance",
                "criterion": criterion,
                "verdict": verdict,
                "evidence": if failed {
                    serde_json::json!([])
                } else if human_only {
                    serde_json::json!(["human: 需要人工确认"])
                } else {
                    serde_json::json!(["mock: 项目交付分支产物已核查"])
                },
                "gap": if failed {
                    "mock 项目验收未通过"
                } else if human_only {
                    "mock 需要人工确认"
                } else {
                    ""
                },
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "kind": "project_acceptance",
        "summary": if fail { "mock 项目验收未通过" } else { "mock 项目验收通过" },
        "criteria": items,
        "replan_reason": if fail { "mock 项目验收发现未满足条目" } else { "" },
    }))
}

fn mock_summary(context: &serde_json::Value) -> serde_json::Value {
    let opinions: Vec<String> = context
        .get("messages")
        .and_then(|value| value.as_array())
        .map(|messages| {
            messages
                .iter()
                .filter(|message| {
                    message.get("role").and_then(|value| value.as_str())
                        == Some(crate::models::REPORT_MESSAGE_ROLE)
                })
                .filter_map(|message| {
                    message
                        .get("content")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    let revision = if opinions.is_empty() {
        String::new()
    } else {
        format!(
            "## 依据人工意见修订\n{}\n\n",
            opinions
                .iter()
                .enumerate()
                .map(|(index, item)| format!("- 已回应第 {} 条意见：{item}", index + 1))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let report = format!(
        "# Mock 项目报告\n\n所有 mock 任务已完成并通过验收，产物位于 `domo_mock/`。\n\n{revision}"
    );
    serde_json::json!({ "kind": "report", "report": report })
}

/// Mock agent. It writes result JSON in-process and never spawns a CLI.
#[derive(Debug, Default)]
pub struct MockAdapter;

impl AgentAdapter for MockAdapter {
    const KIND: &'static str = crate::models::AGENT_MOCK;

    fn requires_command_template(_config: &AgentConfig) -> bool {
        false
    }

    fn validate(_config: &AgentConfig) -> Result<()> {
        Ok(())
    }

    fn session_for_job(_job_id: &str, _existing: Option<&str>) -> Option<String> {
        None
    }

    async fn run_turn(&self, context: TurnContext<'_>) -> Result<AgentRun> {
        run_mock(context.spec).await
    }
}
