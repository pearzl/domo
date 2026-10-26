//! Agent adapters. A job is one process (or one in-process mock) that changes the
//! workspace and writes a result JSON file.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::models::{
    AgentRow, AGENT_CLI, AGENT_MOCK, AGENT_OPENCODE, JOB_ROLE_EXECUTOR, JOB_ROLE_GOAL,
    JOB_ROLE_SUMMARY, JOB_ROLE_VERIFIER,
};

const OUTPUT_CAP: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct AgentRun {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i64>,
    pub timed_out: bool,
}

#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub job_id: String,
    pub role: String,
    pub cwd: PathBuf,
    pub job_dir: PathBuf,
    pub prompt: String,
}

pub async fn execute(agent: &AgentRow, spec: &AgentSpec, timeout: Duration) -> Result<AgentRun> {
    match agent.kind.as_str() {
        AGENT_MOCK => run_mock(spec).await,
        AGENT_OPENCODE => run_opencode(agent, spec, timeout).await,
        AGENT_CLI => run_cli(agent, spec, timeout).await,
        other => Err(anyhow!("未知 Agent 类型: {other}")),
    }
}

async fn run_mock(spec: &AgentSpec) -> Result<AgentRun> {
    let delay_ms = std::env::var("DOMO_MOCK_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(120);
    let delay_ms = if spec.role == JOB_ROLE_EXECUTOR {
        std::env::var("DOMO_MOCK_EXECUTOR_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(delay_ms)
    } else {
        delay_ms
    };
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;

    let context = read_context(&spec.job_dir)?;
    let result = match spec.role.as_str() {
        JOB_ROLE_GOAL => mock_plan(&context),
        JOB_ROLE_EXECUTOR => mock_execute(&context, &spec.cwd)?,
        JOB_ROLE_VERIFIER => mock_verify(&context, &spec.cwd)?,
        JOB_ROLE_SUMMARY => mock_summary(),
        other => return Err(anyhow!("mock 不支持角色 {other}")),
    };
    let path = spec.job_dir.join("result.json");
    tokio::fs::write(&path, serde_json::to_vec_pretty(&result)?).await?;
    Ok(AgentRun {
        stdout: format!("mock job {} finished\n", spec.job_id),
        stderr: String::new(),
        exit_code: Some(0),
        timed_out: false,
    })
}

fn read_context(job_dir: &Path) -> Result<serde_json::Value> {
    let raw = std::fs::read_to_string(job_dir.join("context.json"))
        .with_context(|| format!("读取 {} 失败", job_dir.display()))?;
    Ok(serde_json::from_str(&raw)?)
}

fn mock_plan(context: &serde_json::Value) -> serde_json::Value {
    let goal = context
        .get("project")
        .and_then(|v| v.get("goal"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let has_planner_message = context
        .get("messages")
        .and_then(|v| v.as_array())
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
    serde_json::json!({
        "kind": "plan",
        "summary": "mock 规划：两个有依赖的实现任务",
        "tasks": [
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
        ]
    })
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

fn mock_summary() -> serde_json::Value {
    serde_json::json!({
        "kind": "report",
        "report": "# Mock 项目报告\n\n所有 mock 任务已完成并通过验收，产物位于 `domo_mock/`。\n"
    })
}

async fn run_opencode(agent: &AgentRow, spec: &AgentSpec, timeout: Duration) -> Result<AgentRun> {
    let bin = std::env::var("DOMO_OPENCODE_BIN").unwrap_or_else(|_| "opencode".into());
    let mut args = vec![
        "run".to_string(),
        "--auto".into(),
        "--format".into(),
        "json".into(),
    ];
    if !agent.model.trim().is_empty() {
        args.push("--model".into());
        args.push(agent.model.trim().to_string());
    }
    args.push(spec.prompt.clone());
    run_process(&bin, &args, spec, timeout, None).await
}

async fn run_cli(agent: &AgentRow, spec: &AgentSpec, timeout: Duration) -> Result<AgentRun> {
    let command: Vec<String> = agent
        .command_json
        .as_deref()
        .map(|raw| serde_json::from_str::<Vec<String>>(raw).unwrap_or_default())
        .unwrap_or_default();
    if command.is_empty() {
        return Err(anyhow!("CLI Agent 未配置命令模板"));
    }
    let model_args: Vec<String> = agent
        .model_args_json
        .as_deref()
        .map(|raw| serde_json::from_str::<Vec<String>>(raw).unwrap_or_default())
        .unwrap_or_default();
    let model = agent.model.trim();
    let mut expanded_model_args = Vec::new();
    if !model.is_empty() {
        expanded_model_args = model_args
            .iter()
            .map(|arg| arg.replace("{model}", model))
            .collect();
    }
    let mut args = Vec::new();
    let mut has_prompt = false;
    for arg in &command[1..] {
        match arg.as_str() {
            "{prompt}" => {
                args.push(spec.prompt.clone());
                has_prompt = true;
            }
            "{job_dir}" => args.push(spec.job_dir.to_string_lossy().to_string()),
            "{cwd}" => args.push(spec.cwd.to_string_lossy().to_string()),
            "{model}" => {
                if !model.is_empty() {
                    args.push(model.to_string());
                }
            }
            "{model_args}" => args.extend(expanded_model_args.iter().cloned()),
            other => args.push(other.replace("{model}", model)),
        }
    }
    if !model.is_empty()
        && !command
            .iter()
            .any(|arg| arg.contains("{model}") || arg == "{model_args}")
    {
        args.extend(expanded_model_args);
    }
    let stdin = if has_prompt {
        None
    } else {
        Some(spec.prompt.as_bytes().to_vec())
    };
    run_process(&command[0], &args, spec, timeout, stdin).await
}

async fn run_process(
    program: &str,
    args: &[String],
    spec: &AgentSpec,
    timeout: Duration,
    stdin: Option<Vec<u8>>,
) -> Result<AgentRun> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&spec.cwd)
        .env("DOMO_JOB_ID", &spec.job_id)
        .env("DOMO_JOB_ROLE", &spec.role)
        .env("DOMO_JOB_DIR", &spec.job_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("启动 Agent 进程失败: {program}"))?;
    let pid = child.id();
    if let Some(pid) = pid {
        let start = process_start_time(pid).unwrap_or_default();
        let _ = std::fs::write(spec.job_dir.join("pid"), format!("{pid}:{start}"));
    }
    if let Some(input) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            tokio::spawn(async move {
                let _ = pipe.write_all(&input).await;
                let _ = pipe.shutdown().await;
            });
        }
    }
    let stdout = child.stdout.take().expect("stdout pipe");
    let stderr = child.stderr.take().expect("stderr pipe");
    let stdout_task = tokio::spawn(read_capped(stdout));
    let stderr_task = tokio::spawn(read_capped(stderr));

    let mut timed_out = false;
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => result?,
        Err(_) => {
            timed_out = true;
            if let Some(pid) = pid {
                kill_process_group(pid);
            }
            let _ = child.kill().await;
            child.wait().await?
        }
    };
    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();
    Ok(AgentRun {
        stdout,
        stderr,
        exit_code: status.code().map(|code| code as i64),
        timed_out,
    })
}

async fn read_capped<R: AsyncRead + Unpin>(mut reader: R) -> String {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => {
                if output.len() < OUTPUT_CAP {
                    let take = n.min(OUTPUT_CAP - output.len());
                    output.extend_from_slice(&buffer[..take]);
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&output).to_string()
}

pub fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
        libc::kill(pid as i32, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

pub fn kill_job_dir(job_dir: &Path) {
    let path = job_dir.join("pid");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let (pid_text, recorded_start) = match raw.trim().split_once(':') {
        Some((pid, start)) => (pid, Some(start)),
        None => (raw.trim(), None),
    };
    let Ok(pid) = pid_text.parse::<u32>() else {
        return;
    };
    if let Some(recorded) = recorded_start {
        if !recorded.is_empty() && process_start_time(pid).as_deref() != Some(recorded) {
            return;
        }
    }
    kill_process_group(pid);
}

fn process_start_time(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The executable name may contain spaces; fields after the closing parenthesis
    // are stable. Start time is field 22 overall, index 19 after the state field.
    let rest = stat.rsplit_once(") ").map(|(_, rest)| rest)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields.get(19).map(|value| value.to_string())
}
