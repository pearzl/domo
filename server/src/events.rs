//! 事件、状态流转与 SSE 广播的唯一出口。
//!
//! - 事件 **append-only**：只能新增，不能改删；`kind` 命名 `<域>.<动作>`
//!   （如 `task.status_changed`、`milestone.gate_opened`、`sandbox.violation`）。
//! - `payload` 必须自包含（关联 id + 判定所需信息），保证事后可回放。
//! - 任务状态变更只能走 `status_changed`、里程碑用 `milestone_status_changed`：
//!   落库 + 审计事件 + SSE 广播一次完成。**禁止只改库不发事件**（前端会显示陈旧状态），
//!   也不要自行拼装状态变更事件。
//! - SSE 事件类型：`task` / `agent` / `project` / `inbox` / `milestone` / `report` /
//!   `event` / `log`，payload 为实体完整快照（前端据此整体替换，不做增量合并）。
//! - 每次 agent 调用的输入（提示词、上下文、注入的知识条目）与完整输出都要归档。

use std::sync::Arc;
use crate::db;
use crate::models::*;
use crate::state::App;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// Append an audit event and broadcast it on SSE as `event`.
pub fn push_event(
    state: &Arc<App>,
    c: &Connection,
    project_id: &str,
    task_id: Option<&str>,
    kind: &str,
    payload: Value,
) -> Event {
    let ts = db::now();
    c.execute(
        "INSERT INTO events (ts, project_id, task_id, \"type\", payload) VALUES (?1,?2,?3,?4,?5)",
        params![ts, project_id, task_id, kind, payload.to_string()],
    )
    .expect("insert event");
    let id = c.last_insert_rowid();
    let ev = Event {
        id,
        ts,
        project_id: project_id.into(),
        task_id: task_id.map(String::from),
        kind: kind.into(),
        payload,
    };
    state.send("event", serde_json::to_value(&ev).unwrap_or(json!({})));
    ev
}

/// Write a task log line and broadcast it on SSE as `log`.
pub fn log_line(state: &Arc<App>, c: &Connection, task_id: &str, line: &str) {
    let ts = db::now();
    c.execute(
        "INSERT INTO logs (task_id, ts, line) VALUES (?1,?2,?3)",
        params![task_id, ts, line],
    )
    .ok();
    state.send("log", json!({"task_id": task_id, "ts": ts, "line": line}));
}

/// log_line for contexts that only hold the App handle (locks the db itself).
pub fn log_line_state(state: &Arc<App>, task_id: &str, line: &str) {
    let c = state.db.lock().unwrap();
    log_line(state, &c, task_id, line);
}

pub fn status_changed(state: &Arc<App>, c: &Connection, task: &Task, to: &str, reason: &str) -> Task {
    let from = task.status.clone();
    // ARCH A2：状态变更必须推进 version，派发方的 CAS 才能识别“已被别人改过”。
    let _ = c.execute(
        "UPDATE tasks SET status=?2, version=version+1 WHERE id=?1",
        params![task.id, to],
    );
    // M4 三档恢复标记随状态自洽：进入终态清全部；离开 suspended 清「人工重建挂起」
    // （人工恢复/换人重派/恢复派发都算离开，挂起期间保持）。
    if to == "done" || to == "cancelled" {
        let _ = c.execute("UPDATE tasks SET recovery_pending=0, recovery_manual=0 WHERE id=?1", params![task.id]);
    } else if to != "suspended" {
        let _ = c.execute("UPDATE tasks SET recovery_manual=0 WHERE id=?1", params![task.id]);
    }
    tracing::info!(task = %task.id, title = %task.title, from = %from, to = %to, reason, "task status changed");
    push_event(
        state,
        c,
        &task.project_id,
        Some(&task.id),
        "task.status_changed",
        json!({"from": from, "to": to, "reason": reason}),
    );
    let mut t = task.clone();
    t.status = to.into();
    t.version += 1;
    state.send("task", serde_json::to_value(&t).unwrap_or(json!({})));
    t
}

pub fn send_task(state: &Arc<App>, t: &Task) {
    state.send("task", serde_json::to_value(t).unwrap_or(json!({})));
}

pub fn send_agent(state: &Arc<App>, a: &Agent) {
    state.send("agent", serde_json::to_value(a).unwrap_or(json!({})));
}

pub fn send_project(state: &Arc<App>, p: &Project) {
    state.send("project", serde_json::to_value(p).unwrap_or(json!({})));
}

pub fn send_inbox(state: &Arc<App>, i: &InboxItem) {
    state.send("inbox", serde_json::to_value(i).unwrap_or(json!({})));
}

pub fn send_milestone(state: &Arc<App>, m: &Milestone) {
    state.send("milestone", serde_json::to_value(m).unwrap_or(json!({})));
}

/// M3 报告：SSE 广播报告状态变化（前端据此渲染生成中 / 失败重试 / 查看入口）。
pub fn send_report(state: &Arc<App>, r: &Report) {
    state.send("report", serde_json::to_value(r).unwrap_or(json!({})));
}

/// M3: persist + audit a milestone status transition, mirroring `status_changed`
/// for tasks. Always appends a `milestone.status_changed` audit event and
/// broadcasts the milestone on SSE.
pub fn milestone_status_changed(state: &Arc<App>, c: &Connection, m: &Milestone, to: &str, reason: &str) -> Milestone {
    let from = m.status.clone();
    db::set_milestone_status(c, &m.id, to);
    tracing::info!(milestone = %m.id, name = %m.name, from = %from, to = %to, reason, "milestone status changed");
    push_event(
        state,
        c,
        &m.project_id,
        None,
        "milestone.status_changed",
        json!({"milestone_id": m.id, "name": m.name, "from": from, "to": to, "reason": reason}),
    );
    let mut m2 = m.clone();
    m2.status = to.into();
    send_milestone(state, &m2);
    m2
}

pub fn new_inbox_item(
    state: &Arc<App>,
    c: &Connection,
    project_id: &str,
    kind: &str,
    payload: Value,
) -> InboxItem {
    let item = InboxItem {
        id: db::new_id("inb_"),
        project_id: project_id.into(),
        kind: kind.into(),
        payload,
        status: "open".into(),
        resolution: None,
        created_at: db::now(),
    };
    c.execute(
        "INSERT INTO inbox (id, project_id, \"type\", payload, status, resolution, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![item.id, item.project_id, item.kind, item.payload.to_string(), item.status, Option::<String>::None, item.created_at],
    )
    .expect("insert inbox item");
    send_inbox(state, &item);
    item
}
