//! M3 项目总结报告（DESIGN §3「COMPLETED → 产出项目总结报告」、§9 汇报/总结）。
//!
//! 项目进入 COMPLETED 后由调度器自动触发（`maybe_generate_on_completion`）：
//!
//! 1. **汇总事实**：从事件溯源与当前状态计算事实快照（`facts`）——任务状态统计、
//!    每个里程碑的 goal 与 gate 结果、退回/升级/挂起事件摘要、验收记录、产物清单、
//!    事件类型分布与关键时间线。
//! 2. **planner 叙述**：把事实快照交给 planner 角色 agent（优先 opencode，兼容 mock），
//!    由它撰写「报告概述 + 里程碑汇报段落」，不编造事实。
//! 3. **拼装 + 持久化**：叙述 + 确定性事实章节拼成 Markdown，落库（`reports` 表）
//!    并写入项目数据目录 `<data>/projects/<prj_id>/summary-report.md`
//!    （另有 `summary-report.json` 事实快照）。
//!
//! **失败可重试**：planner 运行失败/输出为空 → 报告置 `failed`、`tracing::error!`
//! 写入 `data/domo.log`，等待人工「重试」；`POST /api/projects/:id/report` 触发重生成，
//! 修好 agent 配置后重试即可成功。进程重启时中断的 `pending/generating` 报告在启动
//! 迁移中统一置为 `failed`（同样可手工重试）。
//!
//! 无 planner 可用时不阻塞：回退到确定性叙述（`no_planner_narrative`），报告仍生成。

use std::{collections::BTreeMap, sync::Arc};

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{
    agents::{mock, opencode},
    db, events,
    models::{Agent, Event, Milestone, Project, Report, Task},
    state::{App, HandleGuard},
};

/// 里程碑状态 → gate 结果中文描述（报告用）。
pub fn gate_result(status: &str) -> &'static str {
    match status {
        "passed" => "通过（gate 已确认，下游分支已放行）",
        "failed" => "退回（gate 已退回，下游分支冻结并重开重规划窗口）",
        "awaiting_confirm" => "待人工确认（gate 进行中，下游分支暂挂起）",
        "pending" => "未提交确认（里程碑仍在推进）",
        _ => "未知",
    }
}

fn project_status_label(status: &str) -> &'static str {
    match status {
        "initializing" => "初始化中",
        "pending_confirm" => "待确认",
        "running" => "运行中",
        "paused" => "已暂停",
        "completed" => "已完成",
        "archived" => "已归档",
        _ => "未知",
    }
}

fn task_status_label(status: &str) -> &'static str {
    match status {
        "planned" => "计划中",
        "ready" => "待执行",
        "executing" => "执行中",
        "awaiting_acceptance" => "待验收",
        "escalated" => "已升级",
        "suspended" => "已挂起",
        "done" => "已完成",
        "cancelled" => "已取消",
        _ => "未知",
    }
}

/// 报告落盘目录：`<data_dir>/projects/<project_id>/`。
pub fn report_dir(project_id: &str) -> std::path::PathBuf {
    db::data_dir().join("projects").join(project_id)
}

/// 报告摘要（不含 content/facts），供 GET /api/projects/:id 列表轻量返回。
pub fn summary_json(r: &Report) -> Value {
    json!({
        "id": r.id,
        "project_id": r.project_id,
        "status": r.status,
        "error": r.error,
        "path": r.path,
        "attempts": r.attempts,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
        "generated_at": r.generated_at,
    })
}

/// 调度器在项目转入 COMPLETED 时调用：没有报告才自动生成（幂等）。
pub fn maybe_generate_on_completion(state: &Arc<App>, conn: &Connection, project: &Project) {
    if db::get_report(conn, &project.id).is_some() {
        return; // 报告已存在（ready/failed/pending 均不重复自动触发；失败靠手工重试）
    }
    if let Err(e) = trigger(state, conn, project, "auto") {
        tracing::warn!(target: "report", project = %project.id, "report auto trigger skipped: {e}");
    }
}

/// 触发一次报告生成（自动 / 手工重试共用）。立即落库为 `pending` 并派发后台 worker，
/// 返回 pending 状态的报告行。生成中重复触发 → Err（409）。
pub fn trigger(
    state: &Arc<App>,
    conn: &Connection,
    project: &Project,
    source: &str,
) -> Result<Report, String> {
    let prev = db::get_report(conn, &project.id);
    if let Some(p) = &prev {
        if p.status == "pending" || p.status == "generating" {
            return Err("report generation already in progress".into());
        }
    }
    let now = db::now();
    let report = Report {
        id: prev
            .as_ref()
            .map(|r| r.id.clone())
            .unwrap_or_else(|| db::new_id("rpt_")),
        project_id: project.id.clone(),
        status: "pending".into(),
        content: prev.as_ref().and_then(|r| r.content.clone()),
        narrative: prev.as_ref().and_then(|r| r.narrative.clone()),
        error: None,
        path: prev.as_ref().and_then(|r| r.path.clone()),
        facts: prev
            .as_ref()
            .map(|r| r.facts.clone())
            .unwrap_or_else(|| json!({})),
        attempts: prev.as_ref().map(|r| r.attempts).unwrap_or(0) + 1,
        created_at: prev.as_ref().map(|r| r.created_at).unwrap_or(now),
        updated_at: now,
        generated_at: prev.as_ref().and_then(|r| r.generated_at),
    };
    db::save_report(conn, &report);
    events::push_event(
        state,
        conn,
        &project.id,
        None,
        "report.requested",
        json!({"report_id": report.id, "source": source, "attempts": report.attempts}),
    );
    events::send_report(state, &report);

    let state2 = state.clone();
    let pid = project.id.clone();
    let hid = state.alloc_handle_id();
    let h = tokio::spawn(async move { worker(state2, pid, hid).await });
    state.register_handle(&project.id, hid, h);
    Ok(report)
}

/// 后台 worker：汇总事实 → planner 叙述 → 拼装 → 落库/落盘。
/// 任何一步失败都置 `failed` 并写日志，绝不 panic（报告失败不应影响项目状态）。
async fn worker(state: Arc<App>, project_id: String, hid: u64) {
    let guard = HandleGuard { state: state.clone(), task_id: project_id.clone(), id: hid };

    // ---- 快照 + 置 generating ----
    let snap = {
        let c = state.db.lock().unwrap();
        let Some(mut report) = db::get_report(&c, &project_id) else {
            return;
        };
        let Some(project) = db::get_project(&c, &project_id) else {
            report.status = "failed".into();
            report.error = Some("project not found".into());
            report.updated_at = db::now();
            db::save_report(&c, &report);
            events::send_report(&state, &report);
            return;
        };
        report.status = "generating".into();
        report.updated_at = db::now();
        db::save_report(&c, &report);
        events::send_report(&state, &report);
        (
            report,
            project,
            db::list_tasks(&c, &project_id),
            db::list_milestones(&c, &project_id),
            db::list_agents(&c),
            db::list_events(&c, Some(&project_id), None),
        )
    };
    let (report, project, tasks, milestones, agents, events_list) = snap;

    // ---- 事实快照 ----
    let facts = build_facts(&project, &tasks, &milestones, &events_list, db::now());

    // ---- planner 叙述（按登记顺序依次尝试候选，单个限流/故障自动回退到下一个）----
    let candidates = planner_candidates(&agents);
    let narrative: Result<String, String> = if candidates.is_empty() {
        tracing::warn!(target: "report", project = %project_id,
            "no planner-role agent registered; generating deterministic report without planner narrative");
        Ok(no_planner_narrative(&facts))
    } else {
        let mut last: Result<String, String> = Err("no planner candidate attempted".into());
        for a in &candidates {
            match run_narrative(&state, &project, &facts, a).await {
                Ok(text) => {
                    last = Ok(text);
                    break;
                }
                Err(e) => {
                    tracing::warn!(target: "report", project = %project_id, agent = %a.name,
                        "planner narrative failed, falling back to next candidate: {e}");
                    last = Err(e);
                }
            }
        }
        last
    };

    let now = db::now();
    // ---- 拼装 / 落库 / 落盘 ----
    match narrative {
        Ok(text) => {
            let content = compose_markdown(&project, &facts, &text);
            let path = match persist(&project.id, &content, &facts) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!(target: "report", project = %project.id, "report persist failed: {e}");
                    None
                }
            };
            let c = state.db.lock().unwrap();
            let mut r = db::get_report(&c, &project_id).unwrap_or(report);
            r.status = "ready".into();
            r.narrative = Some(text);
            r.content = Some(content);
            r.error = None;
            r.path = path;
            r.facts = facts;
            r.generated_at = Some(now);
            r.updated_at = now;
            db::save_report(&c, &r);
            events::push_event(
                &state,
                &c,
                &project_id,
                None,
                "report.generated",
                json!({
                    "report_id": r.id,
                    "path": r.path,
                    "milestones": r.facts["milestones"].as_array().map(|a| a.len()).unwrap_or(0),
                    "tasks": r.facts["counts"]["total"],
                }),
            );
            events::send_report(&state, &r);
            tracing::info!(target: "report", project = %project_id, "project summary report generated");
        }
        Err(e) => {
            let c = state.db.lock().unwrap();
            let mut r = db::get_report(&c, &project_id).unwrap_or(report);
            r.status = "failed".into();
            r.error = Some(e.clone());
            r.facts = facts;
            r.updated_at = now;
            db::save_report(&c, &r);
            // 报告生成失败写 data/domo.log（tracing 文件层），保留 failed 供手工重试
            tracing::error!(target: "report", project = %project_id, attempts = r.attempts, error = %e,
                "project summary report generation failed; manual retry available via POST /api/projects/:id/report");
            events::push_event(
                &state,
                &c,
                &project_id,
                None,
                "report.failed",
                json!({"report_id": r.id, "error": e, "attempts": r.attempts}),
            );
            events::send_report(&state, &r);
        }
    }
    drop(guard);
    state.wake();
}

/// 选用叙述 planner：优先 opencode（真实 LLM），否则任意持 planner 角色的实例。
/// 叙述 planner 候选：opencode（真实 LLM）按登记顺序全部入选——单个被限流/故障时
/// 自动回退到下一个；没有 opencode 实例时回退到任意持 planner 角色的实例（mock）。
fn planner_candidates(agents: &[Agent]) -> Vec<Agent> {
    let cands: Vec<Agent> = agents
        .iter()
        .filter(|a| a.adapter == "opencode" && a.roles.iter().any(|r| r == "planner"))
        .cloned()
        .collect();
    if !cands.is_empty() {
        return cands;
    }
    agents
        .iter()
        .filter(|a| a.roles.iter().any(|r| r == "planner"))
        .cloned()
        .collect()
}

async fn run_narrative(
    state: &Arc<App>,
    project: &Project,
    facts: &Value,
    agent: &Agent,
) -> Result<String, String> {
    match agent.adapter.as_str() {
        "opencode" => {
            opencode::report_narrative(state.clone(), project.clone(), facts.clone(), agent.clone()).await
        }
        _ => mock::report_narrative(project, facts, agent).await,
    }
}

/// 无 planner 时的确定性叙述兜底（不阻断报告生成）。
fn no_planner_narrative(facts: &Value) -> String {
    let total = facts["counts"]["total"].as_i64().unwrap_or(0);
    let done = facts["counts"]["done"].as_i64().unwrap_or(0);
    let m = facts["milestones"].as_array().map(|a| a.len()).unwrap_or(0);
    format!(
        "（未注册 planner 角色 agent，本节为系统生成的概述）项目共 {} 个任务、完成 {} 个，覆盖 {} 个里程碑。详细统计、里程碑 gate 结果、事件摘要与产物清单见后续章节。",
        total, done, m
    )
}

// ---------- 事实快照 ----------

/// 从项目当前状态 + 事件流计算报告事实快照（纯函数，便于单测）。
pub fn build_facts(
    project: &Project,
    tasks: &[Task],
    milestones: &[Milestone],
    events: &[Event],
    generated_at: i64,
) -> Value {
    let mut by_status: BTreeMap<String, i64> = BTreeMap::new();
    for t in tasks {
        *by_status.entry(t.status.clone()).or_insert(0) += 1;
    }
    let done = by_status.get("done").copied().unwrap_or(0);
    let cancelled = by_status.get("cancelled").copied().unwrap_or(0);
    let failure_sum: i64 = tasks.iter().map(|t| t.failure_count).sum();
    let rejection_sum: i64 = tasks.iter().map(|t| t.rejection_count).sum();
    let attempt_sum: i64 = tasks.iter().map(|t| t.attempt).sum();

    // 里程碑 + 成员任务 + gate 结果
    let milestones_json: Vec<Value> = milestones
        .iter()
        .map(|m| {
            let members: Vec<Value> = tasks
                .iter()
                .filter(|t| t.milestone_id.as_deref() == Some(m.id.as_str()))
                .map(|t| {
                    json!({
                        "id": t.id, "title": t.title, "status": t.status,
                        "status_label": task_status_label(&t.status),
                        "artifacts": t.artifacts,
                    })
                })
                .collect();
            json!({
                "id": m.id, "name": m.name, "goal": m.goal, "effect": m.effect,
                "status": m.status, "gate_result": gate_result(&m.status),
                "members": members,
            })
        })
        .collect();

    let title_of = |tid: &Option<String>| -> Option<String> {
        tid.as_deref().and_then(|id| tasks.iter().find(|t| t.id == id).map(|t| t.title.clone()))
    };

    let mut rejections: Vec<Value> = Vec::new();
    let mut escalations: Vec<Value> = Vec::new();
    let mut suspensions: Vec<Value> = Vec::new();
    let mut acceptance_records: Vec<Value> = Vec::new();
    let mut event_types: BTreeMap<String, i64> = BTreeMap::new();
    let mut timeline: Vec<Value> = Vec::new();

    for e in events {
        *event_types.entry(e.kind.clone()).or_insert(0) += 1;
        let title = title_of(&e.task_id);
        match e.kind.as_str() {
            "acceptance.rejected" => rejections.push(json!({
                "ts": e.ts, "task_id": e.task_id, "task_title": title,
                "by": e.payload["by"], "comment": e.payload["comment"].as_str()
                    .or_else(|| e.payload["error"].as_str()),
            })),
            "acceptance.recorded" => acceptance_records.push(json!({
                "ts": e.ts, "task_id": e.task_id, "task_title": title,
                "verdict": e.payload["verdict"], "by": e.payload["by"],
                "acceptor": e.payload["acceptor"], "comment": e.payload["comment"],
            })),
            "escalation.raised" => escalations.push(json!({
                "ts": e.ts, "task_id": e.task_id, "task_title": title,
                "reason": e.payload["reason"], "detail": e.payload["detail"],
                "last_error": e.payload["last_error"], "attempt": e.payload["attempt"],
            })),
            "task.status_changed" => {
                let to = e.payload["to"].as_str().unwrap_or("");
                if to == "suspended" {
                    suspensions.push(json!({
                        "ts": e.ts, "task_id": e.task_id, "task_title": title,
                        "reason": e.payload["reason"],
                    }));
                } else if to == "escalated" {
                    escalations.push(json!({
                        "ts": e.ts, "task_id": e.task_id, "task_title": title,
                        "reason": e.payload["reason"], "detail": "验收退回达上限，升级人工裁决",
                    }));
                }
            }
            _ => {}
        }
        if is_key_event(&e.kind) {
            timeline.push(json!({
                "ts": e.ts, "type": e.kind, "task_id": e.task_id, "task_title": title,
                "summary": event_summary(e),
            }));
        }
    }

    let artifacts: Vec<Value> = tasks
        .iter()
        .filter(|t| !t.artifacts.is_empty())
        .map(|t| json!({"task_id": t.id, "task_title": t.title, "artifacts": t.artifacts}))
        .collect();

    json!({
        "project": {
            "id": project.id,
            "name": project.name,
            "brief": project.brief,
            "status": project.status,
            "status_label": project_status_label(&project.status),
            "workspace": project.workspace,
            "created_at": project.created_at,
        },
        "generated_at": generated_at,
        "counts": {
            "total": tasks.len() as i64,
            "done": done,
            "cancelled": cancelled,
            "open": tasks.len() as i64 - done - cancelled,
            "by_status": by_status.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<_, _>>(),
        },
        "activity": {
            "attempt_sum": attempt_sum,
            "failure_sum": failure_sum,
            "rejection_sum": rejection_sum,
        },
        "milestones": milestones_json,
        "rejections": rejections,
        "escalations": escalations,
        "suspensions": suspensions,
        "acceptance_records": acceptance_records,
        "artifacts": artifacts,
        "events": {
            "total": events.len() as i64,
            "by_type": event_types.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<_, _>>(),
        },
        "timeline": timeline,
    })
}

fn is_key_event(kind: &str) -> bool {
    matches!(
        kind,
        "project.created" | "project.confirmed" | "project.completed" | "project.cancelled"
            | "project.reopened" | "project.paused" | "project.resumed"
    ) || kind.starts_with("milestone.")
        || kind.starts_with("replan.")
        || kind.starts_with("report.")
}

fn event_summary(e: &Event) -> String {
    let p = &e.payload;
    match e.kind.as_str() {
        "project.created" => format!("项目创建：{}", p["name"].as_str().unwrap_or("")),
        "project.confirmed" => "规划确认，项目进入运行".into(),
        "project.completed" => "全部任务终态且无未决 gate → 项目完成".into(),
        "project.cancelled" => "项目归档（取消）".into(),
        "project.reopened" => "项目从 completed 回到 running（有新工作）".into(),
        "milestone.created" => format!("里程碑创建：{}", p["name"].as_str().unwrap_or("")),
        "milestone.status_changed" => format!(
            "里程碑 {} 状态 {} → {}（{}）",
            p["name"].as_str().unwrap_or(""),
            p["from"].as_str().unwrap_or(""),
            p["to"].as_str().unwrap_or(""),
            p["reason"].as_str().unwrap_or("")
        ),
        "milestone.gate_opened" => format!("里程碑「{}」gate 打开，等待人工确认", p["name"].as_str().unwrap_or("")),
        "milestone.passed" => format!("里程碑「{}」gate 通过", p["name"].as_str().unwrap_or("")),
        "milestone.failed" => format!("里程碑「{}」gate 退回", p["name"].as_str().unwrap_or("")),
        "milestone.window_opened" => format!("里程碑「{}」重开重规划窗口", p["name"].as_str().unwrap_or("")),
        "milestone.window_applied" => format!(
            "里程碑窗口方案应用：{} 项已应用、{} 项跳过",
            p["applied"].as_array().map(|a| a.len()).unwrap_or(0),
            p["skipped"].as_array().map(|a| a.len()).unwrap_or(0)
        ),
        "milestone.window_resolved" => "里程碑重规划窗口关闭".into(),
        _ => String::new(),
    }
}

// ---------- Markdown 拼装 ----------

/// 拼装最终 Markdown 报告：规划者叙述 + 确定性事实章节。
pub fn compose_markdown(project: &Project, facts: &Value, narrative: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!("# 项目总结报告 · {}\n\n", project.name));
    s.push_str(&format!(
        "> 生成时间：{} ｜ 项目状态：{} ｜ 报告生成方式：事件溯源汇总 + planner 叙述\n\n",
        crate::report::fmt_ts(facts["generated_at"].as_i64().unwrap_or(0)),
        facts["project"]["status_label"].as_str().unwrap_or(""),
    ));

    // 一、概述（planner 叙述）
    s.push_str("## 一、报告概述\n\n");
    let n = narrative.trim();
    if n.is_empty() {
        s.push_str("（planner 未产出叙述）\n\n");
    } else {
        s.push_str(n);
        s.push_str("\n\n");
    }

    // 二、项目概况
    s.push_str("## 二、项目概况\n\n");
    s.push_str(&format!("- 项目名称：{}\n", project.name));
    s.push_str(&format!("- 项目目标：{}\n", if project.brief.trim().is_empty() { "（未填写）" } else { project.brief.trim() }));
    s.push_str(&format!(
        "- 项目状态：{}\n",
        facts["project"]["status_label"].as_str().unwrap_or(&project.status)
    ));
    if let Some(ws) = project.workspace.as_ref() {
        let desc = match ws["kind"].as_str().unwrap_or("") {
            "local" => format!("本地目录 {}", ws["path"].as_str().unwrap_or("")),
            "git" => format!(
                "Git 仓库 {}{}",
                ws["url"].as_str().unwrap_or(""),
                ws["branch"].as_str().filter(|b| !b.is_empty()).map(|b| format!(" @ {b}")).unwrap_or_default()
            ),
            _ => "未设置".into(),
        };
        s.push_str(&format!("- 工作区：{desc}\n"));
    } else {
        s.push_str("- 工作区：（未设置）\n");
    }
    s.push_str(&format!(
        "- 任务总数：{}（完成 {} / 取消 {} / 进行中 {}）\n",
        facts["counts"]["total"].as_i64().unwrap_or(0),
        facts["counts"]["done"].as_i64().unwrap_or(0),
        facts["counts"]["cancelled"].as_i64().unwrap_or(0),
        facts["counts"]["open"].as_i64().unwrap_or(0),
    ));
    s.push_str(&format!(
        "- 活动量：执行尝试 {} 次 ｜ 执行失败 {} 次 ｜ 验收退回 {} 次\n\n",
        facts["activity"]["attempt_sum"].as_i64().unwrap_or(0),
        facts["activity"]["failure_sum"].as_i64().unwrap_or(0),
        facts["activity"]["rejection_sum"].as_i64().unwrap_or(0),
    ));

    // 三、里程碑汇报
    s.push_str("## 三、里程碑汇报\n\n");
    let milestones = facts["milestones"].as_array().cloned().unwrap_or_default();
    if milestones.is_empty() {
        s.push_str("本项目未设置里程碑（无人工 gate 环节，全程自动调度）。\n\n");
    } else {
        for m in &milestones {
            s.push_str(&format!(
                "### {} · {}\n\n",
                m["name"].as_str().unwrap_or("里程碑"),
                m["gate_result"].as_str().unwrap_or("")
            ));
            s.push_str(&format!("- 阶段目标（goal）：{}\n", m["goal"].as_str().unwrap_or("（未填写）")));
            if let Some(effect) = m["effect"].as_str().filter(|e| !e.trim().is_empty()) {
                s.push_str(&format!("- 预期效果（effect）：{effect}\n"));
            }
            let members = m["members"].as_array().cloned().unwrap_or_default();
            let mdone = members.iter().filter(|t| t["status"] == "done").count();
            s.push_str(&format!("- 成员任务：{}/{} 完成\n", mdone, members.len()));
            for t in &members {
                s.push_str(&format!(
                    "  - [{}] {}（{}）\n",
                    t["status_label"].as_str().unwrap_or(""),
                    t["title"].as_str().unwrap_or(""),
                    t["id"].as_str().unwrap_or("")
                ));
            }
            s.push('\n');
        }
    }

    // 四、任务状态统计
    s.push_str("## 四、任务状态统计\n\n");
    s.push_str("| 状态 | 数量 |\n|---|---|\n");
    let by_status = facts["counts"]["by_status"].as_object().cloned().unwrap_or_default();
    if by_status.is_empty() {
        s.push_str("| （无任务） | 0 |\n");
    } else {
        let mut keys: Vec<&String> = by_status.keys().collect();
        keys.sort();
        for k in keys {
            s.push_str(&format!("| {} | {} |\n", task_status_label(k), by_status[k].as_i64().unwrap_or(0)));
        }
    }
    s.push('\n');

    // 五、退回 / 升级 / 挂起事件摘要
    s.push_str("## 五、退回 / 升级 / 挂起事件摘要\n\n");
    let rejections = facts["rejections"].as_array().cloned().unwrap_or_default();
    let escalations = facts["escalations"].as_array().cloned().unwrap_or_default();
    let suspensions = facts["suspensions"].as_array().cloned().unwrap_or_default();
    if rejections.is_empty() && escalations.is_empty() && suspensions.is_empty() {
        s.push_str("无退回 / 升级 / 挂起事件，全部任务一次通过或按正常流程完成。\n\n");
    } else {
        if !rejections.is_empty() {
            s.push_str(&format!("**验收退回（共 {} 次）**\n\n", rejections.len()));
            for r in &rejections {
                s.push_str(&format!(
                    "- [{}] {}{}：{}\n",
                    r["task_title"].as_str().unwrap_or(r["task_id"].as_str().unwrap_or("")),
                    r["by"].as_str().unwrap_or("agent"),
                    r["comment"].as_str().filter(|c| !c.is_empty()).map(|c| format!("退回意见：{c}")).unwrap_or_default(),
                    r["task_id"].as_str().unwrap_or("")
                ));
            }
            s.push('\n');
        }
        if !escalations.is_empty() {
            s.push_str(&format!("**升级（共 {} 次）**\n\n", escalations.len()));
            for e in &escalations {
                s.push_str(&format!(
                    "- {}｜reason={}{}\n",
                    e["task_title"].as_str().unwrap_or(e["task_id"].as_str().unwrap_or("")),
                    e["reason"].as_str().unwrap_or(""),
                    e["last_error"].as_str().filter(|v| !v.is_empty()).map(|v| format!("｜最近错误：{v}")).unwrap_or_default()
                ));
            }
            s.push('\n');
        }
        if !suspensions.is_empty() {
            s.push_str(&format!("**挂起（共 {} 次）**\n\n", suspensions.len()));
            for e in &suspensions {
                s.push_str(&format!(
                    "- {}｜reason={}\n",
                    e["task_title"].as_str().unwrap_or(e["task_id"].as_str().unwrap_or("")),
                    e["reason"].as_str().unwrap_or("")
                ));
            }
            s.push('\n');
        }
    }

    // 六、验收记录
    s.push_str("## 六、验收记录\n\n");
    let records = facts["acceptance_records"].as_array().cloned().unwrap_or_default();
    if records.is_empty() {
        s.push_str("暂无 agent/人工验收通过记录。\n\n");
    } else {
        for r in &records {
            s.push_str(&format!(
                "- [{}] {}（by={}{}）\n",
                r["verdict"].as_str().unwrap_or(""),
                r["task_title"].as_str().unwrap_or(r["task_id"].as_str().unwrap_or("")),
                r["by"].as_str().unwrap_or(""),
                r["acceptor"].as_str().filter(|a| !a.is_empty()).map(|a| format!("，acceptor={a}")).unwrap_or_default()
            ));
        }
        s.push('\n');
    }

    // 七、产物清单
    s.push_str("## 七、产物清单\n\n");
    let artifacts = facts["artifacts"].as_array().cloned().unwrap_or_default();
    if artifacts.is_empty() {
        s.push_str("本次交付未登记任务级产物（opencode 执行产物落在项目工作区，见上方工作区路径）。\n\n");
    } else {
        for a in &artifacts {
            let paths: Vec<String> = a["artifacts"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            s.push_str(&format!(
                "- {}（{}）：{}\n",
                a["task_title"].as_str().unwrap_or(""),
                a["task_id"].as_str().unwrap_or(""),
                paths.join("、")
            ));
        }
        s.push('\n');
    }
    s.push_str(&format!(
        "- 报告存档：数据目录 `projects/{}/summary-report.md`（另有 `summary-report.json` 事实快照）\n\n",
        project.id
    ));

    // 八、关键事件时间线
    s.push_str("## 八、关键事件时间线\n\n");
    let timeline = facts["timeline"].as_array().cloned().unwrap_or_default();
    if timeline.is_empty() {
        s.push_str("无关键事件。\n");
    } else {
        for t in &timeline {
            let sum = t["summary"].as_str().unwrap_or("");
            if sum.is_empty() {
                continue;
            }
            s.push_str(&format!(
                "- [{}] {}：{}\n",
                crate::report::fmt_ts(t["ts"].as_i64().unwrap_or(0)),
                t["type"].as_str().unwrap_or(""),
                sum
            ));
        }
    }
    s.push_str(&format!(
        "\n---\n\n_事件总数 {}，事件类型分布见附录。_\n",
        facts["events"]["total"].as_i64().unwrap_or(0)
    ));
    s
}

/// 时间戳 → `YYYY-MM-DD HH:MM:SS`（本地时区近似；报告展示用，不含时区后缀）。
pub fn fmt_ts(ms: i64) -> String {
    // 无 chrono 依赖：用简单 UTC 偏移换算（秒级），足够报告展示。
    let secs = ms / 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{sec:02}")
}

/// days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

// ---------- 落盘 ----------

/// 落盘：Markdown 报告 + JSON 事实快照。返回 Markdown 路径。
fn persist(project_id: &str, content: &str, facts: &Value) -> Result<String, String> {
    let dir = report_dir(project_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create report dir {}: {e}", dir.display()))?;
    let md = dir.join("summary-report.md");
    std::fs::write(&md, content).map_err(|e| format!("write {}: {e}", md.display()))?;
    if let Ok(js) = serde_json::to_string_pretty(facts) {
        let _ = std::fs::write(dir.join("summary-report.json"), js);
    }
    Ok(md.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Project {
        Project {
            id: "prj_1".into(),
            engineering_id: None,
            name: "演示项目".into(),
            brief: "交付一个可验收的东西".into(),
            status: "completed".into(),
            plan: None,
            workspace: Some(json!({"kind": "local", "path": "/tmp/ws"})),
            agent_pool: None,
            repos: Vec::new(),
            created_at: 0,
        }
    }

    fn task(id: &str, ms: Option<&str>, status: &str) -> Task {
        Task {
            id: id.into(),
            project_id: "prj_1".into(),
            milestone_id: ms.map(String::from),
            title: format!("任务{id}"),
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
            agent_pinned: false,
            tried_agents: serde_json::json!({}),
            status: status.into(),
            attempt: 1,
            rejection_count: 0,
            failure_count: 0,
            artifacts: if status == "done" { vec![format!("artifact://{id}")] } else { vec![] },
            created_at: 0,
        }
    }

    fn milestone(id: &str, status: &str) -> Milestone {
        Milestone {
            id: id.into(),
            project_id: "prj_1".into(),
            name: format!("里程碑{id}"),
            goal: Some(format!("{id} 的目标")),
            effect: None,
            task_ids: vec![],
            status: status.into(),
        }
    }

    fn event(kind: &str, task_id: Option<&str>, payload: Value) -> Event {
        Event { id: 0, ts: 1000, project_id: "prj_1".into(), task_id: task_id.map(String::from), kind: kind.into(), payload }
    }

    #[test]
    fn gate_result_labels_cover_all_statuses() {
        assert!(gate_result("passed").contains("通过"));
        assert!(gate_result("failed").contains("退回"));
        assert!(gate_result("awaiting_confirm").contains("待人工确认"));
        assert!(gate_result("pending").contains("未提交"));
    }

    #[test]
    fn facts_capture_counts_milestones_events_artifacts() {
        let tasks = vec![
            task("a", Some("m1"), "done"),
            task("b", Some("m1"), "cancelled"),
            task("c", Some("m2"), "done"),
        ];
        let ms = vec![milestone("m1", "passed"), milestone("m2", "failed")];
        let events = vec![
            event("acceptance.rejected", Some("b"), json!({"by": "agent", "comment": "缺文件"})),
            event("escalation.raised", Some("b"), json!({"reason": "rejection_limit", "detail": "d", "last_error": "e"})),
            event("task.status_changed", Some("b"), json!({"to": "suspended", "reason": "failure_limit"})),
            event("acceptance.recorded", Some("a"), json!({"verdict": "approved", "by": "agent"})),
            event("milestone.passed", None, json!({"name": "里程碑m1"})),
        ];
        let f = build_facts(&project(), &tasks, &ms, &events, 42);
        assert_eq!(f["counts"]["total"], 3);
        assert_eq!(f["counts"]["done"], 2);
        assert_eq!(f["counts"]["cancelled"], 1);
        assert_eq!(f["milestones"].as_array().unwrap().len(), 2);
        assert_eq!(f["milestones"][0]["gate_result"].as_str().unwrap(), gate_result("passed"));
        assert_eq!(f["rejections"].as_array().unwrap().len(), 1);
        assert_eq!(f["escalations"].as_array().unwrap().len(), 1);
        assert_eq!(f["suspensions"].as_array().unwrap().len(), 1);
        assert_eq!(f["acceptance_records"].as_array().unwrap().len(), 1);
        assert_eq!(f["artifacts"].as_array().unwrap().len(), 2);
        assert_eq!(f["events"]["total"], 5);
        assert_eq!(f["events"]["by_type"]["acceptance.rejected"], 1);
        assert!(f["timeline"].as_array().unwrap().iter().any(|t| t["type"] == "milestone.passed"));
    }

    #[test]
    fn markdown_covers_required_sections() {
        let tasks = vec![task("a", Some("m1"), "done"), task("b", Some("m1"), "done")];
        let ms = vec![milestone("m1", "passed")];
        let events = vec![event("acceptance.recorded", Some("a"), json!({"verdict": "approved", "by": "agent"}))];
        let facts = build_facts(&project(), &tasks, &ms, &events, 0);
        let md = compose_markdown(&project(), &facts, "这是规划者叙述。");
        for needle in [
            "项目总结报告",
            "报告概述",
            "这是规划者叙述。",
            "项目概况",
            "里程碑汇报",
            "m1 的目标",          // milestone goal
            "gate 已确认",        // gate result
            "任务状态统计",
            "退回 / 升级 / 挂起事件摘要",
            "验收记录",
            "产物清单",
            "artifact://a",       // artifact path list
            "关键事件时间线",
        ] {
            assert!(md.contains(needle), "report markdown missing {needle:?}\n{md}");
        }
    }

    #[test]
    fn timestamp_formatting_is_stable() {
        assert_eq!(fmt_ts(0), "1970-01-01 00:00:00");
        // 2021-01-01T00:00:00Z = 1609459200
        assert_eq!(fmt_ts(1_609_459_200_000), "2021-01-01 00:00:00");
    }
}
