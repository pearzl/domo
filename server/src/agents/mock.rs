use super::AdapterCapabilities;
use crate::{db, events, models::{Agent, Task}, state::App};
use serde_json::json;
use std::sync::Arc;

pub struct MockAdapter;

impl AdapterCapabilities for MockAdapter {
    fn name(&self) -> &'static str { "mock" }
    fn supports_resume(&self) -> bool { false }
    fn supports_spawn(&self) -> bool { true }
    fn supports_send(&self) -> bool { true }
    fn supports_observe(&self) -> bool { true }
}

fn duration_of(agent: &Agent) -> u64 {
    agent
        .config
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(4000)
}

/// Execute one attempt. Runs as a worker task; only sleeps, writes streamed logs,
/// produces an artifact on success. Failure is simulated by a `[fail]` tag in the
/// task description.
///
/// M4 三档恢复：mock 适配器不声明 `supports_resume`（capabilities 由 agent 档案给出，
/// 默认 false）→ 第一档跳过、走第二档交接重建；mock 的「会话」即任务-尝试本身，
/// 交接模式只做日志留痕后续作同样逻辑。
pub async fn run_execution(
    state: Arc<App>,
    task: Task,
    agent: Agent,
    mode: super::ExecMode,
) -> Result<super::ExecOutcome, String> {
    // 测试钩子：模拟供应商限流（验证熔断 + 换下一优先级 agent）
    if task.description.contains("[rate_limit]") {
        let conn = state.db.lock().unwrap();
        events::log_line(&state, &conn, &task.id, "[rate_limit] mock 模拟限流错误");
        return Err("opencode run failed (exit exit status: 1); Rate limit exceeded. Please try again later.".into());
    }
    {
        let conn = state.db.lock().unwrap();
        let how = match &mode {
            super::ExecMode::Fresh => String::new(),
            super::ExecMode::Resume { real_id } => format!("（三档恢复·原生 resume {real_id}）"),
            super::ExecMode::Handoff { session, .. } => format!("（三档恢复·交接重建，新会话 {session}）"),
        };
        events::log_line(
            &state,
            &conn,
            &task.id,
            &format!("mock executor '{}' started attempt {} for task '{}' (adapter=mock){how}", agent.name, task.attempt + 1, task.title),
        );
        if let Some(g) = db::latest_guidance(&conn, &task.id) {
            events::log_line(&state, &conn, &task.id, &format!("carrying human guidance: {g}"));
        }
    }
    // M5：与 opencode 同源组装执行提示词并落审计（知识注入可被断言）。
    {
        let c = state.db.lock().unwrap();
        let project = db::get_project(&c, &task.project_id);
        let guidance = db::latest_guidance(&c, &task.id);
        drop(c);
        let _prompt = crate::knowledge::assemble(
            &state, &task.project_id, Some(&task.id), "executor", "execute",
            |k| super::opencode::exec_prompt(project.as_ref(), &task, guidance.as_deref(), k),
        );
    }
    let dur = duration_of(&agent);
    tokio::time::sleep(std::time::Duration::from_millis(dur)).await;
    if task.description.contains("[fail]") {
        {
            let conn = state.db.lock().unwrap();
            events::log_line(&state, &conn, &task.id, &format!("[fail] mock execution FAILED for task '{}' (simulated, {}ms)", task.title, dur));
        }
        return Err("[fail] simulated execution failure".into());
    }
    let artifact = format!("artifact://{}/a{}.txt", task.id, chrono_like());
    {
        let conn = state.db.lock().unwrap();
        events::log_line(&state, &conn, &task.id, &format!("mock execution succeeded for task '{}' ({}ms), artifact: {}", task.title, dur, artifact));
    }
    Ok(super::ExecOutcome { artifacts: vec![artifact], session_id: None })
}

fn chrono_like() -> String {
    db::now().to_string()
}

/// M5：mock planner 的确定性规划产出（无 opencode planner 时兜底，供本地/冒烟端到端走通）。
/// 产出 2 个相互依赖的任务（第二个依赖第一个），均需验收、含可检验验收标准。
pub fn plan_json(project: &crate::models::Project) -> String {
    serde_json::json!({
        "tasks": [
            {
                "title": "梳理需求与约束",
                "description": format!("为项目「{}」梳理目标、约束与验收口径", project.name),
                "depends_on": [],
                "acceptance_required": true,
                "acceptance_criteria": "产出需求要点清单，覆盖项目简介中的全部目标"
            },
            {
                "title": "交付最小可用产物",
                "description": "基于需求要点产出可验收的最小可用交付物",
                "depends_on": [0],
                "acceptance_required": true,
                "acceptance_criteria": "产物可独立运行/使用，并通过验收者复核"
            }
        ],
        "milestones": []
    })
    .to_string()
}

/// Run acceptance. Rejects when the task description contains `[reject]`,
/// otherwise approves after 2 seconds.
/// M4 计划阶段（mock）：产出模拟任务计划；`[fail_plan]` 触发计划制定失败。
pub async fn run_task_plan(
    state: Arc<App>,
    task: Task,
    agent: Agent,
    review_feedback: Option<&str>,
) -> Result<String, String> {
    {
        let conn = state.db.lock().unwrap();
        events::log_line(
            &state, &conn, &task.id,
            &format!("mock executor '{}' 制定任务计划（adapter=mock）{}", agent.name,
                     review_feedback.map(|f| format!("，按打回意见修订：{f}")).unwrap_or_default()),
        );
    }
    // M5：mock 与 opencode 同源组装提示词并落 `prompt.assembled` 审计（含知识注入）。
    {
        let c = state.db.lock().unwrap();
        let project = db::get_project(&c, &task.project_id);
        let guidance = db::latest_guidance(&c, &task.id);
        drop(c);
        let _prompt = crate::knowledge::assemble(
            &state, &task.project_id, Some(&task.id), "executor", "plan_drafting",
            |k| super::opencode::task_plan_prompt(
                project.as_ref(),
                &task,
                guidance.as_deref(),
                task.execution_plan.as_deref(),
                review_feedback,
                k,
            ),
        );
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    if task.description.contains("[fail_plan]") {
        return Err("[fail_plan] simulated plan drafting failure".into());
    }
    Ok(format!(
        "## 任务计划（mock）\n\n1. 读取任务描述与验收标准\n2. 产出交付物（模拟）\n3. 自检并报告\n\n验收对齐：{}",
        task.acceptance_criteria.as_deref().unwrap_or("(未指定)")
    ))
}

/// M4 计划阶段（mock）：审查计划；`[reject_plan]` 触发打回（可带轮次控制：
/// `[reject_plan_always]` 每轮都拒）。
pub async fn run_plan_review(state: Arc<App>, task: Task, agent: Agent) -> Result<(), String> {
    {
        let conn = state.db.lock().unwrap();
        events::log_line(
            &state, &conn, &task.id,
            &format!("mock acceptor '{}' 审查任务计划（adapter=mock）", agent.name),
        );
    }
    // M5：与 opencode 同源组装提示词并落审计（知识注入可被断言）。
    {
        let c = state.db.lock().unwrap();
        let Some(p) = db::get_project(&c, &task.project_id) else {
            return Ok(());
        };
        drop(c);
        let _prompt = crate::knowledge::assemble(
            &state, &task.project_id, Some(&task.id), "acceptor", "plan_review",
            |k| super::opencode::plan_review_prompt(
                &p,
                &task,
                task.execution_plan.as_deref().unwrap_or("(计划缺失)"),
                k,
            ),
        );
    }
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    if task.description.contains("[reject_plan_always]")
        || (task.description.contains("[reject_plan]") && task.plan_rejection_count == 0)
    {
        return Err("[reject_plan] mock 计划审查打回：请补充验证方式与产物清单（模拟）".into());
    }
    Ok(())
}

pub async fn run_acceptance(state: Arc<App>, task: Task, agent: Agent) -> Result<(), String> {
    {
        let conn = state.db.lock().unwrap();
        events::log_line(
            &state,
            &conn,
            &task.id,
            &format!("mock acceptor '{}' started reviewing artifact of task '{}'", agent.name, task.title),
        );
    }
    // M5：与 opencode 同源组装验收提示词并落审计（知识注入可被断言）。
    {
        let c = state.db.lock().unwrap();
        let project = db::get_project(&c, &task.project_id);
        let logs = db::task_logs(&c, &task.id);
        drop(c);
        if let Some(p) = project {
            let log_tail: String = logs
                .iter()
                .rev()
                .take(20)
                .map(|l| l.line.clone())
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            let _prompt = crate::knowledge::assemble(
                &state, &task.project_id, Some(&task.id), "acceptor", "accept",
                |k| super::opencode::accept_prompt(&p, &task, &log_tail, k),
            );
        }
    }
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    {
        let conn = state.db.lock().unwrap();
        if task.description.contains("[reject]") {
            events::log_line(&state, &conn, &task.id, &format!("[reject] mock acceptance REJECTED task '{}' (simulated)", task.title));
        }
    }
    if task.description.contains("[reject]") {
        return Err("[reject] simulated acceptance rejection".into());
    }
    {
        let conn = state.db.lock().unwrap();
        events::log_line(&state, &conn, &task.id, &format!("mock acceptance PASSED task '{}' (2s review)", task.title));
    }
    Ok(())
}

pub async fn run_adjudication(
    state: Arc<App>,
    _task: Option<Task>,
    decision: crate::models::Decision,
    agent: Agent,
) -> Result<crate::decision::Ruling, String> {
    {
        let c = state.db.lock().unwrap();
        let tid = decision.task_id.as_deref().unwrap_or("-");
        events::log_line(
            &state,
            &c,
            tid,
            &format!(
                "mock 决策者 '{}' 开始裁定决策请求 {}（kind={}）",
                agent.name, decision.id, decision.kind
            ),
        );
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    match decision.kind.as_str() {
        "exemption" => Ok(crate::decision::Ruling {
            approve: true,
            rationale: "简单任务，豁免验收更有效率".into(),
            criteria: None,
            action: None,
        }),
        "negotiation_failed" => Ok(crate::decision::Ruling {
            approve: true,
            rationale: "协商未达成，采用最简可检验标准推进".into(),
            criteria: Some("按任务说明及产出是否满足目标判定".into()),
            action: None,
        }),
        "change_review" => Ok(crate::decision::Ruling {
            approve: true,
            rationale: "任务级调整不触及里程碑/项目目标，予以批准".into(),
            criteria: None,
            action: None,
        }),
        // M4 合并冲突：mock 决策者选择 rebase 让系统重试
        "merge_conflict" => Ok(crate::decision::Ruling {
            approve: true,
            rationale: "采用 rebase 后重试合并".into(),
            criteria: None,
            action: Some("rebase".into()),
        }),
        // 重规划（常规纠偏）：reason 含 `[reject]` 时否决，便于 e2e 覆盖拒绝分支
        "replan" => {
            let reason = decision.request["reason"].as_str().unwrap_or("");
            if reason.contains("[reject]") {
                Ok(crate::decision::Ruling {
                    approve: false,
                    rationale: "上报的规划问题不足以支撑调整，维持原规划".into(),
                    criteria: None,
                    action: None,
                })
            } else {
                Ok(crate::decision::Ruling {
                    approve: true,
                    rationale: "规划问题成立且不触及里程碑/项目目标，批准重规划".into(),
                    criteria: None,
                    action: None,
                })
            }
        }
        _ => Ok(crate::decision::Ruling {
            approve: false,
            rationale: "未知决策类型，谨慎否决".into(),
            criteria: None,
            action: None,
        }),
    }
}

/// 每轮协商的模拟耗时（与 duration_ms 无关，保持协商快速且可预期）。
const NEGOTIATE_STEP_MS: u64 = 150;

/// M3 协商：executor 提出/修订验收标准。第 1 轮以任务既有标准（planer 草案）为基础，
/// 之后携带 acceptor 的历史意见修订，模拟「多轮往复」。
#[allow(dead_code)] // M4 协商退场
pub async fn negotiate_propose(
    state: Arc<App>,
    task: Task,
    agent: Agent,
    round: i64,
    feedback: Option<&str>,
) -> Result<String, String> {
    {
        let c = state.db.lock().unwrap();
        events::log_line(
            &state,
            &c,
            &task.id,
            &format!(
                "mock executor '{}' 提出第 {} 轮验收标准{}",
                agent.name,
                round,
                feedback.map(|f| format!("（携带验收者上轮意见：{f}）")).unwrap_or_default()
            ),
        );
    }
    tokio::time::sleep(std::time::Duration::from_millis(NEGOTIATE_STEP_MS)).await;
    let base = task
        .acceptance_criteria
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| crate::api::default_acceptance_criteria(&task.title, &task.description));
    if round <= 1 {
        Ok(base)
    } else {
        let fb = feedback.unwrap_or("补充可检验的量化指标");
        Ok(format!("{base}（第 {round} 轮修订：{fb}）"))
    }
}

/// M3 协商：acceptor 审核 executor 的提案。
/// - 任务描述含 `[negotiate-reject]` → 拒绝任何标准（对抗用例，必然协商失败）；
/// - 含 `[negotiate-round2]` → 第 1 轮反对、第 2 轮起同意（演示多轮）；
/// - 其余 → 首轮即一致通过。
#[allow(dead_code)] // M4 协商退场
pub async fn negotiate_review(
    state: Arc<App>,
    task: Task,
    agent: Agent,
    proposal: &str,
) -> Result<super::Review, String> {
    {
        let c = state.db.lock().unwrap();
        events::log_line(
            &state,
            &c,
            &task.id,
            &format!("mock acceptor '{}' 审核提案：{}", agent.name, proposal),
        );
    }
    tokio::time::sleep(std::time::Duration::from_millis(NEGOTIATE_STEP_MS)).await;
    let desc = task.description.as_str();
    if desc.contains("[negotiate-reject]") {
        Ok(super::Review {
            agree: false,
            comment: "（对抗用例）拒绝任何验收标准".into(),
            criteria: None,
        })
    } else if desc.contains("[negotiate-round2]") && !proposal.contains("第 2 轮修订") {
        Ok(super::Review {
            agree: false,
            comment: "请补充可检验的量化指标".into(),
            criteria: None,
        })
    } else {
        Ok(super::Review {
            agree: true,
            comment: "标准可检验，一致通过".into(),
            criteria: Some(proposal.to_string()),
        })
    }
}

/// Static capability snapshot for the agent payload (used by capabilities fields).
#[allow(dead_code)]
pub fn capability_info() -> serde_json::Value {
    json!({
        "adapter": "mock",
        "required": ["spawn", "send", "observe", "session_id"],
        "supports_resume": false
    })
}

/// M3 项目总结报告：mock planner 的叙述产出。
///
/// 模拟浏览器/CI 下可复现的失败路径：agent config 里 `report_fail=true` 时返回
/// Err（对应「planner 故障 → domo.log 记录错误 + 可手工重试」），修好 config 后
/// 重试即成功。其余情况返回一段基于事实快照的确定性叙述。
pub async fn report_narrative(
    project: &crate::models::Project,
    facts: &serde_json::Value,
    agent: &Agent,
) -> Result<String, String> {
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    if agent
        .config
        .get("report_fail")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err(format!(
            "mock planner '{}' 报告生成失败（config.report_fail=true 模拟故障）",
            agent.name
        ));
    }
    let counts = &facts["counts"];
    let total = counts["total"].as_i64().unwrap_or(0);
    let done = counts["done"].as_i64().unwrap_or(0);
    let cancelled = counts["cancelled"].as_i64().unwrap_or(0);
    let milestones = facts["milestones"].as_array().cloned().unwrap_or_default();
    let mut s = format!(
        "项目「{}」已完成交付：共 {} 个任务，完成 {} 个、取消 {} 个，横跨 {} 个里程碑。\n\n",
        project.name,
        total,
        done,
        cancelled,
        milestones.len()
    );
    if milestones.is_empty() {
        s.push_str("本项目未设置里程碑，交付全程由调度器自动推进，无人工 gate 介入。\n\n");
    } else {
        s.push_str("里程碑汇报：\n");
        for m in &milestones {
            let members = m["members"].as_array().map(|a| a.len()).unwrap_or(0);
            let member_done = m["members"]
                .as_array()
                .map(|a| a.iter().filter(|t| t["status"] == "done").count())
                .unwrap_or(0);
            s.push_str(&format!(
                "- {}（{}）：目标「{}」，{} 个成员任务完成 {}/{}。{}\n",
                m["name"].as_str().unwrap_or("里程碑"),
                m["status"].as_str().unwrap_or(""),
                m["goal"].as_str().unwrap_or("未填写"),
                members,
                member_done,
                members,
                m["gate_result"].as_str().unwrap_or(""),
            ));
        }
        s.push('\n');
    }
    let escalations = facts["escalations"].as_array().map(|a| a.len()).unwrap_or(0);
    let rejections = facts["rejections"].as_array().map(|a| a.len()).unwrap_or(0);
    s.push_str(&format!(
        "风险与纠偏：执行失败/挂起 {} 次，验收退回 {} 次，均由收件箱裁决或自动重规划闭环处理。",
        escalations, rejections
    ));
    Ok(s)
}
