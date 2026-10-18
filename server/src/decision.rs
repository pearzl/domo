//! M3 决策者（decision_maker）自动裁决通道（DESIGN §6 权限分层）。
//!
//! - **战术事项自决并留痕**：豁免裁定（acceptance_required=false 的任务请求）、
//!   验收标准协商失败裁决、不触及已确认里程碑/项目目标的任务级变更复核——
//!   由持有 decision_maker 角色的 agent 实例（mock / opencode 适配器）自动裁定，
//!   全程写入 `decision.requested` / `decision.ruled` 审计事件。
//! - **战略事项自动转人工**：触及已确认里程碑、项目目标、新增 agent 的请求
//!   （以及卡死升级）决策者不得自决，直接进决策收件箱等人裁决。
//! - **决策者故障降级**：实例缺失或连续运行失败 → 待办自动转入收件箱
//!   （`decision_maker_down`），相关分支挂起，但不阻塞无依赖的其他分支。
//! - **角色隔离**：决策者不得同时担任 executor/acceptor（api::validate_agent_roles
//!   与 agent 角色编辑共同校验；DOMO_DM_ROLE_OVERLAP=1 可显式打开例外）。

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::{
    agents::{mock, opencode},
    db, events,
    models::{Agent, Decision, Project, Task},
    state::{App, HandleGuard},
};

/// 决策者单条请求的最大尝试次数；超过则转 decision_maker_down 收件箱。
pub const MAX_DECISION_ATTEMPTS: i64 = 2;

/// DESIGN §6 角色隔离例外开关：BYO 池过小时显式允许决策者兼任执行/验收。
pub fn dm_role_overlap_allowed() -> bool {
    matches!(std::env::var("DOMO_DM_ROLE_OVERLAP"), Ok(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

pub fn is_decision_maker(a: &Agent) -> bool {
    a.roles.iter().any(|r| r == "decision_maker")
}

/// 角色隔离守卫：决策者实例不得持有任何非终态任务的 executor/acceptor 指派。
/// 返回 Some(违规说明) 表示必须禁止其担任裁决者。
pub fn dm_role_conflict(conn: &Connection, dm: &Agent) -> Option<String> {
    const TERMINAL: [&str; 2] = ["done", "cancelled"];
    for p in db::list_projects(conn) {
        for t in db::list_tasks(conn, &p.id) {
            if TERMINAL.contains(&t.status.as_str()) {
                continue;
            }
            let as_executor = t.executor_agent_id.as_deref() == Some(dm.id.as_str());
            let as_acceptor = t.acceptor_agent_id.as_deref() == Some(dm.id.as_str());
            if as_executor || as_acceptor {
                let role = if as_executor { "executor" } else { "acceptor" };
                return Some(format!(
                    "role isolation: decision maker '{}' ({}) is assigned as {role} on non-terminal task '{}' — \
                     unassign the role (inbox adjudication / task edit) before retrying \
                     (DESIGN §6; set DOMO_DM_ROLE_OVERLAP=1 to override)",
                    dm.name, dm.id, t.title
                ));
            }
        }
    }
    None
}

/// change_review / replan 请求的战略触发键：request 命中任一键即必须升级到人。
pub(crate) const STRATEGIC_KEYS: [&str; 9] = [
    "milestone_id",
    "milestone",
    "milestones",
    "add_milestone",
    "project",
    "project_name",
    "project_brief",
    "new_agent",
    "add_agent",
];

/// 任务级变更复核分类：(scope, reasons)。task 属于里程碑时同样视为战略事项。
pub fn classify_change(task: Option<&Task>, proposal: &Value) -> (String, Vec<String>) {
    let mut reasons: Vec<String> = Vec::new();
    if let Some(t) = task {
        if t.milestone_id.is_some() {
            reasons.push("task_belongs_to_milestone".into());
        }
    }
    for k in STRATEGIC_KEYS {
        match proposal.get(k) {
            None => {}
            Some(v) if v.is_null() => {}
            Some(_) => reasons.push(format!("touches:{k}")),
        }
    }
    if reasons.is_empty() {
        ("tactical".into(), reasons)
    } else {
        ("strategic".into(), reasons)
    }
}

/// 登记一条战术裁定请求（pending，等待调度器派发给决策者 agent）。
pub fn request(
    state: &Arc<App>,
    conn: &Connection,
    project_id: &str,
    task_id: Option<&str>,
    kind: &str,
    request: Value,
) -> Decision {
    request_scoped(state, conn, project_id, task_id, kind, "tactical", request)
}

/// 同 `request`，但可显式指定 scope：strategic 请求落库后由调用方立即升级到人。
pub fn request_scoped(
    state: &Arc<App>,
    conn: &Connection,
    project_id: &str,
    task_id: Option<&str>,
    kind: &str,
    scope: &str,
    request: Value,
) -> Decision {
    let d = Decision {
        id: db::new_id("dec_"),
        project_id: project_id.into(),
        task_id: task_id.map(String::from),
        kind: kind.into(),
        request: request.clone(),
        scope: scope.into(),
        status: "pending".into(),
        ruling: None,
        attempt: 0,
        created_at: db::now(),
        resolved_at: None,
    };
    db::insert_decision(conn, &d);
    events::push_event(
        state,
        conn,
        project_id,
        task_id,
        "decision.requested",
        json!({"decision_id": d.id, "kind": kind, "scope": scope, "request": request}),
    );
    d
}

/// 自愈补齐：任务处于豁免/协商失败状态但缺裁定请求时补建（幂等）。
pub fn ensure(state: &Arc<App>, conn: &Connection, t: &Task, kind: &str, req: Value) {
    if db::task_decision(conn, &t.id, kind).is_some() {
        return;
    }
    request(state, conn, &t.project_id, Some(&t.id), kind, req);
}

/// 战略事项：决策者不得自决，直接升级到人工决策收件箱（不派发 agent）。
pub fn escalate_to_human(
    state: &Arc<App>,
    conn: &Connection,
    d: &Decision,
    reasons: &[String],
    detail: &str,
) {
    let _ = conn.execute(
        "UPDATE decisions SET status='escalated', resolved_at=?2 WHERE id=?1",
        rusqlite::params![d.id, db::now()],
    );
    events::push_event(
        state,
        conn,
        &d.project_id,
        d.task_id.as_deref(),
        "decision.escalated",
        json!({"decision_id": d.id, "kind": d.kind, "reasons": reasons, "detail": detail}),
    );
    let inbox_kind = if d.kind.as_str() == "negotiation_failed" { "negotiation_failed" } else { "replan" };
    events::new_inbox_item(
        state,
        conn,
        &d.project_id,
        inbox_kind,
        json!({
            "decision_id": d.id, "kind": d.kind, "task_id": d.task_id,
            "reasons": reasons, "detail": detail, "request": d.request,
        }),
    );
}

/// 决策者实例缺失或运行故障：待办转入 decision_maker_down 收件箱，
/// 相关分支挂起（豁免/协商类任务停止开工），其他分支不受影响。
pub fn maker_down(state: &Arc<App>, conn: &Connection, d: &Decision, detail: &str) {
    if d.status == "ruled" {
        return;
    }
    let _ = conn.execute("UPDATE decisions SET status='down' WHERE id=?1", rusqlite::params![d.id]);
    events::push_event(
        state,
        conn,
        &d.project_id,
        d.task_id.as_deref(),
        "decision.maker_down",
        json!({"decision_id": d.id, "kind": d.kind, "task_id": d.task_id, "detail": detail}),
    );
    // 豁免/协商裁定直接决定任务能否开工 → 任务挂起（下游依赖随之等待，
    // 无依赖分支继续跑）；变更复核只阻塞「变更」本身，任务按原规划继续。
    if matches!(d.kind.as_str(), "exemption" | "negotiation_failed") {
        if let Some(t) = d.task_id.as_ref().and_then(|id| db::get_task(conn, id)) {
            if t.status == "planned" || t.status == "ready" {
                events::status_changed(state, conn, &t, "suspended", "decision_maker_down");
            }
        }
    }
    events::new_inbox_item(
        state,
        conn,
        &d.project_id,
        "decision_maker_down",
        json!({
            "decision_id": d.id, "kind": d.kind, "task_id": d.task_id,
            "detail": detail, "request": d.request,
        }),
    );
}

#[derive(Debug, Clone)]
pub struct Ruling {
    pub approve: bool,
    pub rationale: String,
    pub criteria: Option<String>,
    /// M4：merge_conflict 裁定的机械动作（rebase | rework | manual）
    pub action: Option<String>,
}

/// 裁定落地：更新任务字段、写审计事件、解除因 decision_maker_down 被挂起的任务。
/// agent 与人工裁决共用此路径（by 区分来源）。
pub fn apply_ruling(
    state: &Arc<App>,
    conn: &Connection,
    d: &Decision,
    r: &Ruling,
    by: &str,
    agent_id: Option<&str>,
) -> Result<(), String> {
    let approve = if r.approve { "approve" } else { "deny" };
    let effect: Value;
    match d.kind.as_str() {
        "exemption" => {
            let t = task_of(conn, d)?;
            if r.approve {
                let _ = conn.execute(
                    "UPDATE tasks SET acceptance_required=0 WHERE id=?1",
                    rusqlite::params![t.id],
                );
                effect = json!({"effect": "exemption_granted", "acceptance_required": false});
            } else {
                let _ = conn.execute(
                    "UPDATE tasks SET acceptance_required=1, negotiation_status='agreed' WHERE id=?1",
                    rusqlite::params![t.id],
                );
                effect = json!({"effect": "exemption_denied", "acceptance_required": true});
            }
            let fresh = db::get_task(conn, &t.id).ok_or("task disappeared")?;
            events::send_task(state, &fresh);
            unblock(state, conn, &fresh);
        }
        "negotiation_failed" => {
            let t = task_of(conn, d)?;
            let proposed = d
                .request
                .get("proposed_criteria")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);
            let criteria = r
                .criteria
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or(proposed)
                .unwrap_or_else(|| {
                    if r.approve {
                        "（决策者裁定）按任务描述与产物是否达成目标判断".into()
                    } else {
                        "（决策者否决原协商方案，采用兜底标准）按任务描述与产物是否达成目标判断".into()
                    }
                });
            let _ = conn.execute(
                "UPDATE tasks SET negotiation_status='agreed', acceptance_required=1, acceptance_criteria=?2 WHERE id=?1",
                rusqlite::params![t.id, criteria],
            );
            effect = json!({"effect": "negotiation_settled", "acceptance_criteria": criteria});
            events::push_event(
                state,
                conn,
                &t.project_id,
                Some(&t.id),
                "negotiation.settled",
                json!({"by": by, "agent_id": agent_id, "decision_id": d.id,
                       "acceptance_criteria": criteria,
                       "rationale": r.rationale}),
            );
            let fresh = db::get_task(conn, &t.id).ok_or("task disappeared")?;
            events::send_task(state, &fresh);
            unblock(state, conn, &fresh);
        }
        "change_review" => {
            if !r.approve {
                effect = json!({"effect": "change_rejected"});
            } else if d.scope == "strategic" {
                // 战略变更（触及里程碑/项目目标/新增 agent/无任务归属）不自动落地：
                // 裁定只记录「人已批准」，具体改动由负责人按 request 手工执行。
                effect = json!({
                    "effect": "strategic_change_approved",
                    "applied": false,
                    "note": "战略事项须由人工按 request 落实（自动路径不得改动已确认里程碑与项目目标）",
                });
            } else {
                let applied = apply_change(state, conn, d)?;
                effect = json!({"effect": "change_applied", "fields": applied});
            }
        }
        "merge_conflict" => {
            // M4：合并冲突——决策者先试（机械动作），失败/人为则转人工
            let t = task_of(conn, d)?;
            let action = r.action.clone().unwrap_or_else(|| if r.approve { "rebase".into() } else { "manual".into() });
            let proj = db::get_project(conn, &t.project_id);
            match (action.as_str(), proj) {
                ("rebase", Some(p)) => {
                    match crate::sandbox::retry_merge_with_rebase(state, conn, &p, &t) {
                        crate::sandbox::MergeResult::Ok(_) => {
                            effect = json!({"effect": "merge_conflict_resolved_by_rebase"});
                            let _ = events::status_changed(state, conn, &t, "done", "merge_conflict_resolved");
                        }
                        crate::sandbox::MergeResult::Conflict { repo, detail } => {
                            effect = json!({"effect": "merge_conflict_retry_failed", "repo": repo, "detail": detail});
                            escalate_to_human(state, conn, d, &["merge_conflict_retry_failed".into()],
                                &format!("决策者重试合并仍冲突（{repo}），转人工处理"));
                            let _ = events::status_changed(state, conn, &t, "suspended", "merge_conflict_manual");
                        }
                    }
                }
                ("rework", _) => {
                    effect = json!({"effect": "merge_conflict_rework"});
                    crate::api::add_guidance_internal(conn, &t.id,
                        "与主分支存在合并冲突：请先把主分支最新改动合并进你的工作区、解决冲突并验证，然后继续任务。");
                    let _ = events::status_changed(state, conn, &t, "executing", "merge_conflict_rework");
                }
                _ => {
                    effect = json!({"effect": "merge_conflict_manual"});
                    escalate_to_human(state, conn, d, &["merge_conflict_needs_human".into()],
                        &format!("任务《{}》合并冲突需人工处理（仓库 {}）", t.title, d.request["repo"]));
                    let _ = events::status_changed(state, conn, &t, "suspended", "merge_conflict_manual");
                }
            }
        }
        "replan" => {
            // 执行中重规划（DESIGN §8 常规纠偏）：批准只到「进入规划」这一步，
            // 真正的任务/依赖调整由 replan::scan 派发 planner 产出并落地
            // （新任务默认带验收、插入 DAG 做环校验、复用规划会话 memory）。
            if r.approve {
                effect = json!({"effect": "replan_approved", "planning": "pending"});
            } else {
                // 拒绝：关闭请求并通知上报者，原任务流程保持不动
                effect = json!({"effect": "replan_rejected"});
                notify_replan_reporter(state, conn, d, &r.rationale);
            }
        }
        other => return Err(format!("unknown decision kind: {other}")),
    }
    let ruling = json!({
        "decision": approve,
        "rationale": r.rationale,
        "by": by,
        "agent_id": agent_id,
        "effect": effect,
    });
    let _ = conn.execute(
        "UPDATE decisions SET status='ruled', ruling=?2, resolved_at=?3 WHERE id=?1",
        rusqlite::params![d.id, ruling.to_string(), db::now()],
    );
    events::push_event(
        state,
        conn,
        &d.project_id,
        d.task_id.as_deref(),
        "decision.ruled",
        json!({"decision_id": d.id, "kind": d.kind, "decision": approve,
               "rationale": r.rationale, "by": by, "agent_id": agent_id, "effect": effect}),
    );
    // 重规划通道：审批结果单独留痕（replan.approved / replan.rejected），
    // 与 decision.ruled 并存，审计页可据此回放完整链路。
    if d.kind == "replan" {
        let ev = if r.approve { "replan.approved" } else { "replan.rejected" };
        events::push_event(
            state,
            conn,
            &d.project_id,
            d.task_id.as_deref(),
            ev,
            json!({
                "decision_id": d.id,
                "by": by,
                "agent_id": agent_id,
                "rationale": r.rationale,
                "reported_by": d.request["reported_by"],
                "reason": d.request["reason"],
                "planning": if r.approve { json!("pending") } else { Value::Null },
            }),
        );
    }
    close_decision_inbox(state, conn, d, by);
    Ok(())
}

/// 重规划请求被拒绝：通知上报者（写入该任务日志，随任务时间线/审计可见），
/// 不改变任务状态，也不生成任何新任务。
fn notify_replan_reporter(state: &Arc<App>, conn: &Connection, d: &Decision, rationale: &str) {
    let Some(tid) = d.task_id.as_deref() else { return };
    if db::get_task(conn, tid).is_none() {
        return;
    }
    let by = d.request["reported_by"].as_str().unwrap_or("executor");
    let reason = d.request["reason"].as_str().unwrap_or("");
    events::log_line(
        state,
        conn,
        tid,
        &format!("重规划请求被决策者拒绝（{by} 上报 · {reason}）：{rationale}；原任务流程不受影响"),
    );
}

/// 裁定落地后，自动关闭同一决策尚未处理的收件箱条目（战略升级 / 决策者故障），
/// 避免「已裁定但仍挂在收件箱」的陈旧条目。
fn close_decision_inbox(state: &Arc<App>, conn: &Connection, d: &Decision, by: &str) {
    for item in db::list_inbox(conn) {
        if item.status != "open" {
            continue;
        }
        if item.payload.get("decision_id").and_then(|v| v.as_str()) != Some(d.id.as_str()) {
            continue;
        }
        let res = json!({"decision_id": d.id, "decision": "auto_closed", "by": by});
        let _ = conn.execute(
            "UPDATE inbox SET status='resolved', resolution=?2 WHERE id=?1",
            rusqlite::params![item.id, res.to_string()],
        );
        events::push_event(
            state,
            conn,
            &d.project_id,
            None,
            "decision.inbox_closed",
            json!({"inbox_id": item.id, "decision_id": d.id, "by": by}),
        );
        if let Some(fresh) = db::get_inbox(conn, &item.id) {
            events::send_inbox(state, &fresh);
        }
    }
}

fn task_of(conn: &Connection, d: &Decision) -> Result<Task, String> {
    d.task_id
        .as_ref()
        .and_then(|id| db::get_task(conn, id))
        .ok_or_else(|| format!("decision {} has no valid task", d.id))
}

/// 变更复核通过后落到任务上（仅任务级字段；战略请求不会走到这里）。
fn apply_change(state: &Arc<App>, conn: &Connection, d: &Decision) -> Result<Vec<String>, String> {
    let mut t = task_of(conn, d)?;
    if t.status == "done" || t.status == "cancelled" {
        return Err(format!("task '{}' is terminal ({})", t.title, t.status));
    }
    let f = &d.request["proposal"];
    let mut applied: Vec<String> = Vec::new();
    if let Some(s) = f["title"].as_str().filter(|s| !s.trim().is_empty()) {
        t.title = s.trim().into();
        applied.push("title".into());
    }
    if let Some(s) = f["description"].as_str() {
        t.description = s.into();
        applied.push("description".into());
    }
    if let Some(s) = f["acceptance_criteria"].as_str() {
        t.acceptance_criteria = Some(s.into());
        applied.push("acceptance_criteria".into());
    }
    if !f["depends_on"].is_null() {
        let arr = f["depends_on"]
            .as_array()
            .ok_or("proposal.depends_on must be an array of task ids")?;
        let mut ids: Vec<String> = Vec::new();
        for v in arr {
            let id = v.as_str().ok_or("proposal.depends_on must contain task ids")?.to_string();
            let dep = db::get_task(conn, &id).ok_or_else(|| format!("unknown dependency task: {id}"))?;
            if dep.project_id != t.project_id {
                return Err(format!("dependency {id} belongs to another project"));
            }
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if db::deps_introduce_cycle(conn, &t.project_id, Some(&t.id), &ids) {
            return Err("depends_on would introduce a dependency cycle".into());
        }
        t.depends_on = ids;
        applied.push("depends_on".into());
    }
    if applied.is_empty() {
        return Err(
            "proposal contains no applicable task-level fields (allowed: title/description/depends_on/acceptance_criteria)".into(),
        );
    }
    let _ = conn.execute(
        "UPDATE tasks SET title=?2, description=?3, depends_on=?4, acceptance_criteria=?5 WHERE id=?1",
        rusqlite::params![
            t.id,
            t.title,
            t.description,
            db::jstring(&Value::Array(t.depends_on.iter().map(|x| Value::String(x.clone())).collect())),
            t.acceptance_criteria
        ],
    );
    events::push_event(
        state,
        conn,
        &t.project_id,
        Some(&t.id),
        "task.updated",
        json!({"source": "decision", "decision_id": d.id, "fields": applied}),
    );
    events::send_task(state, &t);
    Ok(applied)
}

/// 裁定落地后，解除因 decision_maker_down 被挂起的任务（其他原因的挂起不动）。
fn unblock(state: &Arc<App>, conn: &Connection, t: &Task) {
    if t.status == "suspended" && suspended_by_dm_down(conn, &t.id) {
        events::status_changed(state, conn, t, "ready", "decision_ruled");
    }
}

/// 该任务最近一次状态流转是否是 decision_maker_down 挂起。
fn suspended_by_dm_down(conn: &Connection, task_id: &str) -> bool {
    let raw: Option<String> = conn
        .query_row(
            "SELECT payload FROM events WHERE task_id=?1 AND type='task.status_changed' ORDER BY id DESC LIMIT 1",
            rusqlite::params![task_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    raw.and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|p| p["to"] == "suspended" && p["reason"] == "decision_maker_down")
        .unwrap_or(false)
}

/// 人工裁决（收件箱 approve/deny）：与 agent 裁定走同一落地逻辑，by=human。
pub fn human_rule(
    state: &Arc<App>,
    conn: &Connection,
    decision_id: &str,
    approve: bool,
    note: &str,
    criteria: Option<&str>,
) -> Result<(), String> {
    let d = db::get_decision(conn, decision_id).ok_or("decision not found")?;
    if d.status == "ruled" {
        return Err("decision already ruled".into());
    }
    let rationale = if note.trim().is_empty() {
        "由负责人在决策收件箱人工裁决".into()
    } else {
        format!("（人工裁决）{}", note.trim())
    };
    let r = Ruling { approve, rationale, criteria: criteria.map(String::from), action: None };
    apply_ruling(state, conn, &d, &r, "human", None)
}

/// planned → ready 闸门：协商一致（`negotiation_status=agreed`）或豁免已获裁定才允许开工。
/// Some(reason) 可转 ready；None 表示仍被协商 / 裁定请求挂起。
pub fn planned_gate(conn: &Connection, t: &Task) -> Option<&'static str> {
    if t.acceptance_required {
        // M4 流程：验收标准由规划者给出并随确认锁定，开工前无需协商（协商机制已退场；
        // 历史协商任务在新流程清洗后不再出现）。
        return Some("criteria_fixed_by_planner");
    }
    let d = db::task_decision(conn, &t.id, "exemption")?;
    let approved = d.status == "ruled"
        && d
            .ruling
            .as_ref()
            .and_then(|r| r.get("decision"))
            .and_then(|v| v.as_str())
            == Some("approve");
    approved.then_some("exemption_granted_by_decision_maker")
}

/// 调度器决策扫描：自愈补齐裁定请求 → 无决策者转收件箱 → 派发裁定 worker。
pub fn scan(state: &Arc<App>) {
    let conn = state.db.lock().unwrap();
    let projects: Vec<Project> = db::list_projects(&conn)
        .into_iter()
        .filter(|p| matches!(p.status.as_str(), "initializing" | "pending_confirm" | "running"))
        .collect();
    for p in &projects {
        for t in db::list_tasks(&conn, &p.id) {
            if t.status != "planned" {
                continue;
            }
            if !t.acceptance_required {
                ensure(
                    state,
                    &conn,
                    &t,
                    "exemption",
                    json!({"reason": "task requests acceptance exemption (acceptance_required=false)"}),
                );
            }
            if t.negotiation_status == "failed" {
                ensure(
                    state,
                    &conn,
                    &t,
                    "negotiation_failed",
                    json!({"reason": "acceptance criteria negotiation failed"}),
                );
            }
        }
        let pending: Vec<Decision> = db::list_decisions(&conn, Some(&p.id))
            .into_iter()
            .filter(|d| d.status == "pending")
            .collect();
        for d in pending {
            if state.has_live_handle(&d.id) {
                continue;
            }
            match db::find_decision_maker(&conn) {
                None => maker_down(
                    state,
                    &conn,
                    &d,
                    "no agent with the decision_maker role is registered (removed or never added)",
                ),
                Some(dm) => {
                    // 角色隔离守卫（DESIGN §6）：决策者实例若正持有任何非终态任务的
                    // executor/acceptor 指派（历史数据或 DOMO_DM_ROLE_OVERLAP 例外下产生），
                    // 则不能作为裁决者派发 → 转 decision_maker_down，人工解除违规指派后 retry。
                    if !dm_role_overlap_allowed() {
                        if let Some(reason) = dm_role_conflict(&conn, &dm) {
                            maker_down(state, &conn, &d, &reason);
                            continue;
                        }
                    }
                    if !db::agent_free(&conn, &dm.id) {
                        continue; // 决策者忙碌：下一轮再试
                    }
                    if dispatch(state, &conn, &d, &dm) {
                        drop(conn);
                        state.wake();
                        return;
                    }
                }
            }
        }
    }
}

fn dispatch(state: &Arc<App>, conn: &Connection, d: &Decision, dm: &Agent) -> bool {
    let attempt = d.attempt + 1;
    let _ = conn.execute(
        "UPDATE decisions SET attempt=?2 WHERE id=?1",
        rusqlite::params![d.id, attempt],
    );
    db::occupancy_add(conn, &dm.id, &d.id);
    if let Some(a) = db::get_agent(conn, &dm.id) {
        events::send_agent(state, &a);
    }
    events::push_event(
        state,
        conn,
        &d.project_id,
        d.task_id.as_deref(),
        "agent.session_started",
        json!({"agent_id": dm.id, "phase": "decide", "decision_id": d.id, "kind": d.kind,
               "attempt": attempt, "session_id": opencode::decide_session(&d.id),
               "purpose": format!("决策者裁定：{}（决策 {}）", d.kind, d.id),
               "trigger": "scheduled"}),
    );
    let mut d2 = d.clone();
    d2.attempt = attempt;
    let state2 = state.clone();
    let dm2 = dm.clone();
    let hid = state.alloc_handle_id();
    let h = tokio::spawn(async move { worker(state2, d2, dm2, hid).await });
    state.register_handle(&d.id, hid, h);
    true
}

async fn worker(state: Arc<App>, d: Decision, dm: Agent, hid: u64) {
    let guard = HandleGuard { state: state.clone(), task_id: d.id.clone(), id: hid };
    let task = d.task_id.clone().and_then(|tid| {
        let c = state.db.lock().unwrap();
        db::get_task(&c, &tid)
    });
    let outcome = match dm.adapter.as_str() {
        "opencode" => opencode::run_adjudication(state.clone(), task, d.clone(), dm.clone()).await,
        _ => mock::run_adjudication(state.clone(), task, d.clone(), dm.clone()).await,
    };
    let conn = state.db.lock().unwrap();
    db::occupancy_remove(&conn, &dm.id, &d.id);
    if let Some(a) = db::get_agent(&conn, &dm.id) {
        events::send_agent(&state, &a);
    }
    events::push_event(
        &state,
        &conn,
        &d.project_id,
        d.task_id.as_deref(),
        "agent.session_ended",
        json!({"agent_id": dm.id, "phase": "decide", "decision_id": d.id,
               "session_id": opencode::decide_session(&d.id),
               "result": if outcome.is_ok() { "ruled" } else { "failed" },
               "detail": match &outcome { Ok(_) => "已作出裁定".to_string(), Err(e) => e.clone() }}),
    );
    let proj_status = db::get_project(&conn, &d.project_id)
        .map(|p| p.status)
        .unwrap_or_default();
    if let Some(fresh) = db::get_decision(&conn, &d.id) {
        if fresh.status == "pending" && proj_status != "archived" {
            match outcome {
                Ok(ruling) => {
                    if let Err(e) = apply_ruling(&state, &conn, &fresh, &ruling, "agent", Some(&dm.id)) {
                        tracing::warn!(decision = %fresh.id, "apply ruling failed: {e}");
                        failed(&state, &conn, &fresh, &format!("apply ruling failed: {e}"));
                    }
                }
                Err(e) => failed(&state, &conn, &fresh, &e),
            }
        }
    }
    drop(conn);
    drop(guard);
    state.wake();
}

/// 裁定运行失败：未达上限留在 pending 下轮重试；达上限转 decision_maker_down。
fn failed(state: &Arc<App>, conn: &Connection, d: &Decision, err: &str) {
    if d.attempt >= MAX_DECISION_ATTEMPTS {
        maker_down(
            state,
            conn,
            d,
            &format!("decision maker run failed {MAX_DECISION_ATTEMPTS} times: {err}"),
        );
    } else {
        events::push_event(
            state,
            conn,
            &d.project_id,
            d.task_id.as_deref(),
            "decision.retry_scheduled",
            json!({"decision_id": d.id, "kind": d.kind, "attempt": d.attempt, "error": err}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_with_milestone() -> Task {
        Task {
            id: "tsk_1".into(),
            project_id: "prj_1".into(),
            milestone_id: Some("ms_1".into()),
            title: "t".into(),
            description: String::new(),
            depends_on: vec![],
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
            status: "planned".into(),
            attempt: 0,
            rejection_count: 0,
            failure_count: 0,
            artifacts: vec![],
            created_at: 0,
        }
    }

    /// Task NOT attached to any milestone: plain task-level fields stay tactical.
    fn free_task() -> Task {
        let mut t = task_with_milestone();
        t.milestone_id = None;
        t
    }

    #[test]
    fn plain_task_fields_are_tactical() {
        let t = free_task();
        let (scope, reasons) = classify_change(Some(&t), &json!({"title": "x", "description": "y"}));
        assert_eq!(scope, "tactical", "no milestone/goal keys → tactical: {reasons:?}");
        assert!(reasons.is_empty());
    }

    #[test]
    fn milestone_refs_are_strategic() {
        let t = task_with_milestone();
        let (scope, reasons) = classify_change(Some(&t), &json!({"description": "y"}));
        assert_eq!(scope, "strategic");
        assert!(reasons.contains(&"task_belongs_to_milestone".to_string()));

        let (_, reasons) = classify_change(None, &json!({"milestone_id": "ms_9"}));
        assert!(reasons.contains(&"touches:milestone_id".to_string()));
    }

    #[test]
    fn project_goal_and_new_agent_are_strategic() {
        let (scope, reasons) = classify_change(None, &json!({"project": {"brief": "改目标"}}));
        assert_eq!(scope, "strategic");
        assert!(reasons.contains(&"touches:project".to_string()));

        let (scope, _) = classify_change(None, &json!({"new_agent": {"name": "x"}}));
        assert_eq!(scope, "strategic");

        // null 值不触发战略升级（前端常带 null 键）
        let (scope, _) = classify_change(None, &json!({"milestone_id": null, "new_agent": null}));
        assert_eq!(scope, "tactical");
    }

    #[test]
    fn planned_gate_blocks_pending_exemption() {
        let mut t = free_task();
        t.acceptance_required = false;
        // 无裁定请求 → 阻塞
        let dir = std::env::temp_dir().join(format!("domo_gate_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let conn = crate::db::open(dir.join("t.db").to_str().unwrap());
        conn.execute(
            "INSERT INTO tasks (id,project_id,milestone_id,title,description,depends_on,assignee_role,executor_agent_id,acceptor_agent_id,acceptance_required,acceptance_criteria,negotiation_status,status,attempt,rejection_count,failure_count,artifacts,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,NULL,?8,?9,?10,?11,0,0,0,'[]',0)",
            rusqlite::params![
                t.id,
                t.project_id,
                t.milestone_id,
                t.title,
                t.description,
                "[]",
                t.assignee_role,
                t.acceptance_required,
                t.acceptance_criteria,
                t.negotiation_status,
                t.status
            ],
        )
        .unwrap();
        assert_eq!(planned_gate(&conn, &t), None, "exemption without ruling must block");
        // 建立请求（pending）→ 仍阻塞
        let st = std::sync::Arc::new(App {
            db: std::sync::Mutex::new(crate::db::open(":memory:")),
            tx: tokio::sync::broadcast::channel(1).0,
            handles: Default::default(),
            handle_seq: Default::default(),
            wake: Default::default(),
            sessions: Default::default(),
        });
        request(&st, &conn, &t.project_id, Some(&t.id), "exemption", json!({}));
        assert_eq!(planned_gate(&conn, &t), None, "pending exemption must block");
        // 裁定通过 → 放行
        let d = crate::db::task_decision(&conn, &t.id, "exemption").unwrap();
        let r = Ruling { approve: true, rationale: "简单任务".into(), criteria: None, action: None };
        apply_ruling(&st, &conn, &d, &r, "agent", Some("agt_x")).unwrap();
        assert_eq!(planned_gate(&conn, &t), Some("exemption_granted_by_decision_maker"));
        // M4：需验收的任务不再依赖协商，验收标准由规划者固定 → 直接放行
        t.acceptance_required = true;
        assert_eq!(planned_gate(&conn, &t), Some("criteria_fixed_by_planner"));
    }
}
