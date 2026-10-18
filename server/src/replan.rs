//! M3 动态重规划双轨之「常规纠偏」通道（DESIGN §6/§8）。
//!
//! 链路：executor/acceptor 上报「发现规划问题」→ 生成 `replan.proposed` 事件与
//! 一条 kind=`replan` 的决策请求 → 决策者复核：
//!
//! - **tactical（不触及里程碑/项目目标）**：由持有 decision_maker 角色的 agent 自决；
//! - **strategic（触及已确认里程碑/项目目标）**：`decision::escalate_to_human`
//!   直接投递人工收件箱，决策者不得自决。
//!
//! 批准（agent 或人）只把决策置为 ruled 并写 `replan.approved`，随后 `scan`
//! 派发规划 worker：
//! - 有真实 planner → 复用 `planner-<prj_id>` 规划会话（memory 连续）产出 changes；
//! - 否则采用上报者给出的 `proposed_changes`，再无则生成一条带验收的兜底补充任务；
//! - changes 经 `api::apply_replan_changes` 逐项落地：新任务默认带验收、
//!   插入 DAG 前做环校验（成环项跳过）、单项失败不影响其余项；
//! - 落地结果写入 `replan.applied`，新任务进入调度。
//!
//! 拒绝 → 写 `replan.rejected` 并通知上报者，原任务流程不动。
//!
//! 批准后若进程重启（规划尚未落地），`scan` 会从 "planning=pending" 的决策
//! 继续派发，保证可恢复。

use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{
    agents::opencode,
    api, db, decision, events,
    models::{Decision, Task},
    state::{App, HandleGuard},
};

/// 递归查找对象键：命中任一战略键（且值非 null）即视为触及里程碑/项目目标。
fn touches_strategic_key(v: &Value) -> bool {
    match v {
        Value::Object(map) => map.iter().any(|(k, val)| {
            (decision::STRATEGIC_KEYS.contains(&k.as_str()) && !val.is_null())
                || touches_strategic_key(val)
        }),
        Value::Array(arr) => arr.iter().any(touches_strategic_key),
        _ => false,
    }
}

/// 重规划请求分类：(scope, reasons)。
///
/// 保守策略：任务只要挂在任意里程碑下即视为「触及里程碑目标」；请求体里出现
/// 里程碑/项目目标相关键同样升级。战略事项一律转人工收件箱。
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

/// M4：agent（执行者/验收者）在执行/验收输出中提出规划修改建议（`REPLAN:` 块）。
/// 与人工上报共用同一条链路：幂等去重 → replan.proposed → 决策者复核（战略事项转人工）。
pub fn propose_from_agent(
    st: &Arc<App>,
    c: &Connection,
    task: &Task,
    reporter_agent_id: &str,
    reason: &str,
    detail: &str,
    changes: Value,
) -> Option<Decision> {
    // 幂等：同一任务已有未决重规划请求 → 不重复登记
    if db::list_decisions(c, Some(&task.project_id)).into_iter().any(|d| {
        d.kind == "replan"
            && d.task_id.as_deref() == Some(task.id.as_str())
            && (d.status == "pending" || d.status == "escalated")
    }) {
        return None;
    }
    let request = json!({
        "reason": reason,
        "detail": detail,
        "reported_by": "agent",
        "reporter_agent_id": reporter_agent_id,
        "task_id": task.id,
        "task_title": task.title,
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
    let d = decision::request_scoped(st, c, &task.project_id, Some(&task.id), "replan", &scope, request);
    if scope == "strategic" {
        let detail = format!(
            "agent 提出的重规划触及里程碑/项目目标（{}），决策者不得自决，转人工裁决",
            reasons.join(", ")
        );
        decision::escalate_to_human(st, c, &d, &reasons, &detail);
    }
    db::get_decision(c, &d.id).or(Some(d))
}

/// 从 agent 输出尾部解析 `REPLAN: {json}` 块（容错：任意位置、可带代码围栏）。
pub fn parse_replan_block(text: &str) -> Option<Value> {
    let idx = text.rfind("REPLAN:")?;
    let tail = &text[idx + "REPLAN:".len()..];
    api::extract_json_value_pub(tail)
}

/// 决策已批准、等待 planner 产出并落地调整（含进程重启后的续跑）。
pub fn planning_pending(d: &Decision) -> bool {
    d.kind == "replan"
        && d.status == "ruled"
        && d.ruling
            .as_ref()
            .and_then(|r| r.get("effect"))
            .and_then(|e| e.get("planning"))
            .and_then(|v| v.as_str())
            == Some("pending")
}

/// 是否属于「决策者 agent 自决的战术批准」——只有这种批准才可能被 planner 产出
/// 悄悄带上战略改动，需要二次护栏；人工裁定或本就战略的请求不在此列。
fn tactical_agent_approval(d: &Decision) -> bool {
    if d.scope != "tactical" {
        return false;
    }
    let by = d.ruling.as_ref().and_then(|r| r.get("by")).and_then(|v| v.as_str());
    by != Some("human")
}

/// 调整方案是否触及里程碑/项目目标：新增任务挂到里程碑、或变更字段含战略键。
fn changes_touch_strategic(changes: &[Value]) -> bool {
    changes.iter().any(|ch| match ch["op"].as_str().unwrap_or("") {
        "add" => {
            touches_strategic_key(ch.get("task").unwrap_or(&Value::Null)) || touches_strategic_key(ch)
        }
        "update" => touches_strategic_key(ch.get("fields").unwrap_or(&Value::Null)),
        _ => false,
    })
}

/// 规划产出触及战略目标：撤销「自动落地」，转人工收件箱裁决（复用决策升级通道）。
/// 决策从 ruled 置为 escalated，人工裁定后 replan::scan 会重新派发规划并落地。
fn escalate_planning(state: &Arc<App>, d: &Decision, changes: &[Value]) {
    let conn = state.db.lock().unwrap();
    let reasons = vec!["planner_changes_touch:milestone_or_project_goal".to_string()];
    let mut escalated = d.clone();
    if let Some(obj) = escalated.request.as_object_mut() {
        obj.insert("planner_changes".into(), json!(changes));
    }
    decision::escalate_to_human(
        state,
        &conn,
        &escalated,
        &reasons,
        "决策者已批准的战术重规划，其规划产出触及里程碑/项目目标，自动升级人工裁决",
    );
    events::push_event(
        state,
        &conn,
        &d.project_id,
        d.task_id.as_deref(),
        "replan.escalated",
        json!({"decision_id": d.id, "changes": changes.len(), "reasons": reasons}),
    );
}

/// 调度器扫描：找到「已批准但尚未规划落地」的重规划请求并派发规划 worker。
pub fn scan(state: &Arc<App>) {
    let conn = state.db.lock().unwrap();
    let pending: Vec<Decision> = db::list_decisions(&conn, None)
        .into_iter()
        .filter(planning_pending)
        .collect();
    for d in pending {
        if state.has_live_handle(&d.id) {
            continue; // 规划 worker 正在跑（或决策 worker 尚未释放句柄）
        }
        // 冻结语义（与 scheduler::scan / decision::scan 对齐）：暂停/归档只冻结
        // 「自动」规划派发——人工或 agent 的裁定（decisions 已 ruled）仍立即生效，
        // 但由此触发的 planner 派发延后到项目恢复后再续跑（planning=pending 即恢复锚点）。
        // completed 不在此列：批准后若项目已 auto-completed，由 apply_replan_changes 的
        // reopen_if_completed 重新拉起（保持原有可恢复行为，避免裁定悬空）。
        match db::get_project(&conn, &d.project_id).map(|p| p.status) {
            None => continue, // project missing
            Some(s) if s == "paused" || s == "archived" => continue, // 冻结自动流程
            Some(_) => {}
        }
        dispatch(state, &conn, &d);
        drop(conn);
        state.wake();
        return;
    }
}

fn dispatch(state: &Arc<App>, conn: &Connection, d: &Decision) {
    events::push_event(
        state,
        conn,
        &d.project_id,
        d.task_id.as_deref(),
        "replan.planning_started",
        json!({"decision_id": d.id}),
    );
    let state2 = state.clone();
    let d2 = d.clone();
    let hid = state.alloc_handle_id();
    let h = tokio::spawn(async move { worker(state2, d2, hid).await });
    state.register_handle(&d.id, hid, h);
}

async fn worker(state: Arc<App>, d: Decision, hid: u64) {
    let guard = HandleGuard { state: state.clone(), task_id: d.id.clone(), id: hid };
    let (source, changes) = build_changes(&state, &d).await;

    // 战略护栏：批准得到的调整若触及里程碑/项目目标，而该决策本身是「战术」、且由
    // 决策者 agent 自决，则不得自动落地——planner 产出可能替上报者补出跨里程碑的
    // 调整，绕过上报时的战略分类。此时自动升级到人工收件箱等待裁决（与上报触及战略
    // 目标即升级一致）；人工已裁定过的批准不再升级，避免循环。
    if tactical_agent_approval(&d) && changes_touch_strategic(&changes) {
        escalate_planning(&state, &d, &changes);
        drop(guard);
        state.wake();
        return;
    }

    let (applied, skipped) =
        api::apply_replan_changes(&state, &d.project_id, &changes, None, "replan");
    {
        let conn = state.db.lock().unwrap();
        if let Some(mut fresh) = db::get_decision(&conn, &d.id) {
            let mut ruling = fresh.ruling.take().unwrap_or_else(|| json!({}));
            if let Some(obj) = ruling.as_object_mut() {
                let effect = obj.entry("effect").or_insert_with(|| json!({}));
                if let Some(eff) = effect.as_object_mut() {
                    eff.insert("planning".into(), json!("applied"));
                    eff.insert("source".into(), json!(source));
                    eff.insert("changes".into(), json!(changes.len()));
                    eff.insert("applied".into(), json!(applied));
                    eff.insert("skipped".into(), json!(skipped));
                }
            }
            let _ = conn.execute(
                "UPDATE decisions SET ruling=?2 WHERE id=?1",
                rusqlite::params![d.id, ruling.to_string()],
            );
        }
        events::push_event(
            &state,
            &conn,
            &d.project_id,
            d.task_id.as_deref(),
            "replan.applied",
            json!({
                "decision_id": d.id,
                "source": source,
                "changes": changes.len(),
                "applied": applied,
                "skipped": skipped,
            }),
        );
    }
    drop(guard);
    state.wake();
    tracing::info!(
        decision = %d.id,
        project = %d.project_id,
        source,
        changes = changes.len(),
        "replan planning worker finished"
    );
}

/// 产出调整方案：真实 planner 优先（复用规划会话 memory）→ 上报者建议 → 兜底补充任务。
async fn build_changes(state: &Arc<App>, d: &Decision) -> (String, Vec<Value>) {
    let (project, tasks, planner) = {
        let c = state.db.lock().unwrap();
        let project = db::get_project(&c, &d.project_id);
        let tasks = db::list_tasks(&c, &d.project_id);
        let planner = db::list_agents(&c)
            .into_iter()
            .find(|a| a.adapter == "opencode" && a.roles.iter().any(|r| r == "planner"));
        (project, tasks, planner)
    };

    // 1) 真实 planner：与生成规划 / 人工介入共用 planner-<prj_id> 会话 → memory 连续
    if let (Some(project), Some(planner)) = (project.as_ref(), planner.as_ref()) {
        let note = compose_note(&d.request);
        let prompt = opencode::intervene_prompt(project, &tasks, &note);
        let session = opencode::plan_session(&d.project_id);
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
                tracing::warn!(target: "planner", decision = %d.id, "replan: planner returned no changes; falling back");
            }
            Err(e) => {
                tracing::warn!(target: "planner", decision = %d.id, "replan: planner failed: {e}; falling back");
            }
        }
    }

    // 2) 回退：采用上报者随请求给出的建议调整
    if let Some(arr) = d.request["proposed_changes"].as_array().filter(|a| !a.is_empty()) {
        return ("reporter".into(), arr.clone());
    }

    // 3) 兜底：生成一条带验收的补充任务，保证批准后确有推进并进入调度
    let title = d.request["task_title"].as_str().unwrap_or("任务");
    let reason = d.request["reason"].as_str().unwrap_or("");
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

    fn task(milestone: Option<&str>) -> Task {
        Task {
            id: "tsk_1".into(),
            project_id: "prj_1".into(),
            milestone_id: milestone.map(String::from),
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
            status: "executing".into(),
            attempt: 1,
            rejection_count: 0,
            failure_count: 0,
            artifacts: vec![],
            created_at: 0,
        }
    }

    #[test]
    fn free_task_replan_is_tactical() {
        let (scope, reasons) = classify(
            Some(&task(None)),
            &json!({"reason": "依赖顺序不合理", "proposed_changes": [{"op": "add", "task": {"title": "x"}}]}),
        );
        assert_eq!(scope, "tactical", "{reasons:?}");
    }

    #[test]
    fn milestone_task_replan_is_strategic() {
        let (scope, reasons) = classify(Some(&task(Some("mst_1"))), &json!({"reason": "x"}));
        assert_eq!(scope, "strategic");
        assert!(reasons.contains(&"task_belongs_to_milestone".to_string()));
    }

    #[test]
    fn milestone_or_goal_keys_inside_nested_changes_are_strategic() {
        let nested = json!({
            "reason": "调整归属",
            "proposed_changes": [{"op": "update", "fields": {"milestone_id": "mst_9"}}]
        });
        let (scope, reasons) = classify(Some(&task(None)), &nested);
        assert_eq!(scope, "strategic", "{reasons:?}");

        let project = json!({"proposed_changes": [{"op": "update", "fields": {"project": {"brief": "改目标"}}}]});
        assert_eq!(classify(Some(&task(None)), &project).0, "strategic");
    }

    #[test]
    fn null_strategic_keys_do_not_escalate() {
        let req = json!({"reason": "x", "snapshot": {"milestone_id": null}, "milestone_id": null});
        assert_eq!(classify(Some(&task(None)), &req).0, "tactical");
    }

    #[test]
    fn planning_pending_detection() {
        let mut d = Decision {
            id: "dec_1".into(),
            project_id: "prj_1".into(),
            task_id: Some("tsk_1".into()),
            kind: "replan".into(),
            request: json!({}),
            scope: "tactical".into(),
            status: "ruled".into(),
            ruling: Some(json!({"decision": "approve", "effect": {"effect": "replan_approved", "planning": "pending"}})),
            attempt: 1,
            created_at: 0,
            resolved_at: Some(1),
        };
        assert!(planning_pending(&d));
        d.ruling = Some(json!({"decision": "approve", "effect": {"effect": "replan_approved", "planning": "applied"}}));
        assert!(!planning_pending(&d));
        d.status = "escalated".into();
        assert!(!planning_pending(&d));
    }

    #[test]
    fn planner_changes_touching_milestone_are_detected() {
        let plain = json!([{"op": "add", "task": {"title": "x", "depends_on": []}}]);
        assert!(!changes_touch_strategic(&[plain[0].clone()]));
        let with_ms = json!([{"op": "add", "task": {"title": "x", "milestone_id": "mst_1"}}]);
        assert!(changes_touch_strategic(&[with_ms[0].clone()]));
        let upd = json!([{"op": "update", "task_id": "t", "fields": {"milestone": "M1"}}]);
        assert!(changes_touch_strategic(&[upd[0].clone()]));
        let null_ms = json!([{"op": "add", "task": {"title": "x", "milestone_id": null}}]);
        assert!(!changes_touch_strategic(&[null_ms[0].clone()]));
    }

    #[test]
    fn only_agent_approved_tactical_replans_get_the_guard() {
        let mut d = Decision {
            id: "dec_1".into(),
            project_id: "prj_1".into(),
            task_id: Some("tsk_1".into()),
            kind: "replan".into(),
            request: json!({}),
            scope: "tactical".into(),
            status: "ruled".into(),
            ruling: Some(json!({"decision": "approve", "by": "agent"})),
            attempt: 1,
            created_at: 0,
            resolved_at: Some(1),
        };
        assert!(tactical_agent_approval(&d));
        d.ruling = Some(json!({"decision": "approve", "by": "human"}));
        assert!(!tactical_agent_approval(&d));
        d.scope = "strategic".into();
        d.ruling = Some(json!({"decision": "approve", "by": "agent"}));
        assert!(!tactical_agent_approval(&d));
    }
}
