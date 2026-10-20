//! 调度循环：派发、状态推进、失败处置、卡死判定与恢复。
//!
//! - 派发前必须校验：项目未暂停/未归档、依赖全部完成、gate 未冻结、agent 可用
//!   （未熔断、未达并行上限）。任何一条不满足都只能等待，不能"先派了再说"。
//! - 选人与排队规则见 `pool`；同一任务的执行者与验收者**不得**是同一实例。
//! - 卡死用**空闲看门狗**（持续无输出达到该 agent 的 `timeout_ms`），不限制总运行时长；
//!   终止顺序 TERM → 3s → KILL，错误信息保留 `timed out` 字样，便于归类为可换人的超时。
//! - 换人重试不计失败预算；池内候选耗尽才计 1 次失败，累计 3 次挂起；
//!   连续失败按指数退避重试（5s → 300s 封顶）。
//! - 恢复必须可重入：重复执行不得产生重复收件箱条目、重复计次或重复提交。

use std::{collections::{HashMap, HashSet}, sync::Arc, time::Duration};
use tokio::time::MissedTickBehavior;

use crate::{
    agents::{mock, opencode, ExecMode, ExecOutcome},
    db,
    decision,
    events,
    models::*,

    negotiation,
    recovery::{self, TIER_HANDOFF, TIER_NATIVE},
    replan,
    report,
    state::{App, HandleGuard},
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub async fn run(state: Arc<App>) {
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = state.wake.notified() => {}
        }
        scan(&state);
        decision::scan(&state);
        replan::scan(&state);
    }
}

const TERMINAL: [&str; 2] = ["done", "cancelled"];

fn deps_met(map: &HashMap<String, Task>, t: &Task) -> bool {
    t.depends_on.iter().all(|d| {
        map.get(d)
            .map(|p| p.status == "done")
            .unwrap_or(true) // unknown dep is validated at creation; treat as satisfied
    })
}

/// One scan pass over all running projects. May perform dispatches / status
/// transitions synchronously; workers run as tagged tasks.
fn scan(state: &Arc<App>) {
    let conn = state.db.lock().unwrap();

    // M3.5 状态机：
    // - running：完整调度（协商/执行/验收派发）
    // - initializing + 已确认：仅派发验收标准协商；协商全部完成后自动转 running
    // - pausing：冻结派发，仅在途会话全部结束后转 paused
    let projects: Vec<_> = db::list_projects(&conn)
        .into_iter()
        .filter(|p| p.status == "running" || p.status == "pausing")
        .collect();

    for project in projects {
        let tasks = db::list_tasks(&conn, &project.id);

        if project.status == "pausing" {
            // 所有会话停止 → 已暂停
            if !tasks.iter().any(|t| state.has_live_handle(&t.id)) {
                if let Some(p) = db::get_project(&conn, &project.id) {
                    let _ = conn.execute("UPDATE projects SET status='paused' WHERE id=?1", params![project.id]);
                    events::push_event(state, &conn, &project.id, None, "project.status_changed",
                        json!({"from": "pausing", "to": "paused", "reason": "sessions_drained"}));
                    events::send_project(state, &p);
                }
            }
            continue; // 暂停中不派发任何新工作
        }

        // M4.5：多仓库项目禁止任务并行（保证合入前同步 + 合入后评估的有效性）：
        // 该项目内已有任一在途会话 → 本轮不派发任何新工作。
        let multi_repo = db::project_repos(&conn, &project.id).len() > 1;
        if multi_repo && tasks.iter().any(|t| state.has_live_handle(&t.id)) {
            continue;
        }

        let mut map: HashMap<String, Task> = tasks.iter().map(|t| (t.id.clone(), t.clone())).collect();

        // planned -> ready：协商一致（criteria_agreed_auto）或豁免已获决策者裁定
        // 才放行；协商失败 / 豁免待裁 → 保持 planned（decision::scan 负责派发裁定）。
        for t in tasks.clone() {
            if t.status == "planned" {
                if let Some(reason) = decision::planned_gate(&conn, &t) {
                    events::status_changed(state, &conn, &t, "ready", reason);
                    map.entry(t.id.clone()).and_modify(|m| m.status = "ready".into());
                }
            }
        }

        // M3: milestone gate — when every task inside a milestone is DONE the
        // milestone enters `awaiting_confirm` and a milestone_gate item lands in
        // the inbox. Idempotent: reopening an already-awaiting milestone only
        // ensures its inbox item exists (survives restart).
        for m in db::list_milestones(&conn, &project.id) {
            let members: Vec<Task> = tasks
                .iter()
                .filter(|t| t.milestone_id.as_deref() == Some(m.id.as_str()))
                .cloned()
                .collect();
            let member_ids: Vec<String> = members.iter().map(|t| t.id.clone()).collect();
            match m.status.as_str() {
                "pending" => {
                    if !members.is_empty() && members.iter().all(|t| t.status == "done") {
                        let m2 = events::milestone_status_changed(state, &conn, &m, "awaiting_confirm", "all_milestone_tasks_done");
                        events::push_event(state, &conn, &m.project_id, None, "milestone.gate_opened",
                            json!({"milestone_id": m.id, "name": m.name, "goal": m.goal,
                                   "effect": m.effect, "task_ids": member_ids}));
                        ensure_gate_item(state, &conn, &m2, &member_ids);
                    }
                }
                "awaiting_confirm" => {
                    // restart recovery: an awaiting milestone must always own an open gate item
                    if db::open_gate_item(&conn, &m.id).is_none() {
                        tracing::warn!(milestone = %m.id, "awaiting_confirm milestone missing gate inbox item; re-inserting");
                        ensure_gate_item(state, &conn, &m, &member_ids);
                    }
                }
                "failed" => {
                    // Rejected gate: once the human has worked the replanning
                    // window (it was opened and is now closed) and every member
                    // task — including any appended in the window — is done
                    // again, re-present the gate so the branch can be released.
                    let all_done = !members.is_empty() && members.iter().all(|t| t.status == "done");
                    let window_open = db::open_window_item(&conn, &m.id).is_some();
                    if should_regate_failed_milestone(all_done, db::has_window_item(&conn, &m.id), window_open) {
                        let m2 = events::milestone_status_changed(state, &conn, &m, "awaiting_confirm", "window_replan_done");
                        events::push_event(state, &conn, &m.project_id, None, "milestone.gate_opened",
                            json!({"milestone_id": m.id, "name": m.name, "goal": m.goal,
                                   "effect": m.effect, "task_ids": member_ids,
                                   "reason": "window_replan_done"}));
                        ensure_gate_item(state, &conn, &m2, &member_ids);
                    }
                }
                _ => {}
            }
        }

        // gate rule: a milestone that is awaiting confirmation (or failed) blocks
        // *only* its downstream dependency branch; unrelated branches keep running.
        let milestones = db::list_milestones(&conn, &project.id);
        let blocked = blocked_by_gates(&tasks, &milestones);

        for t in tasks.iter() {
            if blocked.contains(&t.id) {
                continue; // frozen behind an unconfirmed milestone gate
            }
            match t.status.as_str() {
                // M3 协商：需验收的 planned 任务先由 executor↔acceptor 协商验收标准，
                // 一致后才由 planned_gate 放行；协商未完成的任务不会被派发执行。
                "planned" if !state.has_live_handle(&t.id) && negotiation::needs_dispatch(t) => {
                    if negotiation::dispatch(state, &conn, t) {
                        drop(conn);
                        state.wake();
                        return;
                    }
                }
                "ready" => {
                    if deps_met(&map, t) && !state.has_live_handle(&t.id) {
                        // M4 流程：先制定任务计划（执行者）→ 验收者审查 → 才进入执行
                        if dispatch_plan(state, &conn, t, "scheduled") {
                            drop(conn);
                            state.wake();
                            return;
                        }
                    }
                }
                // 计划制定阶段中断/失败重试（含审查打回后的重拟）
                "plan_drafting" if !state.has_live_handle(&t.id) => {
                    if t.failure_count >= 3 {
                        suspend(state, &conn, t, "plan");
                    } else if !backoff_elapsed(&conn, t) {
                        // 退避窗口内
                    } else if dispatch_plan(state, &conn, t, "retry") {
                        drop(conn);
                        state.wake();
                        return;
                    }
                }
                // 计划审查阶段：派发验收者审查（占用即视为进行中）
                "plan_review" if !state.has_live_handle(&t.id) => {
                    // M4.5：验收者由池优先级动态解析（dispatch 内部解析），无需预绑定
                    if backoff_elapsed(&conn, t) && dispatch_plan_review(state, &conn, t, "") {
                        drop(conn);
                        state.wake();
                        return;
                    }
                }
                // 执行 worker 结束但最终状态没落下：
                //  a) 进程重启中断（boot 已标记 recovery_pending）→ M4 三档恢复
                //     （原生 resume → 交接摘要重建 → 人工重建），attempt 不自增；
                //  b) 上一轮真实执行失败（有失败计数）→ 原有失败重试语义（attempt+1 全新派发）。
                // M5 状态归一：中断尝试（recovering）等待恢复续跑；agent 忙则继续排队
                "recovering" if !state.has_live_handle(&t.id) => {
                    if db::recovery_state(&conn, &t.id).pending {
                        if recover_dispatch(state, &conn, &t) {
                            drop(conn);
                            state.wake();
                            return;
                        }
                    } else if t.failure_count >= 3 {
                        suspend(state, &conn, t, "execute");
                    } else if !backoff_elapsed(&conn, t) {
                        // 退避窗口内
                    } else if dispatch_exec(state, &conn, t, "retry") {
                        drop(conn);
                        state.wake();
                        return;
                    }
                }
                "executing" if !state.has_live_handle(&t.id) => {
                    if db::recovery_state(&conn, &t.id).pending {
                        if recover_dispatch(state, &conn, &t) {
                            drop(conn);
                            state.wake();
                            return;
                        }
                        // 派发守卫暂不满足（agent 忙/悬空等）：本轮不动，等下一轮 scan
                    } else if t.failure_count >= 3 {
                        suspend(state, &conn, t, "execute");
                    } else if t.rejection_count >= 2 {
                        escalate(state, &conn, t, "acceptance");
                    } else if !backoff_elapsed(&conn, t) {
                        // 指数退避：连续快速失败时压住重试风暴（限流/供应商故障场景）
                    } else {
                        // failed-attempt retry: re-dispatch as a fresh attempt（attempt+1）
                        if dispatch_exec(state, &conn, t, "retry") {
                            drop(conn);
                            state.wake();
                            return;
                        }
                    }
                }
                // 第三档人工重建挂起的任务：重启后重新进一轮三档（每轮重启至多一轮）
                "suspended" if !state.has_live_handle(&t.id) => {
                    if db::recovery_state(&conn, &t.id).pending
                        && recover_dispatch(state, &conn, &t)
                    {
                        drop(conn);
                        state.wake();
                        return;
                    }
                }
                "awaiting_acceptance" if !state.has_live_handle(&t.id) => {
                    // M4.5：验收者由池优先级动态解析；池内无可用候选 → 等待（人工验收仍可 REST 触发）
                    if backoff_elapsed(&conn, t) {
                        let resolved = crate::pool::resolve_role(state, &conn, &t.project_id, t, "acceptor");
                        if let Some(acc) = resolved {
                            if db::agent_free(&conn, &acc) {
                                dispatch_accept(state, &conn, t, &acc);
                                drop(conn);
                                state.wake();
                                return;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // project completion: all tasks terminal AND no gate left open
        let all = db::list_tasks(&conn, &project.id);
        let gate_open = milestones
            .iter()
            .any(|m| m.status == "awaiting_confirm" || m.status == "failed");
        if !all.is_empty()
            && !gate_open
            && all.iter().all(|t| TERMINAL.contains(&t.status.as_str()))
        {
            let _ = conn.execute("UPDATE projects SET status='completed' WHERE id=?1", params![project.id]);
            if let Some(p) = db::get_project(&conn, &project.id) {
                events::push_event(state, &conn, &project.id, None, "project.completed", json!({}));
                events::send_project(state, &p);
                // M3 项目总结报告：COMPLETED 时自动触发一次（无报告才生成，
                // 失败靠前端/API 手工重试，不在此处循环重试）。
                report::maybe_generate_on_completion(state, &conn, &p);
            }
        }
    }
}

/// Create the milestone_gate inbox item if the milestone has no open one.
fn ensure_gate_item(state: &Arc<App>, conn: &Connection, m: &Milestone, task_ids: &[String]) -> InboxItem {
    if let Some(item) = db::open_gate_item(conn, &m.id) {
        return item;
    }
    events::new_inbox_item(
        state,
        conn,
        &m.project_id,
        "milestone_gate",
        json!({
            "milestone_id": m.id,
            "name": m.name,
            "goal": m.goal,
            "effect": m.effect,
            "task_ids": task_ids,
            "detail": format!("里程碑「{}」内全部任务已完成，等待人工确认后放行下游分支", m.name),
        }),
    )
}

/// A `failed` milestone re-enters the confirmation gate when its member tasks
/// are all done again, but only after its replanning window was actually opened
/// and then closed by the human. A stale `failed` milestone with no window (or
/// one still awaiting work) stays frozen so the downstream branch is never
/// released without an explicit human pass.
pub fn should_regate_failed_milestone(members_all_done: bool, has_window: bool, window_open: bool) -> bool {
    members_all_done && has_window && !window_open
}

/// Downstream dependency closure crossing out of every not-yet-passed
/// milestone. A task is blocked when its transitive dependency closure reaches
/// a task that belongs to a milestone which has not `passed`, *unless* the task
/// itself is in that same milestone (within-milestone deps must keep running).
///
/// This holds the milestone's downstream branch from the moment it starts being
/// relevant — not only once `awaiting_confirm` is reached — which removes the
/// race where one milestone task finishing early could release a cross-milestone
/// dependent before the gate opens. A task in an unrelated branch is never
/// included, so it keeps being dispatched.
pub fn blocked_by_gates(tasks: &[Task], milestones: &[Milestone]) -> HashSet<String> {
    let ms_status: HashMap<&str, &str> =
        milestones.iter().map(|m| (m.id.as_str(), m.status.as_str())).collect();
    let by_id: HashMap<&str, &Task> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut blocked: HashSet<String> = HashSet::new();
    for t in tasks {
        let own = t.milestone_id.as_deref();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = t.depends_on.iter().map(|s| s.as_str()).collect();
        let mut is_blocked = false;
        while let Some(dep) = stack.pop() {
            if !seen.insert(dep) {
                continue;
            }
            if let Some(dt) = by_id.get(dep) {
                if let Some(dm) = dt.milestone_id.as_deref() {
                    let status = ms_status.get(dm).copied().unwrap_or("passed");
                    if Some(dm) != own && status != "passed" {
                        is_blocked = true;
                        break;
                    }
                }
                stack.extend(dt.depends_on.iter().map(|s| s.as_str()));
            }
        }
        if is_blocked {
            blocked.insert(t.id.clone());
        }
    }
    blocked
}

/// 从事件流取该任务最近一次 run 级失败（执行失败 / 验收退回 / 验收基建失败）。
/// 返回 (事件类型, 具体原因)——事件类型用于区分执行端/验收端问题。
fn last_fail_reason(conn: &Connection, task_id: &str) -> Option<(String, String)> {
    let mut stmt = conn.prepare(
        "SELECT type, payload FROM events WHERE task_id=?1 AND type IN \
         ('task.execution_failed','acceptance.rejected','acceptance.run_failed') ORDER BY ts DESC LIMIT 1",
    ).ok()?;
    let (typ, raw): (String, String) = stmt
        .query_row(params![task_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let msg = v.get("error").and_then(|e| e.as_str())
        .or_else(|| v.get("comment").and_then(|e| e.as_str()))
        .map(String::from)?;
    Some((typ, msg))
}

/// 最近一次 run 级失败距今是否已过指数退避窗口。
/// 退避随当前失败连击拉长：5s → 15s → 45s → 120s → 封顶 300s。
/// 无失败记录（首次派发/人工恢复后清除）→ 立即可派。
fn backoff_elapsed(conn: &Connection, t: &Task) -> bool {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(i64::MAX);
    let ts: Option<i64> = conn
        .query_row(
            "SELECT MAX(ts) FROM events WHERE task_id=?1 AND type IN ('task.execution_failed','acceptance.run_failed')",
            params![t.id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let Some(ts) = ts else { return true };
    let streak = t.failure_count.max(1) as i64;
    let ladder = [5i64, 15, 45, 120, 300];
    let delay = ladder[(streak as usize - 1).min(ladder.len() - 1)];
    now_ms - ts >= delay * 1000
}

pub fn agent_summary(conn: &Connection, id: &Option<String>) -> Option<String> {
    id.as_deref().filter(|s| !s.is_empty())
        .and_then(|a| db::get_agent(conn, a))
        .map(|a| {
            let model = a.config.get("model").and_then(|m| m.as_str()).unwrap_or("default");
            format!("{} ({}, model={}, roles={})", a.name, a.adapter, model, a.roles.join("/"))
        })
}

/// M4.5 失败统一处理：错误分类 → 限流/配置错误熔断该 agent（全局）→ 换下一优先级
/// agent 重试（不计入失败预算）→ 池内耗尽才返回 false 交给调用方计失败。
fn handle_agent_failure(
    state: &Arc<App>,
    conn: &Connection,
    t: &Task,
    role: &str,
    phase: &str,
    msg: &str,
) -> bool {
    let bound = if role == "acceptor" { t.acceptor_agent_id.clone() } else { t.executor_agent_id.clone() };
    let (class, disposition, cooldown) = match bound.as_deref().and_then(|id| db::get_agent(conn, id)) {
        Some(agent) => {
            let err = crate::pool::classify_error(&agent, msg);
            if matches!(err.class, crate::pool::ErrorClass::RateLimit | crate::pool::ErrorClass::Config) {
                crate::pool::circuit_open_for(state, conn, &agent, &err);
            }
            (err.class.as_str().to_string(), err.disposition.clone(), err.cooldown_ms)
        }
        None => ("unknown".to_string(), "未找到失败 agent 记录".to_string(), None),
    };
    events::push_event(state, conn, &t.project_id, Some(&t.id), "agent.error_classified",
        json!({"role": role, "phase": phase, "class": class, "disposition": disposition,
               "cooldown_ms": cooldown, "error": msg}));
    crate::pool::switch_on_failure(state, conn, t, role, &format!("{phase} 失败（{class}）"))
}

fn suspend(state: &Arc<App>, conn: &Connection, t: &Task, phase: &str) {
    let t = events::status_changed(state, conn, t, "suspended", "failure_limit");
    let (fail_type, last_err) = last_fail_reason(conn, &t.id)
        .unwrap_or_else(|| ("unknown".into(), "原因未知（无失败事件记录，查看任务日志）".into()));
    let (reason, detail) = match phase {
        "accept" => ("acceptance_failure_limit", format!("task '{}' acceptance run failed {} times (executor succeeded; acceptance infrastructure/agent is failing)", t.title, t.failure_count)),
        "plan" => ("plan_failure_limit", format!("task '{}' plan drafting failed {} times", t.title, t.failure_count)),
        _ => ("failure_limit", format!("task '{}' execution failed {} times", t.title, t.failure_count)),
    };
    let executor = agent_summary(conn, &t.executor_agent_id).unwrap_or_else(|| "未指派".into());
    let acceptor = agent_summary(conn, &t.acceptor_agent_id).unwrap_or_else(|| "未指派".into());
    events::push_event(
        state, conn, &t.project_id, Some(&t.id), "escalation.raised",
        json!({
            "task_id": t.id, "reason": reason, "phase": phase,
            "detail": detail,
            "last_failure_type": fail_type, "last_error": last_err,
            "executor": executor, "acceptor": acceptor, "attempt": t.attempt,
        }),
    );
    events::new_inbox_item(
        state, conn, &t.project_id, "escalation",
        json!({
            "task_id": t.id, "reason": reason, "phase": phase,
            "detail": detail,
            "last_failure_type": fail_type, "last_error": last_err,
            "executor": executor, "acceptor": acceptor, "attempt": t.attempt,
        }),
    );
}

fn escalate(state: &Arc<App>, conn: &Connection, t: &Task, phase: &str) {
    let t = events::status_changed(state, conn, t, "escalated", "rejection_limit");
    let last_err = last_fail_reason(conn, &t.id)
        .map(|(_, msg)| msg)
        .unwrap_or_else(|| "原因未知（无退回事件记录，查看任务日志）".into());
    let executor = agent_summary(conn, &t.executor_agent_id).unwrap_or_else(|| "未指派".into());
    let acceptor = agent_summary(conn, &t.acceptor_agent_id).unwrap_or_else(|| "未指派".into());
    events::push_event(
        state, conn, &t.project_id, Some(&t.id), "escalation.raised",
        json!({
            "task_id": t.id,
            "reason": if phase == "plan" { "plan_rejection_limit" } else { "rejection_limit" },
            "phase": phase,
            "detail": if phase == "plan" {
                format!("任务《{}》计划被验收者连续打回 {} 轮，升级待裁决", t.title, t.plan_rejection_count)
            } else {
                format!("任务《{}》验收退回 {} 轮，升级待裁决", t.title, t.rejection_count)
            },
            "last_error": last_err, "executor": executor, "acceptor": acceptor,
            "attempt": t.attempt,
        }),
    );
    events::new_inbox_item(
        state, conn, &t.project_id, "escalation",
        json!({
            "task_id": t.id, "reason": "rejection_limit",
            "detail": format!("task '{}' returned {} times", t.title, t.rejection_count),
            "last_error": last_err, "executor": executor, "acceptor": acceptor,
            "attempt": t.attempt,
        }),
    );
}

fn set_counts(state: &Arc<App>, conn: &Connection, task_id: &str, sql_set: &str, val: i64) {
    let sql = format!("UPDATE tasks SET {sql_set}=?2 WHERE id=?1");
    let _ = conn.execute(&sql, params![task_id, val]);
    if let Some(f) = db::get_task(conn, task_id) {
        events::send_task(state, &f);
    }
}

/// M4.5 派发守卫：实例存在、未软删、持有该角色、未熔断、且有空闲槽位。
/// **空闲不作为分配依据**（选人阶段已按优先级定人）；此处只判断"此刻能否开工"——
/// 不满足 → None（任务在该 agent 处排队，下一轮 scan 再试，不降级到低优先级）。
fn resolve_agent_for_role(
    state: &Arc<App>,
    conn: &Connection,
    task_id: &str,
    agent_id: &str,
    role: &str,
) -> Option<Agent> {
    let agent = db::get_agent(conn, agent_id)?;
    if agent.status == "deleted" {
        return None;
    }
    if !agent.roles.iter().any(|r| r == role) {
        tracing::warn!(task = %task_id, agent = %agent_id, role, "agent lacks required role (dangling assignment); dispatch skipped");
        return None;
    }
    if crate::pool::circuit_open(state, agent_id) {
        return None; // 熔断中：排队等待
    }
    if !db::agent_free(conn, agent_id) {
        return None; // 忙：排队等待
    }
    Some(agent)
}

/// M4.5：按角色 + 项目池优先级解析 agent（人类固定优先），解析结果落回任务字段。
fn resolve_and_bind_role(
    state: &Arc<App>,
    conn: &Connection,
    t: &Task,
    role: &str,
) -> Option<(String, Agent)> {
    let id = crate::pool::resolve_role(state, conn, &t.project_id, t, role)?;
    let bound = if role == "acceptor" { t.acceptor_agent_id.clone() } else { t.executor_agent_id.clone() };
    if bound.as_deref() != Some(id.as_str()) {
        db::set_task_agent(conn, &t.id, role, Some(&id), t.agent_pinned);
    }
    let agent = resolve_agent_for_role(state, conn, &t.id, &id, role)?;
    Some((id, agent))
}

/// Commit an execution dispatch (status -> executing) and spawn the worker.
fn dispatch_exec(state: &Arc<App>, conn: &Connection, t: &Task, trigger: &str) -> bool {
    let tid = t.id.clone();
    let fresh = match db::get_task(conn, &tid) {
        Some(t2) => t2,
        None => return false,
    };
    if fresh.status != "ready" && fresh.status != "executing" {
        return false;
    }
    let (agent_id, agent) = match resolve_and_bind_role(state, conn, &fresh, "executor") {
        Some(v) => v,
        None => return false, // 池内暂无可派 agent（熔断/排队）→ 下一轮再试
    };
    // 绑定结果落库后重取快照，保证 worker 看到解析后的 agent（占用释放依赖它）
    let fresh = db::get_task(conn, &tid).unwrap_or(fresh);

    let attempt = fresh.attempt + 1;
    let _ = conn.execute(
        "UPDATE tasks SET status='executing', attempt=?2 WHERE id=?1",
        params![tid, attempt],
    );
    db::occupancy_add(conn, &agent_id, &tid);
    if let Some(a) = db::get_agent(conn, &agent_id) {
        events::send_agent(&state, &a);
    }
    events::push_event(
        state, conn, &fresh.project_id, Some(&tid), "agent.session_started",
        json!({"agent_id": agent_id, "phase": "execute", "attempt": attempt,
               "session_id": opencode::exec_session(&tid, attempt),
               "title": fresh.title,
               "purpose": format!("执行任务《{}》第 {} 轮", fresh.title, attempt),
               "trigger": trigger}),
    );
    let mut f = fresh.clone();
    f.status = "executing".into();
    f.attempt = attempt;
    events::status_changed(state, conn, &fresh, "executing", "dispatched");
    events::send_task(state, &f);

    let state2 = state.clone();
    let hid = state.alloc_handle_id();
    let h = tokio::spawn(async move { exec_worker(state2, f, agent, hid, ExecMode::Fresh).await });
    state.register_handle(&tid, hid, h);
    true
}

/// M4 计划阶段：派发执行者制定任务计划（ready → plan_drafting）。
fn dispatch_plan(state: &Arc<App>, conn: &Connection, t: &Task, trigger: &str) -> bool {
    let tid = t.id.clone();
    let fresh = match db::get_task(conn, &tid) {
        Some(t2) => t2,
        None => return false,
    };
    if fresh.status != "ready" && fresh.status != "plan_drafting" {
        return false;
    }
    let (agent_id, agent) = match resolve_and_bind_role(state, conn, &fresh, "executor") {
        Some(v) => v,
        None => return false,
    };
    let fresh = db::get_task(conn, &tid).unwrap_or(fresh);
    let attempt = fresh.attempt + 1;
    let _ = conn.execute(
        "UPDATE tasks SET status='plan_drafting', attempt=?2 WHERE id=?1",
        params![tid, attempt],
    );
    db::occupancy_add(conn, &agent_id, &tid);
    if let Some(a) = db::get_agent(conn, &agent_id) {
        events::send_agent(state, &a);
    }
    events::push_event(
        state, conn, &fresh.project_id, Some(&tid), "agent.session_started",
        json!({"agent_id": agent_id, "phase": "plan_task", "attempt": attempt,
               "session_id": opencode::task_plan_session(&tid, attempt),
               "title": fresh.title,
               "purpose": format!("制定任务计划《{}》第 {} 轮", fresh.title, attempt),
               "trigger": trigger}),
    );
    let mut f = fresh.clone();
    f.status = "plan_drafting".into();
    f.attempt = attempt;
    events::status_changed(state, conn, &fresh, "plan_drafting", "plan_drafting_started");
    events::send_task(state, &f);

    let state2 = state.clone();
    let hid = state.alloc_handle_id();
    let h = tokio::spawn(async move { plan_worker(state2, f, agent, hid).await });
    state.register_handle(&tid, hid, h);
    true
}

/// 计划阶段：派发验收者审查计划（plan_review 状态，占用验收者）。
fn dispatch_plan_review(state: &Arc<App>, conn: &Connection, t: &Task, _acc_id: &str) -> bool {
    let tid = t.id.clone();
    let (acc_owned, agent) = match resolve_and_bind_role(state, conn, t, "acceptor") {
        Some(v) => v,
        None => return false,
    };
    let acc_id = acc_owned.as_str();
    // 绑定结果落库后重取快照（worker 依赖其中的 acceptor_agent_id 释放占用）
    let t = &db::get_task(conn, &tid).unwrap_or_else(|| t.clone());
    db::occupancy_add(conn, acc_id, &tid);
    events::send_agent(state, &agent);
    events::push_event(
        state, conn, &t.project_id, Some(&tid), "agent.session_started",
        json!({"agent_id": acc_id, "phase": "plan_review", "attempt": t.attempt,
               "session_id": opencode::plan_review_session(&tid, t.attempt),
               "title": t.title,
               "purpose": format!("审查任务计划《{}》", t.title),
               "trigger": "scheduled"}),
    );
    let state2 = state.clone();
    let hid = state.alloc_handle_id();
    let spawn_task = t.clone();
    let h = tokio::spawn(async move { plan_review_worker(state2, spawn_task, agent, hid).await });
    state.register_handle(&tid, hid, h);
    true
}

/// 最近一次计划审查打回意见（用于修订计划）。
fn last_plan_feedback(conn: &Connection, task_id: &str) -> Option<String> {
    let mut stmt = conn.prepare(
        "SELECT payload FROM events WHERE task_id=?1 AND type='task.plan_rejected' ORDER BY ts DESC LIMIT 1",
    ).ok()?;
    let raw: String = stmt.query_row(params![task_id], |r| r.get(0)).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.get("comment").and_then(|c| c.as_str()).map(String::from)
}

async fn plan_worker(state: Arc<App>, task: Task, agent: Agent, hid: u64) {
    let guard = HandleGuard { state: state.clone(), task_id: task.id.clone(), id: hid };
    let executor_id = task.executor_agent_id.clone().unwrap_or_default();
    let feedback = {
        let c = state.db.lock().unwrap();
        last_plan_feedback(&c, &task.id)
    };
    let outcome = match agent.adapter.as_str() {
        "opencode" => opencode::run_task_plan(state.clone(), task.clone(), agent.clone(), feedback.as_deref()).await,
        _ => mock::run_task_plan(state.clone(), task.clone(), agent.clone(), feedback.as_deref()).await,
    };
    {
        let conn = state.db.lock().unwrap();
        let (detail, _) = match &outcome {
            Ok(p) => (format!("计划已产出（{} 字）", p.chars().count()), ()),
            Err(m) => (m.clone(), ()),
        };
        events::push_event(
            &state, &conn, &task.project_id, Some(&task.id), "agent.session_ended",
            json!({"agent_id": executor_id, "phase": "plan_task", "attempt": task.attempt,
                   "session_id": opencode::task_plan_session(&task.id, task.attempt),
                   "result": if outcome.is_ok() { "success" } else { "failed" },
                   "detail": detail}),
        );
    }
    {
        let conn = state.db.lock().unwrap();
        db::occupancy_remove(&conn, &executor_id, &task.id);
        if let Some(a) = db::get_agent(&conn, &executor_id) {
            events::send_agent(&state, &a);
        }
    }
    {
        let conn = state.db.lock().unwrap();
        let Some(fresh) = db::get_task(&conn, &task.id) else { return };
        if fresh.status != "plan_drafting" {
            return; // 人工干预/取消
        }
        let proj_status = db::get_project(&conn, &task.project_id).map(|p| p.status).unwrap_or_default();
        if proj_status == "archived" {
            return;
        }
        match outcome {
            Ok(plan) => {
                db::set_task_plan(&conn, &fresh.id, Some(&plan));
                if !fresh.acceptance_required {
                    // 免验收任务没有验收者，跳过计划审查直接进入执行
                    events::push_event(&state, &conn, &fresh.project_id, Some(&fresh.id), "task.plan_approved",
                        json!({"attempt": fresh.attempt, "note": "acceptance_required=false，跳过计划审查"}));
                    let _ = events::status_changed(&state, &conn, &fresh, "executing", "plan_approved_no_review");
                    drop(guard);
                    state.wake();
                    return;
                }
                let _ = conn.execute("UPDATE tasks SET status='plan_review' WHERE id=?1", params![fresh.id]);
                events::push_event(&state, &conn, &fresh.project_id, Some(&fresh.id), "task.plan_drafted",
                    json!({"attempt": fresh.attempt, "excerpt": plan.chars().take(400).collect::<String>()}));
                let mut f2 = fresh.clone();
                f2.status = "plan_review".into();
                f2.execution_plan = Some(plan);
                events::send_task(&state, &f2);
            }
            Err(msg) => {
                if handle_agent_failure(&state, &conn, &fresh, "executor", "plan", &msg) {
                    events::push_event(&state, &conn, &fresh.project_id, Some(&fresh.id), "task.plan_failed",
                        json!({"error": msg, "attempt": fresh.attempt, "failover": true}));
                } else {
                    let fc = fresh.failure_count + 1;
                    set_counts(&state, &conn, &fresh.id, "failure_count", fc);
                    let mut f2 = fresh;
                    f2.failure_count = fc;
                    events::push_event(&state, &conn, &f2.project_id, Some(&f2.id), "task.plan_failed",
                        json!({"error": msg, "attempt": f2.attempt, "failure_count": fc}));
                    if fc >= 3 {
                        suspend(&state, &conn, &f2, "plan");
                    }
                }
            }
        }
    }
    drop(guard);
    state.wake();
}

async fn plan_review_worker(state: Arc<App>, task: Task, agent: Agent, hid: u64) {
    let guard = HandleGuard { state: state.clone(), task_id: task.id.clone(), id: hid };
    let acceptor_id = task.acceptor_agent_id.clone().unwrap_or_default();
    let outcome = match agent.adapter.as_str() {
        "opencode" => opencode::run_plan_review(state.clone(), task.clone(), agent.clone()).await,
        _ => mock::run_plan_review(state.clone(), task.clone(), agent.clone()).await,
    };
    {
        let conn = state.db.lock().unwrap();
        events::push_event(
            &state, &conn, &task.project_id, Some(&task.id), "agent.session_ended",
            json!({"agent_id": acceptor_id, "phase": "plan_review", "attempt": task.attempt,
                   "session_id": opencode::plan_review_session(&task.id, task.attempt),
                   "result": if outcome.is_ok() { "pass" } else { "reject" },
                   "detail": match &outcome { Ok(()) => "计划审查通过".to_string(), Err(c) => c.clone() }}),
        );
    }
    {
        let conn = state.db.lock().unwrap();
        db::occupancy_remove(&conn, &acceptor_id, &task.id);
        if let Some(a) = db::get_agent(&conn, &acceptor_id) {
            events::send_agent(&state, &a);
        }
    }
    {
        let conn = state.db.lock().unwrap();
        let Some(fresh) = db::get_task(&conn, &task.id) else { return };
        if fresh.status != "plan_review" {
            return;
        }
        let proj_status = db::get_project(&conn, &task.project_id).map(|p| p.status).unwrap_or_default();
        if proj_status == "archived" {
            return;
        }
        match outcome {
            Ok(()) => {
                events::push_event(&state, &conn, &fresh.project_id, Some(&fresh.id), "task.plan_approved",
                    json!({"attempt": fresh.attempt, "reviewer": acceptor_id}));
                let _ = events::status_changed(&state, &conn, &fresh, "executing", "plan_approved");
            }
            Err(comment) => {
                if comment.starts_with(opencode::INFRA_ACCEPT_ERROR_PREFIX) {
                    if handle_agent_failure(&state, &conn, &fresh, "acceptor", "plan_review", &comment) {
                        events::push_event(&state, &conn, &fresh.project_id, Some(&fresh.id), "acceptance.run_failed",
                            json!({"error": comment, "attempt": fresh.attempt, "phase": "plan_review", "failover": true}));
                    } else {
                        let fc = fresh.failure_count + 1;
                        set_counts(&state, &conn, &fresh.id, "failure_count", fc);
                        let mut f2 = fresh;
                        f2.failure_count = fc;
                        events::push_event(&state, &conn, &f2.project_id, Some(&f2.id), "acceptance.run_failed",
                            json!({"error": comment, "attempt": f2.attempt, "failure_count": fc, "phase": "plan_review"}));
                        if fc >= 3 {
                            suspend(&state, &conn, &f2, "accept");
                        }
                    }
                } else {
                    let prc = fresh.plan_rejection_count + 1;
                    db::set_plan_rejection_count(&conn, &fresh.id, prc);
                    let mut f2 = fresh;
                    f2.plan_rejection_count = prc;
                    events::push_event(&state, &conn, &f2.project_id, Some(&f2.id), "task.plan_rejected",
                        json!({"attempt": f2.attempt, "comment": comment, "plan_rejection_count": prc}));
                    if prc >= 2 {
                        escalate(&state, &conn, &f2, "plan");
                    } else {
                        db::set_task_plan(&conn, &f2.id, None);
                        let _ = events::status_changed(&state, &conn, &f2, "plan_drafting", "plan_rejected_revise");
                    }
                }
            }
        }
    }
    drop(guard);
    state.wake();
}

/// M4 三档恢复（DESIGN §10）：为中断（或第三档挂起重启后）的任务推进**一档**恢复。
///
/// 返回 true = 已派发 worker（调用方 drop conn + wake + return）；
/// 返回 false = 本档被跳过（已落 `task.recovered` skipped 事件并切到下一档，下一轮
/// scan 继续）或派发守卫暂不满足（等下一轮 scan，不落事件、不切档）。
fn recover_dispatch(state: &Arc<App>, conn: &Connection, t: &Task) -> bool {
    let rec = db::recovery_state(conn, &t.id);
    if !rec.pending {
        return false;
    }
    let tier = if rec.tier == TIER_HANDOFF { TIER_HANDOFF } else { TIER_NATIVE };
    let (_, agent) = match resolve_and_bind_role(state, conn, t, "executor") {
        Some(v) => v,
        None => return false,
    };
    let t = &db::get_task(conn, &t.id).unwrap_or_else(|| t.clone());

    if tier == TIER_NATIVE {
        let stored = db::exec_resume_session(conn, &t.id, t.attempt);
        // 第一档判定（验收口径 2）：仅当 supports_resume==true **且**该 attempt 的真实
        // opencode 会话 id 已持久化时原生 resume；否则记 skipped 事件后切第二档。
        if !agent.capabilities.supports_resume {
            recovery::recovered(
                state, conn, t, TIER_NATIVE, recovery::OUTCOME_SKIPPED,
                &format!("agent '{}'未声明 supports_resume（capabilities.supports_resume=false），跳过第一档原生会话恢复", agent.name),
                stored.as_deref(),
                json!({"agent_id": agent.id}),
            );
            db::recovery_set(conn, &t.id, None, Some(TIER_HANDOFF), None);
            return false;
        }
        let Some(real) = stored else {
            recovery::recovered(
                state, conn, t, TIER_NATIVE, recovery::OUTCOME_SKIPPED,
                &format!("该 attempt（{}）没有持久化的 opencode 真实会话 id，无法原生 resume，跳过第一档", t.attempt),
                None,
                json!({"agent_id": agent.id}),
            );
            db::recovery_set(conn, &t.id, None, Some(TIER_HANDOFF), None);
            return false;
        };
        return dispatch_recover(state, conn, t, &agent, ExecMode::Resume { real_id: real });
    }

    // 第二档：交接摘要重建——同一 attempt、**新**会话（真实 id ≠ 原 id）+ 结构化交接书
    let no = recovery::handoff_count(conn, &t.id, t.attempt) + 1;
    let session = opencode::handoff_session(&t.id, t.attempt, no);
    let doc = recovery::build_handoff(conn, t, no);
    dispatch_recover(state, conn, t, &agent, ExecMode::Handoff { session, doc })
}

/// Commit a三档恢复 dispatch and spawn the worker。
/// 与 `dispatch_exec` 的关键差异：**attempt 不自增**——第一档沿用中断 attempt 与
/// 同一逻辑会话名 `exec_session(task_id, attempt)`（原生恢复落地证据），
/// 第二档在同一 attempt 上使用新交接会话名。
fn dispatch_recover(
    state: &Arc<App>,
    conn: &Connection,
    t: &Task,
    agent: &Agent,
    mode: ExecMode,
) -> bool {
    let fresh = match db::get_task(conn, &t.id) {
        Some(x) => x,
        None => return false,
    };
    // 恢复重入即结档旧的「已挂起待人工重建」条目（避免条目与实际状态不一致）
    recovery::mark_recovery_retry(state, conn, &fresh);
    // 执行中任务恢复续作；第三档人工挂起任务经三档重新进入流程
    if fresh.status != "executing" && fresh.status != "recovering" && fresh.status != "suspended" {
        return false;
    }
    if state.has_live_handle(&fresh.id) {
        return false; // 同一 task 永远只有一个在途执行 worker/句柄
    }
    let agent = agent.clone();
    let tier = mode.tier().unwrap_or(TIER_NATIVE);
    let session_name = mode.session_name(&fresh.id, fresh.attempt);
    let real = match &mode {
        ExecMode::Resume { real_id } => Some(real_id.clone()),
        _ => None,
    };
    let _ = conn.execute("UPDATE tasks SET status='executing' WHERE id=?1", params![fresh.id]);
    db::occupancy_add(conn, &agent.id, &fresh.id);
    if let Some(a) = db::get_agent(conn, &agent.id) {
        events::send_agent(state, &a);
    }
    events::push_event(
        state, conn, &fresh.project_id, Some(&fresh.id), "agent.session_started",
        json!({"agent_id": agent.id, "phase": "execute", "attempt": fresh.attempt,
               "session_id": session_name, "real_session_id": real,
               "title": fresh.title,
               "purpose": format!("恢复执行任务《{}》第 {} 轮（三档恢复 · {}）", fresh.title, fresh.attempt, tier),
               "trigger": "recovery", "recovery_tier": tier}),
    );
    if fresh.status != "executing" {
        // suspended → executing（事件时间线可读；同时清掉 recovery_manual 标记）
        events::status_changed(state, conn, &fresh, "executing", "recovery");
    } else {
        events::send_task(state, &fresh);
    }
    let state2 = state.clone();
    let hid = state.alloc_handle_id();
    let h = tokio::spawn(async move { exec_worker(state2, fresh, agent, hid, mode).await });
    state.register_handle(&t.id, hid, h);
    true
}

/// Commit an acceptance dispatch and spawn the worker (acceptor agent only;
/// human-only tasks are accepted/rejected via REST).
fn dispatch_accept(state: &Arc<App>, conn: &Connection, t: &Task, _acc_id: &str) {
    let tid = t.id.clone();
    let (acc_owned, agent) = match resolve_and_bind_role(state, conn, t, "acceptor") {
        Some(v) => v,
        None => return,
    };
    let acc_id = acc_owned.as_str();
    let t = &db::get_task(conn, &tid).unwrap_or_else(|| t.clone());
    db::occupancy_add(conn, acc_id, &tid);
    if let Some(a) = db::get_agent(conn, acc_id) {
        events::send_agent(&state, &a);
    }
    events::push_event(
        state, conn, &t.project_id, Some(&tid), "agent.session_started",
        json!({"agent_id": acc_id, "phase": "accept", "attempt": t.attempt,
               "session_id": opencode::accept_session(&tid, t.attempt),
               "title": t.title,
               "purpose": format!("验收任务《{}》第 {} 轮", t.title, t.attempt),
               "trigger": "scheduled"}),
    );
    let state2 = state.clone();
    let hid = state.alloc_handle_id();
    let spawn_task = t.clone();
    let h = tokio::spawn(async move { accept_worker(state2, spawn_task, agent, hid).await });
    state.register_handle(&tid, hid, h);
}

async fn exec_worker(state: Arc<App>, task: Task, agent: Agent, hid: u64, mode: ExecMode) {
    let guard = HandleGuard { state: state.clone(), task_id: task.id.clone(), id: hid };
    let executor_id = task.executor_agent_id.clone().unwrap_or_default();
    let logical_session = mode.session_name(&task.id, task.attempt);
    let outcome: Result<ExecOutcome, String> = match agent.adapter.as_str() {
        "opencode" => opencode::run_execution(state.clone(), task.clone(), agent.clone(), mode.clone()).await,
        _ => mock::run_execution(state.clone(), task.clone(), agent.clone(), mode.clone()).await,
    };
    // resume 档即使失败，审计也要带上「尝试续接的那个真实会话 id」
    let known_real = match &mode {
        ExecMode::Resume { real_id } => Some(real_id.clone()),
        _ => None,
    };
    let real_id = outcome.as_ref().ok().and_then(|o| o.session_id.clone()).or(known_real.clone());
    {
        let conn = state.db.lock().unwrap();
        let (detail, output) = match &outcome {
            Ok(o) => ("成功".to_string(), serde_json::to_value(&o.artifacts).unwrap_or(json!([]))),
            Err(m) => (m.clone(), serde_json::Value::Null),
        };
        events::push_event(
            &state, &conn, &task.project_id, Some(&task.id), "agent.session_ended",
            json!({"agent_id": executor_id, "phase": "execute", "attempt": task.attempt,
                   "session_id": logical_session, "real_session_id": real_id,
                   "result": if outcome.is_ok() { "success" } else { "failed" },
                   "detail": detail, "output": output}),
        );
    }
    {
        let conn = state.db.lock().unwrap();
        db::occupancy_remove(&conn, &executor_id, &task.id);
        if let Some(a) = db::get_agent(&conn, &executor_id) {
            events::send_agent(&state, &a);
        }
    }
    {
        let conn = state.db.lock().unwrap();
        let fresh = match db::get_task(&conn, &task.id) {
            Some(t) => t,
            None => return,
        };
        if fresh.status != "executing" {
            return; // cancelled or human-overridden meanwhile
        }
        let proj_status = db::get_project(&conn, &task.project_id)
            .map(|p| p.status)
            .unwrap_or_else(|| "archived".into());
        if proj_status == "archived" {
            return;
        }
        match outcome {
            Ok(o) => {
                // M4 三档恢复：本档成功 → `task.recovered`(succeeded)；
                // 若此前人工重建挂起过，同步关闭对应收件箱条目（不残留、不堆叠）。
                if let Some(tier) = mode.tier() {
                    let native_reason = recovery::last_tier_reason(&conn, &fresh.id, fresh.attempt, TIER_NATIVE);
                    let reason = match tier {
                        TIER_NATIVE => {
                            "第一档原生会话恢复成功：同一真实会话（--session 同一 id）续作，attempt 未自增".to_string()
                        }
                        _ => format!(
                            "第二档交接摘要重建成功：新会话续作（第一档 native_resume 结果：{}）",
                            native_reason.unwrap_or_else(|| "未尝试".into())
                        ),
                    };
                    let extra = match &mode {
                        ExecMode::Handoff { doc, .. } => json!({"handoff": doc.to_value()}),
                        _ => json!({}),
                    };
                    recovery::recovered(
                        &state, &conn, &fresh, tier, recovery::OUTCOME_SUCCEEDED,
                        &reason, o.session_id.as_deref(), extra,
                    );
                    db::recovery_set(&conn, &fresh.id, Some(false), None, Some(false));
                    recovery::close_recovery_inbox(&state, &conn, &fresh);
                } else {
                    db::recovery_set(&conn, &fresh.id, Some(false), None, None);
                }
                let mut artifacts = fresh.artifacts.clone();
                artifacts.extend(o.artifacts.clone());
                let _ = conn.execute(
                    "UPDATE tasks SET artifacts=?2 WHERE id=?1",
                    params![fresh.id, serde_json::to_string(&artifacts).unwrap()],
                );
                let mut f2 = fresh.clone();
                f2.artifacts = artifacts;
                events::push_event(
                    &state, &conn, &fresh.project_id, Some(&fresh.id), "task.execution_succeeded",
                    json!({"artifacts": o.artifacts, "attempt": fresh.attempt}),
                );
                if f2.acceptance_required {
                    // transition happens even when paused; the acceptance
                    // *dispatch* is gated on project being running.
                    let _ = events::status_changed(&state, &conn, &f2, "awaiting_acceptance", "execution_succeeded");
                } else {
                    let _ = events::status_changed(&state, &conn, &f2, "done", "execution_succeeded");
                }
            }
            Err(msg) => match &mode {
                // 常规派发：真实执行失败 → 失败计数 + 3 次挂起（既有语义不变）
                ExecMode::Fresh => {
                    // M4.5：先分类熔断 + 换下一优先级 agent（不计失败预算）
                    if handle_agent_failure(&state, &conn, &fresh, "executor", "execute", &msg) {
                        events::push_event(&state, &conn, &fresh.project_id, Some(&fresh.id), "task.execution_failed",
                            json!({"error": msg, "attempt": fresh.attempt, "failover": true,
                                   "note": "已换下一优先级 agent 重试，不计入失败预算"}));
                    } else {
                        let fc = fresh.failure_count + 1;
                        set_counts(&state, &conn, &fresh.id, "failure_count", fc);
                        let mut f2 = fresh;
                        f2.failure_count = fc;
                        events::push_event(
                            &state, &conn, &f2.project_id, Some(&f2.id), "task.execution_failed",
                            json!({"error": msg, "attempt": f2.attempt, "failure_count": fc,
                                   "note": "池内候选已耗尽，计入失败预算"}),
                        );
                        if fc >= 3 {
                            suspend(&state, &conn, &f2, "execute");
                        }
                    }
                    // otherwise status stays "executing": scan re-dispatches as fresh attempt
                }
                // 第一档原生 resume 失败（CLI 报错/会话不可用等）→ 记事件后降级第二档。
                // 不计入失败次数：恢复档失败按三档降级处理，不占用「失败 3 次挂起」额度。
                ExecMode::Resume { .. } => {
                    recovery::recovered(
                        &state, &conn, &fresh, TIER_NATIVE, recovery::OUTCOME_FAILED,
                        &msg, known_real.as_deref(), json!({"agent_id": executor_id}),
                    );
                    db::recovery_set(&conn, &fresh.id, Some(true), Some(TIER_HANDOFF), None);
                    events::log_line(
                        &state, &conn, &fresh.id,
                        &format!("第一档原生会话恢复（native_resume）失败：{msg}；降级到第二档交接摘要重建"),
                    );
                }
                // 第二档交接重建也失败 → 第三档人工重建：任务挂起 + 收件箱（附两档失败原因）
                ExecMode::Handoff { doc, .. } => {
                    let extra = json!({"handoff": doc.to_value(), "agent_id": executor_id});
                    recovery::recovered(
                        &state, &conn, &fresh, TIER_HANDOFF, recovery::OUTCOME_FAILED,
                        &msg, None, extra,
                    );
                    events::log_line(
                        &state, &conn, &fresh.id,
                        &format!("第二档交接摘要重建（handoff）失败：{msg}；进入第三档人工重建（任务挂起 + 收件箱）"),
                    );
                    recovery::suspend_manual(&state, &conn, &fresh);
                }
            },
        }
    }
    drop(guard);
    state.wake();
}

async fn accept_worker(state: Arc<App>, task: Task, agent: Agent, hid: u64) {
    let guard = HandleGuard { state: state.clone(), task_id: task.id.clone(), id: hid };
    let acceptor_id = task.acceptor_agent_id.clone().unwrap_or_default();
    // 机械检查先跑（可能执行命令，必须不持数据库锁）；结果作为验收证据
    {
        let project = {
            let c = state.db.lock().unwrap();
            db::get_project(&c, &task.project_id)
        };
        if let Some(p) = project {
            let _ = crate::verification::run(&state, &p, &task).await;
        }
    }
    let outcome = match agent.adapter.as_str() {
        "opencode" => opencode::run_acceptance(state.clone(), task.clone(), agent.clone()).await,
        _ => mock::run_acceptance(state.clone(), task.clone(), agent.clone()).await,
    };
    {
        let conn = state.db.lock().unwrap();
        let detail = match &outcome {
            Ok(()) => "验收通过".to_string(),
            Err(c) if c.starts_with(opencode::INFRA_ACCEPT_ERROR_PREFIX) => c.clone(),
            Err(c) => format!("验收退回：{c}"),
        };
        events::push_event(
            &state, &conn, &task.project_id, Some(&task.id), "agent.session_ended",
            json!({"agent_id": acceptor_id, "phase": "accept", "attempt": task.attempt,
                   "session_id": opencode::accept_session(&task.id, task.attempt),
                   "result": if outcome.is_ok() { "pass" } else { "reject" },
                   "detail": detail}),
        );
    }
    {
        let conn = state.db.lock().unwrap();
        db::occupancy_remove(&conn, &acceptor_id, &task.id);
        if let Some(a) = db::get_agent(&conn, &acceptor_id) {
            events::send_agent(&state, &a);
        }
    }
    // 先做快照与校验（短锁）；合入等长操作一律在锁外执行。
    let fresh = {
        let conn = state.db.lock().unwrap();
        let Some(t) = db::get_task(&conn, &task.id) else { return };
        if t.status != "awaiting_acceptance" {
            return; // cancelled / human already decided
        }
        let proj_status = db::get_project(&conn, &t.project_id)
            .map(|p| p.status)
            .unwrap_or_else(|| "archived".into());
        if proj_status == "archived" {
            return;
        }
        t
    };

    match outcome {
        Ok(()) => {
            {
                let conn = state.db.lock().unwrap();
                events::push_event(
                    &state, &conn, &fresh.project_id, Some(&fresh.id), "acceptance.recorded",
                    json!({"verdict": "approved", "by": "agent", "acceptor": acceptor_id, "comment": null}),
                );
            }
            let project = {
                let conn = state.db.lock().unwrap();
                db::get_project(&conn, &fresh.project_id)
            };
            // 合入与越权检测都要跑 git：必须在数据库锁之外
            let merge = match &project {
                Some(p) => crate::sandbox::merge_all(&state, p, &fresh, true),
                None => crate::sandbox::MergeResult::Ok(vec![]),
            };
            let violations = match &project {
                Some(p) => crate::sandbox::detect_violations(&state, p, &fresh),
                None => vec![],
            };

            let conn = state.db.lock().unwrap();
            let Some(fresh2) = db::get_task(&conn, &fresh.id) else { return };
            if fresh2.status != "awaiting_acceptance" {
                return; // 合入期间被人取消或接管
            }
            if !violations.is_empty() {
                events::new_inbox_item(&state, &conn, &fresh2.project_id, "escalation",
                    json!({"task_id": fresh2.id, "reason": "sandbox_violation",
                           "detail": format!("任务《{}》执行期间发现 scope 外源仓库被直接修改（绕过沙盒）", fresh2.title),
                           "violations": violations}));
            }
            match merge {
                crate::sandbox::MergeResult::Ok(merged_repos) => {
                    if !merged_repos.is_empty() {
                        events::push_event(&state, &conn, &fresh2.project_id, Some(&fresh2.id),
                            "task.merge_completed", json!({"repos": merged_repos}));
                    }
                    let _ = events::status_changed(&state, &conn, &fresh2, "done", "acceptance_passed");
                }
                crate::sandbox::MergeResult::Conflict { repo, detail } => {
                    // 设计：合并冲突一律交人，不再自动 rebase 重试
                    events::push_event(&state, &conn, &fresh2.project_id, Some(&fresh2.id),
                        "task.merge_conflict", json!({"repo": repo, "detail": detail}));
                    events::new_inbox_item(&state, &conn, &fresh2.project_id, "escalation",
                        json!({"task_id": fresh2.id, "reason": "merge_conflict",
                               "detail": format!("任务《{}》合入仓库 {} 时冲突，需人工处理（worktree 已保留）", fresh2.title, repo),
                               "repo": repo, "conflict": detail}));
                }
            }
        }
        Err(comment) => {
            let conn = state.db.lock().unwrap();
            let Some(fresh) = db::get_task(&conn, &fresh.id) else { return };
            // An infrastructure failure of the acceptor run (spawn error,
            // rate limit, timeout...) is NOT an acceptance verdict. Counting
            // it as a rejection used to escalate tasks to the human inbox
            // although no acceptor ever disagreed; those retries should be
            // handled like execution failures instead.
            if comment.starts_with(opencode::INFRA_ACCEPT_ERROR_PREFIX) {
                if handle_agent_failure(&state, &conn, &fresh, "acceptor", "accept", &comment) {
                    events::push_event(&state, &conn, &fresh.project_id, Some(&fresh.id), "acceptance.run_failed",
                        json!({"error": comment, "attempt": fresh.attempt, "failover": true}));
                } else {
                    let fc = fresh.failure_count + 1;
                    set_counts(&state, &conn, &fresh.id, "failure_count", fc);
                    let mut f2 = fresh;
                    f2.failure_count = fc;
                    events::push_event(
                        &state, &conn, &f2.project_id, Some(&f2.id), "acceptance.run_failed",
                        json!({"error": comment, "attempt": f2.attempt, "failure_count": fc}),
                    );
                    if fc >= 3 {
                        suspend(&state, &conn, &f2, "accept");
                    }
                }
                // otherwise status stays "awaiting_acceptance": the scan
                // re-dispatches the acceptor once it is free again,
                // mirroring the execution retry semantics.
            } else {
                let rc = fresh.rejection_count + 1;
                set_counts(&state, &conn, &fresh.id, "rejection_count", rc);
                let mut f2 = fresh;
                f2.rejection_count = rc;
                events::push_event(
                    &state, &conn, &f2.project_id, Some(&f2.id), "acceptance.rejected",
                    json!({"verdict": "rejected", "by": "agent", "acceptor": acceptor_id, "comment": comment}),
                );
                if rc >= 2 {
                    escalate(&state, &conn, &f2, "acceptance");
                } else {
                    let _ = events::status_changed(&state, &conn, &f2, "executing", "rejection_returned");
                }
            }
        }
    }
    drop(guard);
    state.wake();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tk(id: &str, milestone: Option<&str>, deps: &[&str]) -> Task {
        Task {
            id: id.into(),
            project_id: "prj1".into(),
            milestone_id: milestone.map(String::from),
            title: id.into(),
            description: String::new(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            assignee_role: "executor".into(),
            executor_agent_id: None,
            acceptor_agent_id: None,
            acceptance_required: true,
            acceptance_criteria: None,
            negotiation_status: "agreed".into(),
            negotiation_round: 0,
            execution_plan: None,
            plan_rejection_count: 0,
            scope_repos: Vec::new(),
            agent_pinned: false,
            tried_agents: serde_json::json!({}),
            status: "ready".into(),
            attempt: 0,
            rejection_count: 0,
            failure_count: 0,
            artifacts: vec![],
            created_at: 0,
        }
    }

    fn ms(id: &str, status: &str, members: &[&str]) -> Milestone {
        Milestone {
            id: id.into(),
            project_id: "prj1".into(),
            name: id.into(),
            goal: None,
            effect: None,
            task_ids: members.iter().map(|s| s.to_string()).collect(),
            status: status.into(),
        }
    }

    #[test]
    fn no_milestones_blocks_nothing() {
        let tasks = vec![tk("a", None, &[]), tk("b", None, &["a"])];
        assert!(blocked_by_gates(&tasks, &[]).is_empty());
    }

    #[test]
    fn gate_blocks_only_the_downstream_branch() {
        // m1 is awaiting confirmation; m2 holds the cross-milestone branch.
        let tasks = vec![
            tk("a", Some("m1"), &[]),
            tk("b", Some("m1"), &[]),
            tk("c", Some("m2"), &["a"]), // depends on gated milestone -> frozen
            tk("d", None, &[]),          // unrelated branch -> keeps running
            tk("e", Some("m2"), &["d"]), // downstream of the unrelated branch -> keeps running
            tk("f", None, &["c"]),       // transitive downstream of the gate -> frozen
        ];
        let milestones = vec![
            ms("m1", "awaiting_confirm", &["a", "b"]),
            ms("m2", "pending", &["c", "e"]),
        ];
        let blocked = blocked_by_gates(&tasks, &milestones);
        // members of the gated milestone are not themselves downstream
        assert!(!blocked.contains("a"));
        assert!(!blocked.contains("b"));
        // cross-milestone dependent + its transitive dependents are frozen
        assert!(blocked.contains("c"));
        assert!(blocked.contains("f"));
        // the unrelated branch is untouched
        assert!(!blocked.contains("d"));
        assert!(!blocked.contains("e"));
    }

    #[test]
    fn within_milestone_deps_keep_running() {
        let tasks = vec![
            tk("a", Some("m1"), &[]),
            tk("b", Some("m1"), &["a"]), // same milestone -> allowed
            tk("c", None, &["b"]),       // crosses out of m1 -> frozen
        ];
        let milestones = vec![ms("m1", "pending", &["a", "b"])];
        let blocked = blocked_by_gates(&tasks, &milestones);
        assert!(!blocked.contains("a"));
        assert!(!blocked.contains("b"));
        assert!(blocked.contains("c"));
    }

    #[test]
    fn passed_milestone_blocks_nothing() {
        let tasks = vec![tk("a", Some("m1"), &[]), tk("b", None, &["a"])];
        let milestones = vec![ms("m1", "passed", &["a"])];
        assert!(blocked_by_gates(&tasks, &milestones).is_empty());
    }

    #[test]
    fn pending_milestone_blocks_downstream_branch() {
        let tasks = vec![tk("a", Some("m1"), &[]), tk("b", None, &["a"])];
        let milestones = vec![ms("m1", "pending", &["a"])];
        let blocked = blocked_by_gates(&tasks, &milestones);
        assert!(blocked.contains("b"));
        assert!(!blocked.contains("a"));
    }

    #[test]
    fn failed_milestone_keeps_branch_frozen() {
        let tasks = vec![tk("a", Some("m1"), &[]), tk("b", None, &["a"])];
        let milestones = vec![ms("m1", "failed", &["a"])];
        let blocked = blocked_by_gates(&tasks, &milestones);
        assert!(blocked.contains("b"));
    }

    #[test]
    fn failed_milestone_regates_only_after_a_closed_window() {
        // window never opened (stale failed milestone) -> stays frozen
        assert!(!should_regate_failed_milestone(true, false, false));
        // window still awaiting human work -> stays frozen
        assert!(!should_regate_failed_milestone(true, true, true));
        // members not all done yet (tasks appended in the window still running)
        assert!(!should_regate_failed_milestone(false, true, false));
        // window worked & closed, everything done -> re-present the gate
        assert!(should_regate_failed_milestone(true, true, false));
    }
}
