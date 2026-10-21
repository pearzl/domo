//! 动态重规划：agent / 人上报规划问题 → 人工收件箱 → 批准后规划并落地。
//!
//! 目标设计没有决策者 agent：所有批准/否决都由人完成。
//! - 上报：executor/acceptor 的 `REPLAN:` 块或 `POST /tasks/:id/replan` 创建一条
//!   `kind=replan` 的收件箱条目（payload.kind=`replan_report`，含请求与建议变更）。
//! - 批准：收件箱把 payload 置为 `approved=true, planning=pending`，`scan` 派发规划 worker；
//! - 规划：有真实 planner 时复用 `planner-<prj_id>` 会话，否则采用上报者建议，再否则生成
//!   一条带验收的兜底补充任务；
//! - 落地：`api::apply_replan_changes` 逐项应用（新任务带验收、DAG 环校验、单项失败跳过）；
//!   完成后写 `replan.applied` 并把收件箱 payload 的 planning 置为 applied，可跨重启续跑。
//! - 否决：关闭条目、写 `replan.rejected`，原任务流程不动。

use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{
    agents::opencode,
    api, db, events,
    models::{InboxItem, Task},
    state::{App, HandleGuard},
};

/// 递归查找对象键：命中任一战略键（且值非 null）即视为触及里程碑/项目目标。
fn touches_strategic_key(v: &Value) -> bool {
    match v {
        Value::Object(map) => map.iter().any(|(k, val)| {
            (STRATEGIC_KEYS.contains(&k.as_str()) && !val.is_null()) || touches_strategic_key(val)
        }),
        Value::Array(arr) => arr.iter().any(touches_strategic_key),
        _ => false,
    }
}

/// 仍保留的分类，仅用于收件箱上下文展示（不再决定由谁裁决）。
pub const STRATEGIC_KEYS: [&str; 8] = [
    "milestone_id",
    "milestone",
    "milestones",
    "project_goal",
    "goal",
    "brief",
    "new_agent",
    "project",
];

pub fn classify(task: Option<&Task>, request: &Value) -> (String, Vec<String>) {
    let mut reasons: Vec<String> = Vec::new();
    if let Some(t) = task {
        if t.milestone_id.is_some() {
            reasons.push("task_belongs_to_milestone".into());
        }
    }
    if touches_strategic_key(request) {
        reasons.push("touches:milestone_or_project_goal".into());
    }
    if reasons.is_empty() {
        ("tactical".into(), reasons)
    } else {
        ("strategic".into(), reasons)
    }
}

fn is_report(i: &InboxItem) -> bool {
    i.kind == "replan"
        && (i.payload.get("kind").and_then(|v| v.as_str()) == Some("replan_report")
            || (i.payload.get("task_id").is_some()
                && i.payload.get("milestone_id").is_none()
                && i.payload.get("downstream").is_none()
                && i.payload.get("abandoned_task_id").is_none()))
}

/// 登记一条重规划请求（同一任务的 open 请求幂等复用）。
pub fn open_request(
    st: &Arc<App>,
    c: &Connection,
    project_id: &str,
    task: &Task,
    request: Value,
    scope: &str,
) -> (InboxItem, bool) {
    if let Some(existing) = db::list_inbox(c).into_iter().find(|i| {
        i.status == "open"
            && i.kind == "replan"
            && i.project_id == project_id
            && i.payload.get("kind").and_then(|v| v.as_str()) == Some("replan_report")
            && i.payload.get("task_id").and_then(|v| v.as_str()) == Some(task.id.as_str())
    }) {
        return (existing, true);
    }
    let mut payload = request;
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("kind".into(), json!("replan_report"));
        obj.insert("scope".into(), json!(scope));
        obj.insert("task_id".into(), json!(task.id));
        obj.insert("task_title".into(), json!(task.title));
        obj.insert("approved".into(), json!(false));
    }
    let item = events::new_inbox_item(st, c, project_id, "replan", payload);
    (item, false)
}

/// agent（执行者/验收者）在执行/验收输出中提出规划修改建议（`REPLAN:` 块）。
pub fn propose_from_agent(
    st: &Arc<App>,
    c: &Connection,
    task: &Task,
    reporter_agent_id: &str,
    reason: &str,
    detail: &str,
    changes: Value,
) -> Option<InboxItem> {
    if db::list_inbox(c).into_iter().any(|i| {
        i.status == "open"
            && i.kind == "replan"
            && i.payload.get("kind").and_then(|v| v.as_str()) == Some("replan_report")
            && i.payload.get("task_id").and_then(|v| v.as_str()) == Some(task.id.as_str())
    }) {
        return None;
    }
    let request = json!({
        "reason": reason,
        "detail": detail,
        "reported_by": "agent",
        "reporter_agent_id": reporter_agent_id,
        "proposed_changes": changes,
        "snapshot": {
            "status": task.status,
            "milestone_id": task.milestone_id,
            "depends_on": task.depends_on,
            "acceptance_criteria": task.acceptance_criteria,
        },
    });
    let (scope, reasons) = classify(Some(task), &request);
    events::push_event(st, c, &task.project_id, Some(&task.id), "replan.proposed",
        json!({
            "reason": request["reason"],
            "detail": request["detail"],
            "reported_by": "agent",
            "reporter_agent_id": reporter_agent_id,
            "proposed_changes": request["proposed_changes"],
            "task_id": task.id,
            "task_title": task.title,
            "scope": scope,
        }));
    let (item, _) = open_request(st, c, &task.project_id, task, request, &scope);
    tracing::info!(task = %task.id, inbox = %item.id, reasons = ?reasons, "replan request submitted to inbox");
    Some(item)
}

/// 从 agent 输出尾部解析 `REPLAN: {json}` 块（容错：任意位置、可带代码围栏）。
pub fn parse_replan_block(text: &str) -> Option<Value> {
    let idx = text.rfind("REPLAN:")?;
    let tail = &text[idx + "REPLAN:".len()..];
    api::extract_json_value_pub(tail)
}

/// 调度器扫描：已获人批准、尚未落地的重规划请求 → 派发规划 worker（可跨重启续跑）。
pub fn scan(state: &Arc<App>) {
    let pending: Vec<InboxItem> = {
        let conn = state.db.lock().unwrap();
        db::list_inbox(&conn)
            .into_iter()
            .filter(|i| {
                is_report(i)
                    && i.payload.get("approved").and_then(|v| v.as_bool()) == Some(true)
                    && i.payload.get("planning").and_then(|v| v.as_str()) == Some("pending")
            })
            .collect()
    };
    for item in pending {
        let conn = state.db.lock().unwrap();
        if state.has_live_handle(&item.id) {
            continue;
        }
        match db::get_project(&conn, &item.project_id).map(|p| p.status) {
            None => continue,
            Some(s) if s == "paused" || s == "pausing" || s == "archived" => continue,
            Some(_) => {}
        }
        events::push_event(state, &conn, &item.project_id,
            item.payload.get("task_id").and_then(|v| v.as_str()),
            "replan.planning_started", json!({"inbox_id": item.id}));
        let state2 = state.clone();
        let hid = state.alloc_handle_id();
        let item2 = item.clone();
        let h = tokio::spawn(async move { worker(state2, item2, hid).await });
        state.register_handle(&item.id, hid, h);
        drop(conn);
        state.wake();
        return;
    }
}

async fn worker(state: Arc<App>, item: InboxItem, hid: u64) {
    let guard = HandleGuard { state: state.clone(), task_id: item.id.clone(), id: hid };
    let (source, changes) = build_changes(&state, &item.project_id, &item.payload).await;
    let (applied, skipped) =
        api::apply_replan_changes(&state, &item.project_id, &changes, None, "replan");
    {
        let conn = state.db.lock().unwrap();
        if let Some(mut fresh) = db::get_inbox(&conn, &item.id) {
            if let Some(obj) = fresh.payload.as_object_mut() {
                obj.insert("planning".into(), json!("applied"));
                obj.insert("source".into(), json!(source));
                obj.insert("applied".into(), json!(applied.len()));
                obj.insert("skipped".into(), json!(skipped.len()));
            }
            let mut resolution = fresh.resolution.clone().unwrap_or_else(|| json!({}));
            if let Some(obj) = resolution.as_object_mut() {
                obj.insert("planning".into(), json!("applied"));
                obj.insert("source".into(), json!(source));
            }
            let _ = conn.execute(
                "UPDATE inbox SET payload=?2, resolution=?3 WHERE id=?1",
                rusqlite::params![item.id, fresh.payload.to_string(), resolution.to_string()],
            );
        }
        events::push_event(
            &state, &conn, &item.project_id,
            item.payload.get("task_id").and_then(|v| v.as_str()),
            "replan.applied",
            json!({
                "inbox_id": item.id,
                "source": source,
                "changes": changes.len(),
                "applied": applied,
                "skipped": skipped,
            }),
        );
        if let Some(fresh) = db::get_inbox(&conn, &item.id) {
            events::send_inbox(&state, &fresh);
        }
    }
    drop(guard);
    state.wake();
    tracing::info!(inbox = %item.id, project = %item.project_id, source, changes = changes.len(),
        "replan worker finished");
}

/// 产出调整方案：真实 planner 优先（复用规划会话 memory）→ 上报者建议 → 兜底补充任务。
async fn build_changes(state: &Arc<App>, project_id: &str, request: &Value) -> (String, Vec<Value>) {
    let (project, tasks, planner) = {
        let c = state.db.lock().unwrap();
        let project = db::get_project(&c, project_id);
        let tasks = project.as_ref().map(|p| db::list_tasks(&c, &p.id)).unwrap_or_default();
        let planner = db::list_agents(&c)
            .into_iter()
            .find(|a| a.adapter == "opencode" && a.roles.iter().any(|r| r == "planner"));
        (project, tasks, planner)
    };

    if let (Some(project), Some(planner)) = (project.as_ref(), planner.as_ref()) {
        let note = compose_note(request);
        let task_id = request["task_id"].as_str();
        let prompt = crate::knowledge::assemble(state, &project.id, task_id, "planner", "intervene", |k| {
            opencode::intervene_prompt(project, &tasks, &note, k)
        });
        let session = opencode::plan_session(&project.id);
        let call = opencode::run_cli(
            state,
            None,
            None,
            &session,
            opencode::model_of(planner).as_deref(),
            opencode::PLAN_TIMEOUT_MS,
            &prompt,
        )
        .await;
        match call {
            Ok(out) => {
                if let Some(v) = api::extract_json_value(&out.text) {
                    if let Some(arr) = v["changes"].as_array() {
                        if !arr.is_empty() {
                            return ("planner".into(), arr.clone());
                        }
                    }
                }
                tracing::warn!(target: "planner", inbox_project = %project.id, "replan: planner returned no changes; falling back");
            }
            Err(e) => {
                tracing::warn!(target: "planner", inbox_project = %project.id, "replan: planner failed: {e}; falling back");
            }
        }
    }

    if let Some(arr) = request["proposed_changes"].as_array().filter(|a| !a.is_empty()) {
        return ("reporter".into(), arr.clone());
    }

    let title = request["task_title"].as_str().unwrap_or("任务");
    let reason = request["reason"].as_str().unwrap_or("");
    let change = json!({
        "op": "add",
        "task": {
            "title": format!("重规划补充：{title}"),
            "description": reason,
            "depends_on": [],
            "acceptance_required": true,
            "acceptance_criteria": format!("完成针对「{title}」的规划补充并产出可检验结果（{reason}）"),
        }
    });
    ("default".into(), vec![change])
}

fn compose_note(request: &Value) -> String {
    let reported_by = request["reported_by"].as_str().unwrap_or("executor");
    let reason = request["reason"].as_str().unwrap_or("");
    let mut s = format!("执行中上报的规划问题（由 {reported_by} 提出）：\n{reason}\n");
    if let Some(detail) = request["detail"].as_str().filter(|d| !d.trim().is_empty()) {
        s.push_str(&format!("详细说明：{detail}\n"));
    }
    if let Some(task_title) = request["task_title"].as_str() {
        s.push_str(&format!("相关任务：{task_title}\n"));
    }
    if let Some(arr) = request["proposed_changes"].as_array().filter(|a| !a.is_empty()) {
        s.push_str(&format!(
            "上报者建议的调整（可采纳，也可给出更优方案）：{}\n",
            serde_json::to_string(arr).unwrap_or_default()
        ));
    }
    s.push_str("\n请基于以上问题给出任务调整方案（新增/修改/取消），保持克制、只改真正必要的部分。");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tactical_task_is_classified_tactical() {
        let t = Task {
            id: "t1".into(), project_id: "p".into(), milestone_id: None,
            title: "t".into(), description: String::new(), depends_on: vec![],
            assignee_role: "executor".into(), executor_agent_id: None,
            acceptor_agent_id: None, acceptance_required: true, acceptance_criteria: None,
            execution_plan: None, plan_rejection_count: 0, scope_repos: vec![],
            agent_pinned: false, tried_agents: json!({}), status: "executing".into(),
            version: 0, attempt: 1, rejection_count: 0, failure_count: 0,
            artifacts: vec![], created_at: 0,
        };
        assert_eq!(classify(Some(&t), &json!({"reason": "x"})).0, "tactical");
        assert_eq!(classify(Some(&t), &json!({"milestone": "m"})).0, "strategic");
    }

    #[test]
    fn replan_block_parser_finds_last_json() {
        let text = "说明\nREPLAN: {\"reason\":\"r\",\"changes\":[{\"op\":\"add\"}]}\n";
        let v = parse_replan_block(text).unwrap();
        assert_eq!(v["reason"], "r");
    }
}
