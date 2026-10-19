//! ⚠️ 已废弃（随重构移除）：开工前"验收标准协商"已取消，验收标准由规划产出、
//! 人工确认后锁定。不要在此扩展功能；触碰本文件时的方向是删除或替换。
//!
//! ---
//!
//! M3 验收标准协商（DESIGN §7.1）：任务在 PLANNED 阶段由 executor 与 acceptor
//! agent 就验收标准进行多轮协商。
//!
//! - **一致即放行**：某轮 acceptor 同意（或给出最终标准）后写入 `acceptance_criteria`
//!   且 `negotiation_status=agreed`；调度器随后的 `planned_gate` 才允许任务进入 READY。
//! - **多轮不一致 / 超轮次**：达到 `MAX_NEGOTIATION_ROUNDS` 仍不一致 →
//!   `negotiation_status=failed`，自动登记 `negotiation_failed` 决策请求交决策者裁决。
//! - **不派发执行**：协商未达成（pending/failed）时任务停留在 planned，闸门保证不被派发。
//! - **留痕与可见**：每轮写入 `negotiation.round` 事件（role=executor|acceptor，
//!   提案 / 同意与否 / 意见），任务详情时间线可完整回放；`negotiation.started/agreed/failed`
//!   标记过程边界。
//! - **跨重启续跑**：已完成轮次持久化于 `tasks.negotiation_round`，协商历史从事件流重建，
//!   进程重启后从下一轮继续而不是从头再来。
//! - **会话上下文复用**：每轮携带任务上下文与历史轮次（含上轮 acceptor 意见），
//!   opencode 适配器为 executor/acceptor 各自复用同一协商会话（memory 连续）。

use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{
    agents::{mock, opencode},
    db, decision, events,
    models::{Agent, Task},
    state::{App, HandleGuard},
};

/// 单任务最大协商轮次：超过仍不一致 → 协商失败，交决策者裁决。
pub const MAX_NEGOTIATION_ROUNDS: i64 = 3;

/// 从事件流重建某任务的协商轮次历史（跨重启，任务详情据此展示）。
pub fn history(conn: &Connection, task_id: &str) -> Vec<Value> {
    db::list_events(conn, None, Some(task_id))
        .into_iter()
        .filter(|e| e.kind == "negotiation.round")
        .map(|e| {
            let mut p = e.payload.clone();
            if let Some(obj) = p.as_object_mut() {
                obj.insert("ts".to_string(), json!(e.ts));
            }
            p
        })
        .collect()
}

/// 最近一次 acceptor 的反对意见（供 executor 下一轮修订携带）。
pub fn last_feedback(conn: &Connection, task_id: &str) -> Option<String> {
    history(conn, task_id)
        .into_iter()
        .rev()
        .find(|r| r["role"] == "acceptor" && r["agree"] == false)
        .and_then(|r| r["comment"].as_str().map(str::to_string))
        .filter(|s| !s.trim().is_empty())
}

/// 该任务是否处于「协商中」且需要调度器派发协商 worker。
pub fn needs_dispatch(t: &Task) -> bool {
    t.status == "planned" && t.acceptance_required && t.negotiation_status == "pending"
}

/// 角色候选列表（与 `pool::resolve_role` 同口径：项目池优先，未配置则全量角色实例）。
fn candidates(conn: &Connection, project_id: &str, role: &str) -> Vec<String> {
    let pool = db::project_pool(conn, project_id);
    match pool.get(role) {
        Some(list) => list.clone(),
        None => db::list_agents(conn)
            .into_iter()
            .filter(|a| a.roles.iter().any(|r| r == role))
            .map(|a| a.id)
            .collect(),
    }
}

/// 项目是否具备协商条件：执行者与验收者两个角色都存在可用候选实例。
/// 供任务创建时决定默认协商状态（无法协商的任务退回「标准由规划者固定」）。
pub fn possible(conn: &Connection, project_id: &str) -> bool {
    let usable = |role: &str| -> bool {
        candidates(conn, project_id, role).iter().any(|id| {
            db::get_agent(conn, id)
                .map(|a| {
                    a.status != "deleted"
                        && a.roles.iter().any(|r| r == role)
                        // 角色隔离：决策者不能充当协商双方
                        && (crate::roles::overlap_allowed() || !crate::roles::is_decision_maker(&a.roles))
                })
                .unwrap_or(false)
        })
    };
    usable("executor") && usable("acceptor")
}

/// 解析某角色 agent，额外排除若干实例（用于保证验收者 ≠ 执行者）。
fn resolve_role_excluding(
    state: &Arc<App>,
    conn: &Connection,
    task: &Task,
    role: &str,
    exclude: &[String],
) -> Option<String> {
    let bound = if role == "acceptor" { task.acceptor_agent_id.clone() } else { task.executor_agent_id.clone() };
    if task.agent_pinned {
        if let Some(b) = bound.filter(|id| !id.is_empty() && !exclude.contains(id)) {
            return db::get_agent(conn, &b).map(|_| b);
        }
    }
    let tried = db::task_tried(conn, &task.id);
    let tried_ids: Vec<String> = tried
        .get(role)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    for id in candidates(conn, &task.project_id, role) {
        if exclude.contains(&id) || tried_ids.contains(&id) {
            continue;
        }
        let Some(a) = db::get_agent(conn, &id) else { continue };
        if a.status == "deleted" || !a.roles.iter().any(|r| r == role) {
            continue;
        }
        // 角色隔离（裁判不下场）：协商双方都是执行/验收角色，决策者不得充当其一。
        if crate::roles::is_decision_maker(&a.roles) && !crate::roles::overlap_allowed() {
            continue;
        }
        if crate::pool::circuit_open(state, &id) {
            continue;
        }
        return Some(id);
    }
    None
}

/// 派发协商 worker（单任务独占：`has_live_handle` 由调度器先判）。
/// M4.5：规划不绑定 agent，这里按「角色 + 项目池优先级」动态解析执行者/验收者并落回任务字段。
/// 协商阶段不占用 agent 的 `current_task_ids`（不改执行/验收负载口径），
/// 但同一任务不会并发协商；跨任务的并发由适配器/会话隔离。
pub fn dispatch(state: &Arc<App>, conn: &Connection, t: &Task) -> bool {
    let mut t = t.clone();
    if t.executor_agent_id.as_deref().filter(|s| !s.is_empty()).is_none() {
        match resolve_role_excluding(state, conn, &t, "executor", &[]) {
            Some(id) => {
                db::set_task_agent(conn, &t.id, "executor", Some(&id), t.agent_pinned);
                t.executor_agent_id = Some(id);
            }
            None => return false, // 池内暂无可派执行者 → 下一轮再试
        }
    }
    if t.acceptor_agent_id.as_deref().filter(|s| !s.is_empty()).is_none() {
        let exclude: Vec<String> = t.executor_agent_id.iter().cloned().collect();
        match resolve_role_excluding(state, conn, &t, "acceptor", &exclude) {
            Some(id) => {
                db::set_task_agent(conn, &t.id, "acceptor", Some(&id), t.agent_pinned);
                t.acceptor_agent_id = Some(id);
            }
            None => return false, // 池内暂无可派验收者 → 下一轮再试
        }
    }
    let (Some(exec_id), Some(acc_id)) = (t.executor_agent_id.clone(), t.acceptor_agent_id.clone()) else {
        return false;
    };
    let (executor, acceptor) = match (db::get_agent(conn, &exec_id), db::get_agent(conn, &acc_id)) {
        (Some(e), Some(a)) => (e, a),
        _ => return false,
    };
    if exec_id == acc_id {
        return false; // 同一实例禁止既执行又验收（创建时已校验；防御性）
    }
    // 角色隔离（裁判不下场）：协商双方必须是执行/验收实例，决策者不得参与。
    if !crate::roles::overlap_allowed()
        && (crate::roles::is_decision_maker(&executor.roles)
            || crate::roles::is_decision_maker(&acceptor.roles))
    {
        tracing::warn!(
            task = %t.id,
            executor = %exec_id,
            acceptor = %acc_id,
            "negotiation skipped: decision maker instance must not act as executor/acceptor (裁判不下场)"
        );
        return false;
    }
    events::push_event(
        state, conn, &t.project_id, Some(&t.id), "agent.session_started",
        json!({"agent_id": exec_id, "phase": "negotiate", "role": "executor",
               "session_id": opencode::negotiate_exec_session(&t.id),
               "title": t.title,
               "purpose": format!("验收标准协商：执行者《{}》提出方案（第 {} 轮）", t.title, t.negotiation_round),
               "trigger": "scheduled"}),
    );
    events::push_event(
        state, conn, &t.project_id, Some(&t.id), "agent.session_started",
        json!({"agent_id": acc_id, "phase": "negotiate", "role": "acceptor",
               "session_id": opencode::negotiate_acc_session(&t.id),
               "title": t.title,
               "purpose": format!("验收标准协商：验收者评审《{}》（第 {} 轮）", t.title, t.negotiation_round),
               "trigger": "scheduled"}),
    );
    let state2 = state.clone();
    let task = t.clone();
    let hid = state.alloc_handle_id();
    let h = tokio::spawn(async move { worker(state2, task, executor, acceptor, hid).await });
    state.register_handle(&t.id, hid, h);
    true
}

async fn adapter_propose(
    state: &Arc<App>,
    task: &Task,
    agent: &Agent,
    round: i64,
    feedback: Option<&str>,
) -> Result<String, String> {
    match agent.adapter.as_str() {
        "opencode" => opencode::negotiate_propose(state.clone(), task.clone(), agent.clone(), round, feedback).await,
        _ => mock::negotiate_propose(state.clone(), task.clone(), agent.clone(), round, feedback).await,
    }
}

async fn adapter_review(
    state: &Arc<App>,
    task: &Task,
    agent: &Agent,
    proposal: &str,
) -> Result<crate::agents::Review, String> {
    match agent.adapter.as_str() {
        "opencode" => opencode::negotiate_review(state.clone(), task.clone(), agent.clone(), proposal).await,
        _ => mock::negotiate_review(state.clone(), task.clone(), agent.clone(), proposal).await,
    }
}

fn session_ended(state: &Arc<App>, task: &Task, agent_id: &str, session: &str, result: &str) {
    let c = state.db.lock().unwrap();
    let detail = match result {
        "success" => "协商达成一致".to_string(),
        "failed" => "协商运行失败".to_string(),
        other => format!("协商结束：{other}"),
    };
    events::push_event(
        state, &c, &task.project_id, Some(&task.id), "agent.session_ended",
        json!({"agent_id": agent_id, "phase": "negotiate", "session_id": session,
               "result": result, "negotiation_round": task.negotiation_round,
               "detail": detail}),
    );
}

async fn worker(state: Arc<App>, task: Task, executor: Agent, acceptor: Agent, hid: u64) {
    let guard = HandleGuard { state: state.clone(), task_id: task.id.clone(), id: hid };
    let mut round = task.negotiation_round.max(0);
    let mut proposal = task.acceptance_criteria.clone().unwrap_or_default();
    let mut feedback: Option<String> = {
        let conn = state.db.lock().unwrap();
        last_feedback(&conn, &task.id)
    };

    {
        let conn = state.db.lock().unwrap();
        let kind = if round > 0 { "negotiation.resumed" } else { "negotiation.started" };
        events::push_event(
            &state, &conn, &task.project_id, Some(&task.id), kind,
            json!({"round": round, "max_rounds": MAX_NEGOTIATION_ROUNDS,
                   "proposed_criteria": task.acceptance_criteria,
                   "carried_feedback": feedback}),
        );
    }

    let failure: String;
    loop {
        if round >= MAX_NEGOTIATION_ROUNDS {
            failure = format!("协商 {MAX_NEGOTIATION_ROUNDS} 轮仍未达成一致（超出最大轮次）");
            break;
        }
        round += 1;

        // 1) executor 提出 / 修订提案（携带任务上下文 + 历史轮次与上轮意见）
        let new_proposal = match adapter_propose(&state, &task, &executor, round, feedback.as_deref()).await {
            Ok(p) if !p.trim().is_empty() => p,
            Ok(_) => {
                failure = format!("executor 在第 {round} 轮未输出有效验收标准");
                break;
            }
            Err(e) => {
                failure = format!("executor 协商运行失败（第 {round} 轮）：{e}");
                break;
            }
        };
        proposal = new_proposal;
        {
            let conn = state.db.lock().unwrap();
            events::push_event(
                &state, &conn, &task.project_id, Some(&task.id), "negotiation.round",
                json!({"round": round, "role": "executor", "proposal": proposal,
                       "carried_feedback": feedback, "agent_id": executor.id}),
            );
        }

        // 2) acceptor 审核该轮提案
        let review = match adapter_review(&state, &task, &acceptor, &proposal).await {
            Ok(r) => r,
            Err(e) => {
                failure = format!("acceptor 协商运行失败（第 {round} 轮）：{e}");
                break;
            }
        };
        {
            let conn = state.db.lock().unwrap();
            events::push_event(
                &state, &conn, &task.project_id, Some(&task.id), "negotiation.round",
                json!({"round": round, "role": "acceptor", "agree": review.agree,
                       "comment": review.comment,
                       "criteria": review.criteria, "agent_id": acceptor.id}),
            );
        }

        if review.agree {
            let criteria = review
                .criteria
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| proposal.clone());
            {
                let conn = state.db.lock().unwrap();
                db::set_task_negotiation(&conn, &task.id, "agreed", round, Some(&criteria));
                events::push_event(
                    &state, &conn, &task.project_id, Some(&task.id), "negotiation.agreed",
                    json!({"rounds": round, "criteria": criteria,
                           "executor": executor.id, "acceptor": acceptor.id}),
                );
                if let Some(fresh) = db::get_task(&conn, &task.id) {
                    events::send_task(&state, &fresh);
                }
                tracing::info!(task = %task.id, round, "negotiation agreed");
            }
            session_ended(&state, &task, &executor.id, &opencode::negotiate_exec_session(&task.id), "agreed");
            session_ended(&state, &task, &acceptor.id, &opencode::negotiate_acc_session(&task.id), "agreed");
            drop(guard);
            state.wake();
            return;
        }

        // 不一致：记录中间态（pending + 已完成轮次 + 当前草案），重启后从此继续
        feedback = Some(review.comment.clone()).filter(|s| !s.trim().is_empty());
        {
            let conn = state.db.lock().unwrap();
            db::set_task_negotiation(&conn, &task.id, "pending", round, Some(&proposal));
            if let Some(fresh) = db::get_task(&conn, &task.id) {
                events::send_task(&state, &fresh);
            }
        }
    }

    // 协商失败：写入 failed 状态与最终草案，自动登记决策者裁决请求
    let reason = if failure.trim().is_empty() { "协商未达成一致".to_string() } else { failure };
    {
        let conn = state.db.lock().unwrap();
        db::set_task_negotiation(&conn, &task.id, "failed", round, Some(&proposal));
        let hist = history(&conn, &task.id);
        events::push_event(
            &state, &conn, &task.project_id, Some(&task.id), "negotiation.failed",
            json!({"rounds": round, "max_rounds": MAX_NEGOTIATION_ROUNDS,
                   "reason": reason, "proposed_criteria": proposal,
                   "executor": executor.id, "acceptor": acceptor.id}),
        );
        // 自愈保护：已有协商失败决策时不重复登记
        decision::ensure(
            &state, &conn, &db::get_task(&conn, &task.id).unwrap_or_else(|| task.clone()),
            "negotiation_failed",
            json!({"reason": "acceptance criteria negotiation failed",
                   "detail": reason,
                   "proposed_criteria": proposal,
                   "rounds": round,
                   "history": hist,
                   "title": task.title}),
        );
        if let Some(fresh) = db::get_task(&conn, &task.id) {
            events::send_task(&state, &fresh);
        }
        tracing::warn!(task = %task.id, round, %reason, "negotiation failed");
    }
    session_ended(&state, &task, &executor.id, &opencode::negotiate_exec_session(&task.id), "failed");
    session_ended(&state, &task, &acceptor.id, &opencode::negotiate_acc_session(&task.id), "failed");
    drop(guard);
    state.wake();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(status: &str, negotiation: &str) -> Task {
        Task {
            id: "tsk_1".into(),
            project_id: "prj_1".into(),
            milestone_id: None,
            title: "t".into(),
            description: String::new(),
            depends_on: vec![],
            assignee_role: "executor".into(),
            executor_agent_id: Some("a".into()),
            acceptor_agent_id: Some("b".into()),
            acceptance_required: true,
            acceptance_criteria: None,
            negotiation_status: negotiation.into(),
            negotiation_round: 0,
            execution_plan: None,
            plan_rejection_count: 0,
            scope_repos: Vec::new(),
            agent_pinned: false,
            tried_agents: serde_json::json!({}),
            status: status.into(),
            attempt: 0,
            rejection_count: 0,
            failure_count: 0,
            artifacts: vec![],
            created_at: 0,
        }
    }

    #[test]
    fn needs_dispatch_only_for_planned_pending_acceptance() {
        assert!(needs_dispatch(&task("planned", "pending")));
        assert!(!needs_dispatch(&task("planned", "agreed")));
        assert!(!needs_dispatch(&task("planned", "failed")));
        assert!(!needs_dispatch(&task("ready", "pending")));
        let mut t = task("planned", "pending");
        t.acceptance_required = false;
        assert!(!needs_dispatch(&t));
    }

    #[test]
    fn history_rebuilds_rounds_from_events() {
        let c = crate::db::open(":memory:");
        let st = Arc::new(App {
            db: std::sync::Mutex::new(crate::db::open(":memory:")),
            tx: tokio::sync::broadcast::channel(1).0,
            handles: Default::default(),
            circuits: Default::default(),
            handle_seq: Default::default(),
            wake: Default::default(),
            sessions: Default::default(),
        });
        events::push_event(&st, &c, "prj_1", Some("tsk_1"), "negotiation.round",
            json!({"round": 1, "role": "acceptor", "agree": false, "comment": "补充指标"}));
        events::push_event(&st, &c, "prj_1", Some("tsk_1"), "negotiation.round",
            json!({"round": 2, "role": "acceptor", "agree": true, "comment": "通过"}));
        let h = history(&c, "tsk_1");
        assert_eq!(h.len(), 2);
        assert_eq!(h[0]["round"], 1);
        assert_eq!(last_feedback(&c, "tsk_1").as_deref(), Some("补充指标"));
    }
}
