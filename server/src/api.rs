//! HTTP 契约（REST + SSE）。
//!
//! - 错误响应统一 `{"error": "..."}`：`400` 入参非法 / `404` 不存在 /
//!   `409` 状态冲突（只读、重复确认、非法状态转移）/ `500` 内部错误。
//!   非法状态转移必须拒绝，不得静默纠正。
//! - 实时推送走 `/api/stream`（SSE），payload 为实体完整快照；前端靠它保持最新，**不得轮询**。
//! - 本层只做入参校验 + 调业务 + 组装响应；状态写入一律走 `events`（落库 + 审计 + 广播）。
//! - 字段与 `web/src/types.ts` 保持同步；新增字段可选 + 默认值，旧数据缺字段不崩。

use std::sync::Arc;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{sse::{Event as SseEvent, KeepAlive, Sse}, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;

use crate::{
    agents::{mock, opencode},
    db, events, knowledge,
    models::*,
    replan,
    report,
    state::App,
};

pub fn router(state: Arc<App>) -> Router {
    Router::new()
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/engineerings", get(list_engineerings).post(create_engineering))
        .route(
            "/api/engineerings/{id}",
            get(get_engineering_detail).patch(patch_engineering),
        )
        .route("/api/projects/{id}", get(get_project).patch(patch_project))
        .route("/api/projects/{id}/confirm", post(confirm_project))
        .route("/api/projects/{id}/pool", get(get_project_pool).put(put_project_pool))
        .route("/api/projects/{id}/plan", post(plan_project))
        .route("/api/projects/{id}/intervene", post(intervene_project))
        .route("/api/projects/{id}/intervene/apply", post(intervene_apply))
        .route("/api/projects/{id}/pause", post(pause_project))
        .route("/api/projects/{id}/resume", post(resume_project))
        .route("/api/projects/{id}/cancel", post(cancel_project))
        .route("/api/projects/{id}/report", get(get_project_report).post(generate_project_report))
        .route("/api/projects/{id}/tasks", post(create_task))
        .route("/api/projects/{id}/milestones", post(create_milestone))
        .route("/api/milestones/{id}", get(get_milestone_detail).patch(patch_milestone))
        .route("/api/milestones/{id}/confirm", post(confirm_milestone))
        .route("/api/milestones/{id}/fail", post(fail_milestone))
        .route("/api/milestones/{id}/window/apply", post(milestone_window_apply))
        .route("/api/tasks/{id}", get(get_task_detail).patch(patch_task))
        .route("/api/agent-outputs/{id}", get(get_agent_output))
        .route("/api/tasks/{id}/guidance", post(add_guidance))
        // M5 项目知识库：条目 CRUD + 提升为全局（全部写审计事件）
        .route("/api/projects/{id}/knowledge", get(list_project_knowledge).post(create_knowledge))
        .route("/api/knowledge/{id}", get(get_knowledge).patch(patch_knowledge).delete(delete_knowledge))
        .route("/api/knowledge/{id}/promote", post(promote_knowledge))
        .route("/api/tasks/{id}/accept", post(task_accept))
        .route("/api/tasks/{id}/reject", post(task_reject))
        .route("/api/tasks/{id}/replan", post(report_task_replan))
        .route("/api/agents", get(list_agents).post(create_agent))
        .route("/api/agents/{id}", get(|State(st): State<Arc<App>>, Path(id): Path<String>| async move {
            let c = st.db.lock().unwrap();
            match db::get_agent(&c, &id) {
                Some(a) => ok(a),
                None => err(StatusCode::NOT_FOUND, "agent not found"),
            }
        }).patch(patch_agent).delete(delete_agent))
        .route("/api/inbox", get(list_inbox))
        .route("/api/inbox/{id}/resolve", post(resolve_inbox))
        .route("/api/events", get(list_events))
        .route("/api/stream", get(stream))
        .with_state(state)
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    let msg = msg.into();
    tracing::warn!(%status, "api error: {msg}");
    (status, Json(json!({"error": msg}))).into_response()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max / 2).collect();
        let tail: String = s.chars().rev().take(max / 2).collect::<Vec<_>>().into_iter().rev().collect();
        format!("{head}\n……\n{tail}")
    }
}

fn ok<T: serde::Serialize>(v: T) -> Response {
    Json(v).into_response()
}

const ROLES: [&str; 3] = ["planner", "executor", "acceptor"];

/// 机械检查项结构校验（与 `verification::Check` 共用同一套结构）。
fn validate_check(v: &Value) -> Result<(), String> {
    serde_json::from_value::<crate::verification::Check>(v.clone())
        .map(|_| ())
        .map_err(|e| format!("acceptance_checks item invalid: {e}"))
}

// ---------- engineerings（工程层） ----------

async fn list_engineerings(State(st): State<Arc<App>>) -> Response {
    let c = st.db.lock().unwrap();
    ok(db::list_engineerings(&c))
}

async fn create_engineering(State(st): State<Arc<App>>, Json(body): Json<Value>) -> Response {
    let Some(name) = body["name"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "name is required");
    };
    // 方案 B：类型自动识别，不再由用户选择。锚点是 git 仓库根 → single；
    // 其余目录（多仓父目录、非 git 目录）→ plain（共享目录，串行）。
    let path = body["workspace_path"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    if let Some(p) = &path {
        let pb = std::path::Path::new(p);
        if !pb.is_dir() {
            return err(StatusCode::BAD_REQUEST, "workspace_path does not exist or is not a directory");
        }
    }
    let kind = path
        .as_deref()
        .map(|p| crate::sandbox::detect_workspace_kind(std::path::Path::new(p)))
        .unwrap_or("plain")
        .to_string();
    let pool = body.get("default_agent_pool").and_then(|v| v.as_array()).map(|a| {
        a.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>()
    });
    let e = Engineering {
        id: db::new_id("eng_"),
        name: name.to_string(),
        brief: body["brief"].as_str().unwrap_or("").to_string(),
        workspace_kind: kind,
        workspace_path: path,
        default_agent_pool: pool.filter(|p| !p.is_empty()),
        constraints: body["constraints"].as_str().map(String::from).filter(|s| !s.trim().is_empty()),
        created_at: db::now(),
    };
    {
        let c = st.db.lock().unwrap();
        db::insert_engineering(&c, &e);
        events::push_event(&st, &c, "", None, "engineering.created", json!({"engineering_id": e.id, "name": e.name}));
    }
    st.send("engineering", serde_json::to_value(&e).unwrap_or(json!({})));
    ok(e)
}

async fn get_engineering_detail(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(e) = db::get_engineering(&c, &id) else {
        return err(StatusCode::NOT_FOUND, "engineering not found");
    };
    ok(json!({ "engineering": e, "projects": db::list_projects_of(&c, &id) }))
}

async fn patch_engineering(
    State(st): State<Arc<App>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    // 自动识别需要跑 git，必须在数据库锁之外完成。
    let detected_kind = body
        .get("workspace_path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|p| std::path::Path::new(p).is_dir())
        .map(|p| crate::sandbox::detect_workspace_kind(std::path::Path::new(p)));
    let c = st.db.lock().unwrap();
    let Some(mut e) = db::get_engineering(&c, &id) else {
        return err(StatusCode::NOT_FOUND, "engineering not found");
    };
    if let Some(v) = body["name"].as_str() {
        if v.trim().is_empty() {
            return err(StatusCode::BAD_REQUEST, "name must not be empty");
        }
        e.name = v.trim().into();
    }
    if let Some(v) = body["brief"].as_str() {
        e.brief = v.into();
    }
    if let Some(v) = body.get("workspace_path") {
        e.workspace_path = v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        if let Some(p) = &e.workspace_path {
            if !std::path::Path::new(p).is_dir() {
                return err(StatusCode::BAD_REQUEST, "workspace_path does not exist or is not a directory");
            }
            // 路径变化时重新自动识别；旧 multi 值会在下一次保存时收敛为 single/plain。
            e.workspace_kind = detected_kind.unwrap_or("plain").into();
        } else {
            e.workspace_kind = "plain".into();
        }
    }
    if let Some(v) = body.get("constraints") {
        e.constraints = v.as_str().map(String::from).filter(|s| !s.trim().is_empty());
    }
    if let Some(v) = body.get("default_agent_pool") {
        e.default_agent_pool = v.as_array().map(|a| {
            a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()
        }).filter(|p| !p.is_empty());
    }
    db::update_engineering(&c, &e);
    events::push_event(&st, &c, "", None, "engineering.updated", json!({"engineering_id": e.id}));
    st.send("engineering", serde_json::to_value(&e).unwrap_or(json!({})));
    ok(e)
}

// ---------- projects ----------

async fn list_projects(State(st): State<Arc<App>>) -> Response {
    let c = st.db.lock().unwrap();
    ok(db::list_projects(&c))
}

async fn create_project(State(st): State<Arc<App>>, Json(body): Json<Value>) -> Response {
    let name = body["name"].as_str().filter(|s| !s.trim().is_empty());
    let Some(name) = name else { return err(StatusCode::BAD_REQUEST, "name is required"); };
    let brief = body["brief"].as_str().unwrap_or("");
    let engineering_id = body["engineering_id"]
        .as_str()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    // 工程是项目的归属容器：显式指定时必须存在
    let engineering = match &engineering_id {
        Some(eid) => {
            let c = st.db.lock().unwrap();
            match db::get_engineering(&c, eid) {
                Some(e) => Some(e),
                None => return err(StatusCode::BAD_REQUEST, "engineering_id not found"),
            }
        }
        None => None,
    };
    // 工程锚点：项目未显式给工作区时，默认落在工程工作区（共享目录 = 直接在该目录工作）
    let default_ws: Option<Value> = if body.get("workspace").map(|v| v.is_null()).unwrap_or(true) {
        engineering.as_ref().and_then(|e| {
            e.workspace_path.as_ref().map(|path| {
                let mode = if e.workspace_kind == "single" { "worktree" } else { "direct" };
                json!({"kind": "local", "path": path, "mode": mode})
            })
        })
    } else {
        None
    };
    let mut p = Project {
        id: db::new_id("prj_"),
        engineering_id,
        name: name.trim().into(),
        brief: brief.into(),
        status: "initializing".into(),
        plan: None,
        workspace: None,
        agent_pool: None,
        repos: Vec::new(),
        created_at: db::now(),
    };
    {
        let c = st.db.lock().unwrap();
        c.execute(
            "INSERT INTO projects (id,engineering_id,name,brief,status,plan_content,confirmed_at,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![p.id, p.engineering_id, p.name, p.brief, p.status, Option::<String>::None, Option::<i64>::None, p.created_at],
        ).unwrap();
        events::push_event(&st, &c, &p.id, None, "project.created",
            json!({"name": p.name, "brief": p.brief}));
        // M2.5: 创建时携带 agent 池（显式选择本项目可用的 agent；缺省=全部）
        if let Some(pool) = body.get("agent_pool").and_then(|v| v.as_array()) {
            let ids: Vec<String> = pool.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            if !ids.is_empty() {
                db::set_project_agent_pool(&c, &p.id, Some(&ids));
                p.agent_pool = Some(ids);
            }
        } else if let Some(e) = &engineering {
            // 工程默认团队：项目未显式选择时继承（团队配置逐层继承的最小实现）
            if let Some(pool) = &e.default_agent_pool {
                if !pool.is_empty() {
                    db::set_project_agent_pool(&c, &p.id, Some(pool));
                    p.agent_pool = Some(pool.clone());
                }
            }
        }
        // M2.5 修复：创建时携带的工作区直接落库（此前被忽略，逼用户再编辑一次）
        if let Some(ws_val) = body.get("workspace").filter(|v| !v.is_null()).or(default_ws.as_ref()) {
            {
                match validate_workspace(ws_val) {
                    Ok(Some(w)) => {
                        db::set_project_workspace(&c, &p.id, Some(&w));
                        p.workspace = serde_json::from_value(w).ok();
                    }
                    Ok(None) => {}
                    Err(e) => return err(StatusCode::BAD_REQUEST, e),
                }
            }
        }
    }
    st.wake();
    ok(p)
}

async fn get_project(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(p) = db::get_project(&c, &id) else { return err(StatusCode::NOT_FOUND, "project not found"); };
    // keep the stored task_ids mirror consistent with tasks.milestone_id
    db::sync_milestone_task_ids(&c, &id);
    let body = json!({
        "project": p,
        "tasks": db::list_tasks(&c, &id),
        "milestones": db::list_milestones(&c, &id),
        "agents": db::list_agents(&c),
        "inbox_open_count": db::inbox_open_count(&c, &id),
        "report": db::get_report(&c, &id).map(|r| report::summary_json(&r)),
    });
    ok(body)
}

/// M3: 项目总结报告详情（Markdown 正文 + 事实快照）。未生成时 `report: null`。
async fn get_project_report(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let c = st.db.lock().unwrap();
    if db::get_project(&c, &id).is_none() {
        return err(StatusCode::NOT_FOUND, "project not found");
    }
    match db::get_report(&c, &id) {
        Some(r) => ok(json!({ "report": r })),
        None => ok(json!({ "report": null })),
    }
}

/// M3: 手工（重新）生成项目总结报告 —— 失败重试入口。
/// 生成中重复触发 → 409；项目未完成 → 409（报告语义针对已完成项目）。
async fn generate_project_report(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(project) = db::get_project(&c, &id) else {
        return err(StatusCode::NOT_FOUND, "project not found");
    };
    if project.status != "completed" {
        return err(StatusCode::CONFLICT, "报告在项目完成后生成（当前项目未完成）");
    }
    match report::trigger(&st, &c, &project, "manual") {
        Ok(r) => ok(json!({ "report": r })),
        Err(e) => err(StatusCode::CONFLICT, e),
    }
}

/// M2: validate a workspace spec value. None (JSON null) clears it.
fn validate_workspace(v: &Value) -> Result<Option<Value>, String> {
    if v.is_null() {
        return Ok(None);
    }
    let Some(obj) = v.as_object() else { return Err("workspace must be an object or null".into()); };
    let kind = obj.get("kind").and_then(|k| k.as_str()).ok_or("workspace.kind must be 'local' or 'git'")?;
    match kind {
        "local" => {
            let path = obj.get("path").and_then(|p| p.as_str()).unwrap_or("");
            if path.trim().is_empty() {
                return Err("workspace.path is required for kind='local'".into());
            }
            Ok(Some(v.clone()))
        }
        "git" => {
            let url = obj.get("url").and_then(|u| u.as_str()).unwrap_or("");
            if url.trim().is_empty() {
                return Err("workspace.url is required for kind='git'".into());
            }
            Ok(Some(v.clone()))
        }
        other => Err(format!("workspace.kind must be 'local' or 'git' (got '{other}')")),
    }
}

async fn patch_project(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(mut p) = db::get_project(&c, &id) else { return err(StatusCode::NOT_FOUND, "project not found"); };
    if p.status == "archived" {
        return err(StatusCode::CONFLICT, "project is archived (read-only)");
    }
    // name/brief/workspace are all editable in any non-archived status:
    // the user may correct goals and workspace mid-flight (human-safety-taking-over).
    let mut workspace_json: Option<String> = None;
    if let Some(ws_val) = body.get("workspace") {
        match validate_workspace(ws_val) {
            Ok(w) => {
                p.workspace = w.clone();
                workspace_json = w.as_ref().map(|v| v.to_string());
            }
            Err(e) => return err(StatusCode::BAD_REQUEST, e),
        }
    }
    if let Some(n) = body["name"].as_str() { p.name = n.to_string(); }
    if let Some(b) = body["brief"].as_str() { p.brief = b.to_string(); }
    // 工程归属：可在非归档状态调整（null / 空串 = 解除归属）
    let mut eng_changed: Option<Option<String>> = None;
    if let Some(v) = body.get("engineering_id") {
        let eid = v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        if let Some(e) = &eid {
            if db::get_engineering(&c, e).is_none() {
                return err(StatusCode::BAD_REQUEST, "engineering_id not found");
            }
        }
        p.engineering_id = eid.clone();
        eng_changed = Some(eid);
    }
    // M2.5: agent 池（显式选择本项目可用 agent；null=全部）。
    // 只约束「新分配」（规划/介入/建任务）；已有任务指派不受影响。
    let mut pool_json: Option<Option<String>> = None;
    if !body["agent_pool"].is_null() {
        let ids: Vec<String> = body["agent_pool"].as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let ids = if ids.is_empty() { None } else { Some(ids) }; // 空数组=清空=全部
        if let Some(list) = &ids {
            for aid in list {
                if db::get_agent(&c, aid).is_none() {
                    return err(StatusCode::BAD_REQUEST, format!("agent_pool contains unknown agent: {aid}"));
                }
            }
        }
        pool_json = Some(ids.as_ref().map(|l| serde_json::to_string(l).unwrap_or_default()));
        p.agent_pool = ids;
    }
    // 注意：workspace/agent_pool 仅在请求显式携带时更新（否则会把未提交的字段清空）
    {
        let mut sets: Vec<String> = vec!["name=?2".into(), "brief=?3".into()];
        let mut params_v: Vec<Option<String>> = vec![Some(p.name.clone()), Some(p.brief.clone())];
        if body.get("workspace").is_some() {
            sets.push("workspace=?4".into());
            params_v.push(workspace_json);
        }
        if let Some(pj) = &pool_json {
            let idx = params_v.len() + 1;
            sets.push(format!("agent_pool=?{idx}"));
            params_v.push(pj.clone());
        }
        let sql = format!("UPDATE projects SET {} WHERE id=?1", sets.join(", "));
        let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&p.id];
        for v in &params_v {
            bind.push(v as &dyn rusqlite::ToSql);
        }
        let _ = c.execute(&sql, bind.as_slice());
    }
    if let Some(eid) = &eng_changed {
        db::set_project_engineering(&c, &p.id, eid.as_deref());
    }
    events::push_event(&st, &c, &p.id, None, "project.updated",
        json!({"name": p.name, "brief": p.brief, "workspace": p.workspace,
               "agent_pool": p.agent_pool, "engineering_id": p.engineering_id}));
    drop(c);
    st.wake();
    ok(p)
}

/// 项目已「全部任务完成」→ completed 后又来了新任务 / 新裁定请求：
/// 自动回到 running，否则调度器（只扫描 running）会忽略这批新工作。
/// 调用方必须已持有 db 锁。
fn reopen_if_completed(st: &Arc<App>, c: &rusqlite::Connection, id: &str) {
    if db::get_project(c, id).map(|p| p.status) != Some("completed".into()) {
        return;
    }
    let _ = c.execute("UPDATE projects SET status='running' WHERE id=?1", rusqlite::params![id]);
    events::push_event(st, c, id, None, "project.reopened",
        serde_json::json!({"from": "completed", "to": "running"}));
    if let Some(p) = db::get_project(c, id) {
        events::send_project(st, &p);
    }
    tracing::info!(project = %id, "project reopened (completed → running) after new work arrived");
}

/// M4：DOMO 管辖的项目工作区根（每个项目一个子目录）。
/// 可用 DOMO_WORKSPACE_ROOT 覆盖，默认 ~/.domo/workspace。
pub fn workspace_root() -> std::path::PathBuf {
    let root = std::env::var("DOMO_WORKSPACE_ROOT").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.domo/workspace")
    });
    std::path::PathBuf::from(root)
}

pub fn default_workspace_dir(project_id: &str) -> std::path::PathBuf {
    workspace_root().join(project_id)
}

fn set_project_status(st: &Arc<App>, id: &str, status: &str, what: &str) -> Response {
    let c = st.db.lock().unwrap();
    let Some(old) = db::get_project(&c, id) else { return err(StatusCode::NOT_FOUND, "project not found"); };
    let _ = c.execute("UPDATE projects SET status=?2 WHERE id=?1", rusqlite::params![id, status]);
    if what == "confirmed" {
        let _ = c.execute("UPDATE projects SET confirmed_at=?2 WHERE id=?1", rusqlite::params![id, db::now()]);
    }
    let p = db::get_project(&c, id).unwrap();
    events::push_event(&st, &c, id, None, &format!("project.{what}"),
        json!({"from": old.status, "to": status}));
    drop(c);
    st.wake();
    ok(p)
}

async fn confirm_project(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    // 方案 B：不再切项目分支。共享目录（非单仓 worktree）只做项目级串行检查。
    let (project, engineering_kind): (Project, Option<String>) = {
        let c = st.db.lock().unwrap();
        let Some(project) = db::get_project(&c, &id) else {
            return err(StatusCode::NOT_FOUND, "project not found");
        };
        match project.status.as_str() {
            "initializing" | "pending_confirm" => {}
            _ => return err(StatusCode::CONFLICT, "project already confirmed"),
        }
        let kind = project
            .engineering_id
            .as_deref()
            .and_then(|eid| db::get_engineering(&c, eid))
            .map(|e| e.workspace_kind);
        (project, kind)
    };
    let ws: Option<Value> = project.workspace.clone();
    // M2 workspace handling (DESIGN §13):
    // local → the path must exist (400, project stays initializing);
    // git → clone into data/workspaces/<prj_id>/ at confirm time (already
    // cloned → skip / reuse). Failed clone → 500 with stderr, project stays
    // initializing (retryable).
    // M4：无需手动指定工作区——缺省自动使用 ~/.domo/workspace/<project_id>/（自动创建）。
    // 显式 local / git 仍然支持并优先生效。
    let ws = match ws {
        Some(w) => w,
        None => {
            let dir = default_workspace_dir(&id);
            if let Err(e) = std::fs::create_dir_all(&dir) {
                return err(StatusCode::INTERNAL_SERVER_ERROR, format!("cannot create default workspace {}: {e}", dir.display()));
            }
            let w = json!({"kind": "local", "path": dir.display().to_string(), "default": true});
            let c = st.db.lock().unwrap();
            db::set_project_workspace(&c, &id, Some(&w));
            events::push_event(&st, &c, &id, None, "project.workspace_default",
                json!({"path": dir.display().to_string()}));
            w
        }
    };
    let w = &ws;
    match w["kind"].as_str().unwrap_or("") {
            "local" => {
                let path = w["path"].as_str().unwrap_or("");
                if path.is_empty() || !std::path::Path::new(path).is_dir() {
                    return err(StatusCode::BAD_REQUEST, format!("workspace path does not exist or is not a directory: {path}"));
                }
            }
            "git" => {
                let url = w["url"].as_str().unwrap_or("").to_string();
                if url.trim().is_empty() {
                    return err(StatusCode::BAD_REQUEST, "workspace.url is empty");
                }
                let dir = std::path::PathBuf::from(format!("data/workspaces/{id}"));
                if !dir.exists() {
                    if let Some(parent) = dir.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let mut cmd = tokio::process::Command::new("git");
                    cmd.arg("clone").arg(&url);
                    if let Some(b) = w["branch"].as_str().filter(|b| !b.is_empty()) {
                        cmd.args(["--branch", b]);
                    }
                    cmd.arg(&dir);
                    match cmd.output().await {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => {
                            let stderr = String::from_utf8_lossy(&o.stderr);
                            return err(StatusCode::INTERNAL_SERVER_ERROR, format!("git clone failed: {}", stderr.trim()));
                        }
                        Err(e) => {
                            return err(StatusCode::INTERNAL_SERVER_ERROR, format!("failed to run git clone: {e}"));
                        }
                    }
                    let c = st.db.lock().unwrap();
                    events::push_event(&st, &c, &id, None, "project.workspace_cloned",
                        json!({"url": url, "branch": w["branch"].as_str().unwrap_or("")}));
                }
            }
            other => return err(StatusCode::BAD_REQUEST, format!("workspace.kind must be 'local' or 'git' (got '{other}')")),
    }
    // 工作目录：工作区属于工程（共享目录 = 工程目录），扫描 git 仓库在锁外执行。
    let root = {
        let c = st.db.lock().unwrap();
        db::project_workdir(&c, &project)
    };
    let Some(root) = root else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "project has no working directory");
    };
    let repos: Vec<crate::models::RepoRef> = crate::sandbox::scan_repos(&root)
        .into_iter()
        .map(|(rel, main)| crate::models::RepoRef { rel, main })
        .collect();
    {
        let c = st.db.lock().unwrap();
        if !repos.is_empty() {
            db::set_project_repos(&c, &id, &repos);
            events::push_event(&st, &c, &id, None, "project.repos_scanned",
                json!({"repos": repos.iter().map(|r| json!({"rel": r.rel, "main": r.main})).collect::<Vec<_>>()}));
        }
        let p2 = db::get_project(&c, &id).unwrap_or(project);
        let shared = engineering_kind.as_deref() != Some("single")
            && !(engineering_kind.is_none() && p2.repos.len() == 1 && p2.repos[0].rel == ".");
        if shared {
            if let Some(eid) = p2.engineering_id.as_deref() {
                let busy = db::list_projects_of(&c, eid)
                    .into_iter()
                    .any(|x| x.id != id && (x.status == "running" || x.status == "pausing"));
                if busy {
                    return err(StatusCode::CONFLICT,
                        "共享目录工程同一时刻只能运行一个项目：请先暂停或归档其他项目");
                }
            }
        }
    }
    set_project_status(&st, &id, "running", "confirmed")
}

// ---------- planning (M2) ----------

#[derive(Debug, Clone)]
struct PlannedTask {
    title: String,
    description: String,
    dep_idx: Vec<usize>,
    acceptance_required: bool,
    criteria: Option<String>,
    /// M4：任务涉及的仓库（相对工作区路径），planner 可声明
    scope_repos: Vec<String>,
    /// M7：机械检查项（结构化，schema 与 verification::Check 一致）
    checks: Vec<Value>,
}

/// 规划产出的里程碑分组（M3）：planner 可以用里程碑描述项目阶段。
/// `task_idx` 为其成员任务在 `tasks` 数组中的下标。
#[derive(Debug, Clone)]
struct PlannedMilestone {
    name: String,
    goal: Option<String>,
    effect: Option<String>,
    task_idx: Vec<usize>,
}

/// Extract the outermost JSON object from LLM output (tolerates code fences
/// and surrounding prose): substring from the first `{` to the last `}`.
/// 严格的 JSON 提取：planner（LLM）输出常混入解释文字或第 2 个 JSON 对象。
/// 依次尝试：```json 围栏块 → 整串 → 首个 { 到末个 } 区间 → 每个 { 与后续每个
/// } 两两组合的全部子串。返回所有有效 JSON 候选（去重、越长越靠前）。
fn extract_json_candidates(raw: &str) -> Vec<String> {
    let s = raw.trim();
    let mut cands: Vec<String> = Vec::new();

    // 1) fenced ```json blocks
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(p) = s[i..].find("```") {
            let open = i + p;
            let after = open + 3;
            let content = s[after..]
                .find('\n')
                .map(|n| after + n + 1)
                .unwrap_or(after);
            // skip language tag on same line
            let content = if s[content.min(s.len())..].starts_with('{') {
                content
            } else if let Some(j) = s[content.min(s.len())..].find('{') {
                content.min(s.len()) + j
            } else {
                i = after;
                continue;
            };
            match s[content..].find("```") {
                Some(q) => {
                    cands.push(s[content..content + q].trim().to_string());
                    i = content + q + 3;
                }
                None => i = content.max(i + 1),
            }
        } else {
            break;
        }
    }

    // 2) whole string
    cands.push(s.to_string());

    // 3) first { .. last }
    if let (Some(a), Some(b)) = (s.find('{'), s.rfind('}')) {
        if b > a {
            cands.push(s[a..=b].to_string());
        }
    }

    // 4) every { × every later } combination, keep only parseable ones
    let opens: Vec<usize> = s
        .char_indices()
        .filter(|(_, c)| *c == '{')
        .map(|(i, _)| i)
        .collect();
    let closes: Vec<usize> = s
        .char_indices()
        .filter(|(_, c)| *c == '}')
        .map(|(i, _)| i)
        .collect();
    for &a in &opens {
        for &b in &closes {
            if b > a {
                let sub = s[a..=b].to_string();
                if serde_json::from_str::<Value>(&sub).is_ok() {
                    cands.push(sub);
                }
            }
        }
    }

    // dedupe, longest first
    cands.sort_by(|a, b| b.len().cmp(&a.len()));
    cands.dedup();
    cands
}

/// 提取第一个可解析的 JSON 顶层对象（Value），供 plan 与介入解析共用。
/// 供 replan 模块解析 agent 输出中的 REPLAN 块。
pub(crate) fn extract_json_value_pub(raw: &str) -> Option<Value> {
    extract_json_value(raw)
}

pub(crate) fn extract_json_value(raw: &str) -> Option<Value> {
    extract_json_candidates(raw)
        .into_iter()
        .find_map(|c| serde_json::from_str::<Value>(&c).ok())
}

fn parse_plan_json(raw: &str) -> Result<(Vec<PlannedTask>, Vec<PlannedMilestone>), String> {
    let v = extract_json_value(raw)
        .ok_or_else(|| "planner output contains no parseable JSON object".to_string())?;
    let tasks = v
        .get("tasks")
        .and_then(|t| t.as_array())
        .ok_or("planner JSON must contain a 'tasks' array")?;
    let mut out = Vec::new();
    for (i, item) in tasks.iter().enumerate() {
        let title = item["title"].as_str().unwrap_or("").trim().to_string();
        if title.is_empty() {
            return Err(format!("planner task #{i} has no title"));
        }
        let dep_idx = match item["depends_on"].as_array() {
            Some(arr) => {
                let mut idx = Vec::new();
                for d in arr {
                    let n = d.as_u64().ok_or(format!("planner: depends_on of task #{i} must be an integer index"))?;
                    let n = n as usize;
                    if n >= tasks.len() {
                        return Err(format!("planner: depends_on index {n} of task #{i} is out of range ({} tasks)", tasks.len()));
                    }
                    idx.push(n);
                }
                idx
            }
            None => Vec::new(),
        };
        let checks = match item.get("acceptance_checks") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => {
                let mut v = Vec::new();
                for (j, chk) in a.iter().enumerate() {
                    if let Err(e) = validate_check(chk) {
                        return Err(format!(
                            "planner: acceptance_checks[{j}] of task #{i} is invalid: {e}（只允许 file_exists / file_not_exists / command）"
                        ));
                    }
                    v.push(chk.clone());
                }
                v
            }
            Some(_) => {
                return Err(format!("planner: acceptance_checks of task #{i} must be an array"));
            }
        };
        out.push(PlannedTask {
            title,
            description: item["description"].as_str().unwrap_or("").to_string(),
            dep_idx,
            acceptance_required: item["acceptance_required"].as_bool().unwrap_or(true),
            criteria: item["acceptance_criteria"].as_str().map(String::from),
            scope_repos: item["scope_repos"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            checks,
        });
    }

    // 里程碑分组（M3，可选）：top-level `milestones` 与任务级 `milestone` 字符串两种形态
    // 都支持，按名称合并；下标越界 / 无名里程碑 / 空成员里程碑被安全忽略，
    // 绝不让里程碑解析失败拖垮整份规划。
    fn idx_of_name(milestones: &[PlannedMilestone], name: &str) -> Option<usize> {
        milestones.iter().position(|m| m.name == name)
    }
    let n = tasks.len();
    let mut milestones: Vec<PlannedMilestone> = Vec::new();
    if let Some(arr) = v.get("milestones").and_then(|m| m.as_array()) {
        for item in arr {
            let name = item["name"].as_str().map(str::trim).filter(|s| !s.is_empty());
            let Some(name) = name else { continue };
            let indices = item
                .get("tasks")
                .or_else(|| item.get("task_indices"))
                .or_else(|| item.get("task_idx"))
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|d| d.as_u64().map(|u| u as usize))
                        .filter(|k| *k < n)
                        .collect::<Vec<usize>>()
                })
                .unwrap_or_default();
            match idx_of_name(&milestones, name) {
                Some(k) => milestones[k].task_idx.extend(indices),
                None => milestones.push(PlannedMilestone {
                    name: name.to_string(),
                    goal: item["goal"].as_str().map(String::from),
                    effect: item["effect"].as_str().map(String::from),
                    task_idx: indices,
                }),
            }
        }
    }
    for (i, item) in tasks.iter().enumerate() {
        if let Some(name) = item
            .get("milestone")
            .and_then(|m| m.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match idx_of_name(&milestones, name) {
                Some(k) => milestones[k].task_idx.push(i),
                None => milestones.push(PlannedMilestone {
                    name: name.to_string(),
                    goal: None,
                    effect: None,
                    task_idx: vec![i],
                }),
            }
        }
    }
    for m in milestones.iter_mut() {
        m.task_idx.sort_unstable();
        m.task_idx.dedup();
    }
    milestones.retain(|m| !m.task_idx.is_empty());
    Ok((out, milestones))
}

/// Round-robin pick among role candidates; `avoid` skips one agent instance
/// (the same instance may not execute and accept the same task).

fn plan_session_end(
    state: &Arc<App>, project_id: &str, agent_id: &str, session: &str,
    result: &str, detail: Option<String>,
) {
    let c = state.db.lock().unwrap();
    events::push_event(state, &c, project_id, None, "agent.session_ended",
        serde_json::json!({"agent_id": agent_id, "phase": "plan", "session_id": session,
                           "result": result, "detail": detail.unwrap_or_else(|| "规划会话结束".into())}));
}

/// M4.5：项目 agent 池视图（表格数据源：agent × 角色 + 熔断状态）。
async fn get_project_pool(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let c = st.db.lock().unwrap();
    if db::get_project(&c, &id).is_none() {
        return err(StatusCode::NOT_FOUND, "project not found");
    }
    ok(crate::pool::pool_view(&st, &c, &id))
}

/// M4.5：写入项目 agent 池（角色 → 有序 agent id 数组；缺省=自动全量）。
/// body: {"executor": ["agt_a","agt_b"], "acceptor": [...]} 或 {"agent_pool": {...}}
async fn put_project_pool(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let map = body.get("agent_pool").cloned().unwrap_or(body.clone());
    let Some(obj) = map.as_object() else {
        return err(StatusCode::BAD_REQUEST, "pool must be an object: {role: [agent_id...]}");
    };
    let mut pool: std::collections::HashMap<String, Vec<String>> = Default::default();
    for (role, list) in obj {
        if !ROLES.contains(&role.as_str()) {
            return err(StatusCode::BAD_REQUEST, format!("unknown role: {role}"));
        }
        let ids: Vec<String> = list
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        pool.insert(role.clone(), ids);
    }
    {
        let c = st.db.lock().unwrap();
        let Some(p) = db::get_project(&c, &id) else { return err(StatusCode::NOT_FOUND, "project not found"); };
        if p.status == "archived" {
            return err(StatusCode::CONFLICT, "project is archived (read-only)");
        }
        for ids in pool.values() {
            for aid in ids {
                let Some(a) = db::get_agent(&c, aid) else {
                    return err(StatusCode::BAD_REQUEST, format!("unknown agent in pool: {aid}"));
                };
                if a.status == "deleted" {
                    return err(StatusCode::BAD_REQUEST, format!("agent {} is deleted", a.name));
                }
            }
        }
        db::set_project_pool(&c, &id, &pool);
        events::push_event(&st, &c, &id, None, "project.pool_updated",
            json!({"pool": serde_json::to_value(&pool).unwrap_or(json!({}))}));
    }
    st.wake();
    let c = st.db.lock().unwrap();
    ok(crate::pool::pool_view(&st, &c, &id))
}

async fn plan_project(
    State(st): State<Arc<App>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(e) = body["engine"].as_str() {
        if e != "opencode" {
            return err(StatusCode::BAD_REQUEST, "only the 'opencode' engine is supported");
        }
    }
    let (project, agents) = {
        let c = st.db.lock().unwrap();
        let Some(project) = db::get_project(&c, &id) else {
            return err(StatusCode::NOT_FOUND, "project not found");
        };
        if project.status != "initializing" {
            return err(StatusCode::CONFLICT, "planning is only available while the project is initializing");
        }
        // M4：workspace 可缺省（确认时自动落到 ~/.domo/workspace/<prj>/），规划阶段不强制
        (project, db::list_agents(&c))
    };

    // M2.5: 指定执行/验收/规划 agent 的提前校验（在派 planner 之前，省一次 LLM 调用）
    // 项目 agent 池：约束本次规划的分配范围（None=全部）
    let pool = project.agent_pool.clone();
    let in_pool = |id: &str| pool.as_ref().map(|p| p.iter().any(|x| x == id)).unwrap_or(true);
    let forced_executor: Option<String> = body["executor_agent_id"].as_str().map(String::from);
    if let Some(fid) = &forced_executor {
        let a = db::get_agent(&st.db.lock().unwrap(), fid);
        let ok = a.as_ref().map(|a| a.roles.iter().any(|r| r == "executor") && in_pool(&a.id)).unwrap_or(false);
        if !ok {
            return err(StatusCode::BAD_REQUEST, "executor_agent_id is not an executor-capable agent or not in the project agent pool");
        }
    }
    let forced_acceptor: Option<String> = body["acceptor_agent_id"].as_str().map(String::from);
    if let Some(fid) = &forced_acceptor {
        let a = db::get_agent(&st.db.lock().unwrap(), fid);
        let ok = a.as_ref().map(|a| a.roles.iter().any(|r| r == "acceptor") && in_pool(&a.id)).unwrap_or(false);
        if !ok {
            return err(StatusCode::BAD_REQUEST, "acceptor_agent_id is not an acceptor-capable agent or not in the project agent pool");
        }
        if forced_executor.as_deref() == Some(fid) {
            return err(StatusCode::BAD_REQUEST, "acceptor must differ from the executor");
        }
    }
    // 指定规划者（缺省=第一个 opencode+planner 实例，其次任意 planner 实例含 mock）
    let forced_planner: Option<String> = body["planner_agent_id"].as_str().map(String::from);
    if let Some(fid) = &forced_planner {
        let ok = db::get_agent(&st.db.lock().unwrap(), fid)
            .map(|a| a.roles.iter().any(|r| r == "planner"))
            .unwrap_or(false);
        if !ok {
            return err(StatusCode::BAD_REQUEST, "planner_agent_id is not an agent with the 'planner' role");
        }
    }

    let planner = match &forced_planner {
        Some(fid) => agents.iter().find(|a| &a.id == fid),
        None => agents
            .iter()
            .find(|a| a.adapter == "opencode" && a.roles.iter().any(|r| r == "planner"))
            // M5：没有 opencode planner 时，允许 mock planner 兜底（本地/冒烟确定性规划）
            .or_else(|| agents.iter().find(|a| a.roles.iter().any(|r| r == "planner"))),
    };
    let Some(planner) = planner else {
        return err(StatusCode::BAD_REQUEST, "no agent with the 'planner' role is registered; add one to use automatic planning");
    };
    let planner_id = planner.id.clone();
    let planner_is_mock = planner.adapter != "opencode";
    let session = opencode::plan_session(&id);

    {
        let c = st.db.lock().unwrap();
        events::push_event(&st, &c, &id, None, "agent.session_started",
            serde_json::json!({"agent_id": planner.id, "phase": "plan", "session_id": session,
                               "purpose": "生成项目规划（planner 会话）", "trigger": "human"}));
    }

    // M4.5：重新规划 = **原子替换**——规划成功解析后才在事务里删除旧 planned 任务/里程碑
    // 并插入新计划；规划失败则旧计划原封不动（修复此前「先删后规划」导致的丢计划事故）。
    let feedback = body["feedback"].as_str().map(str::trim).filter(|s| !s.is_empty());
    let replacing: bool = {
        let c = st.db.lock().unwrap();
        let existing = db::list_tasks(&c, &id);
        let started = existing.iter().any(|t| !matches!(t.status.as_str(), "planned" | "ready"));
        if started {
            return err(StatusCode::CONFLICT, "cannot replan: some tasks already started; use intervention instead");
        }
        if feedback.is_some() {
            events::push_event(&st, &c, &id, None, "planner.replan_requested",
                json!({"feedback": feedback, "will_replace_tasks": existing.len(),
                       "note": "规划成功后才替换（原子），失败则保留原计划"}));
        }
        !existing.is_empty()
    };

    // M5：planner 提示词注入项目规范段（高优先级），并落 `prompt.assembled` 审计。
    let mut prompt = crate::knowledge::assemble(&st, &id, None, "planner", "plan", |k| {
        let mut p = opencode::plan_prompt(&project, &agents, k);
        if let Some(fb) = feedback {
            p.push_str(&format!(
                "\n\n【人工修改意见（必须据此重做规划）】\n{fb}\n\n请输出**完整**的新规划（全部任务与里程碑），覆盖上一版；不要只输出差异。\n"
            ));
        }
        p
    });
    // mock planner 兜底：不调用 CLI，产出确定性计划（供本地/冒烟端到端走通规划链路）。
    if planner_is_mock {
        prompt = mock::plan_json(&project);
    }
    let planner_model = opencode::model_of(planner);
    // opencode planner 走真实 CLI；mock planner 已在上面产出确定性计划文本。
    let plan_text: String = if planner_is_mock {
        prompt
    } else {
        let call = opencode::run_cli(
            &st,
            None,
            None,
            &session,
            planner_model.as_deref(),
            opencode::PLAN_TIMEOUT_MS, // hard timeout for a single plan call
            &prompt,
        );
        match call.await {
            Ok(o) => o.text,
            Err(e) => {
                plan_session_end(&st, &id, &planner_id, &session, "error", Some(e.clone()));
                return err(StatusCode::INTERNAL_SERVER_ERROR, e);
            }
        }
    };

    let (planned, planned_milestones) = match parse_plan_json(&plan_text) {
        Ok(p) => p,
        Err(e) => {
            plan_session_end(&st, &id, &planner_id, &session, "error", Some(e.clone()));
            // 服务端日志保留完整原始输出，便于事后排障
            tracing::warn!(target: "planner", project = %id, raw = %truncate(&plan_text, 2000), "planner JSON parse failed");
            let tail: String = plan_text.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect();
            return err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}; output tail: {tail}"));
        }
    };
    tracing::info!(target: "planner", project = %id, tasks = planned.len(), milestones = planned_milestones.len(), "planner proposal parsed");
    plan_session_end(&st, &id, &planner_id, &session, "success", None);

    if planned.is_empty() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "planner produced no tasks");
    }

    // Role assignment by contract: executor rotation among executor-role
    // instances; acceptor among acceptor-role instances != the task's executor.
    // Missing roles → 400, nothing is written.
    // M4.5：规划**不绑定 agent**——任务执行时按「角色 + 项目池优先级」动态解析；
    // 只有人类显式指定（规划台选择 / 任务编辑）才固定（agent_pinned）。
    let assigned: Vec<(PlannedTask, Option<String>, Option<String>)> = planned
        .into_iter()
        .map(|pt| {
            let acceptor = if pt.acceptance_required { forced_acceptor.clone() } else { None };
            (pt, forced_executor.clone(), acceptor)
        })
        .collect();

    // Insert inside one transaction so the batch lands atomically: out-of-range
    // deps are rejected up front, cycles fall back to db::deps_introduce_cycle;
    // any failure rolls the whole proposal back.
    let mut created: Vec<Task> = {
        let c = st.db.lock().unwrap();
        let run = || -> Result<Vec<Task>, Response> {
            let tx = c.unchecked_transaction()
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("begin transaction: {e}")))?;
            // M4.5：原子替换——旧计划在此事务内删除，与插入同生共死
            if replacing {
                for t in db::list_tasks(&tx, &id) {
                    let _ = tx.execute("DELETE FROM logs WHERE task_id=?1", rusqlite::params![t.id]);
                    let _ = tx.execute("DELETE FROM guidance WHERE task_id=?1", rusqlite::params![t.id]);
                    let _ = tx.execute("DELETE FROM tasks WHERE id=?1", rusqlite::params![t.id]);
                }
                let _ = tx.execute("DELETE FROM milestones WHERE project_id=?1", rusqlite::params![id]);
            }
            let mut created: Vec<Task> = Vec::new();
            for (i, (pt, executor, acceptor)) in assigned.iter().enumerate() {
                let dep_ids: Vec<String> = pt.dep_idx.iter().map(|k| created[*k].id.clone()).collect();
                let t = Task {
                    id: db::new_id("tsk_"),
                    project_id: id.clone(),
                    milestone_id: None,
                    title: pt.title.clone(),
                    description: pt.description.clone(),
                    depends_on: dep_ids.clone(),
                    assignee_role: "executor".into(),
                    executor_agent_id: executor.clone(),
                    acceptor_agent_id: acceptor.clone(),
                    acceptance_required: pt.acceptance_required,
                    acceptance_criteria: pt.criteria.clone(),
                    execution_plan: None,
                    plan_rejection_count: 0,
                    scope_repos: pt.scope_repos.clone(),
                    agent_pinned: executor.is_some(),
                    tried_agents: serde_json::json!({}),
                    status: "planned".into(),
                    version: 0,
                    attempt: 0,
                    rejection_count: 0,
                    failure_count: 0,
                    artifacts: vec![],
                    created_at: db::now(),
                };
                c.execute(
                    "INSERT INTO tasks (id,project_id,milestone_id,title,description,depends_on,assignee_role,executor_agent_id,acceptor_agent_id,acceptance_required,acceptance_criteria,status,attempt,rejection_count,failure_count,artifacts,created_at,agent_pinned)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
                    rusqlite::params![
                        t.id, t.project_id, t.milestone_id, t.title, t.description,
                        db::jstring(&Value::Array(t.depends_on.iter().map(|d| Value::String(d.clone())).collect())),
                        t.assignee_role, t.executor_agent_id, t.acceptor_agent_id,
                        t.acceptance_required as i64, t.acceptance_criteria,
                        t.status, t.attempt, t.rejection_count, t.failure_count,
                        db::jstring(&Value::Array(Vec::new())), t.created_at, t.agent_pinned as i64
                    ],
                ).map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("insert planned task: {e}")))?;
                if !pt.checks.is_empty() {
                    db::set_task_checks(&tx, &t.id, &pt.checks);
                }
                if !t.scope_repos.is_empty() {
                    db::set_task_scope_repos(&tx, &t.id, &t.scope_repos);
                }
                if db::deps_introduce_cycle(&tx, &id, Some(&t.id), &dep_ids) {
                    return Err(err(StatusCode::INTERNAL_SERVER_ERROR, format!(
                        "planner produced a dependency cycle (task #{i}: '{}'); plan rolled back", pt.title
                    )));
                }
                created.push(t);
            }
            tx.commit()
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("commit planned tasks: {e}")))?;
            Ok(created)
        };
        match run() {
            Ok(v) => v,
            Err(resp) => return resp,
        }
    };

    // M3：落地 planner 规划出的里程碑（可空）。成员按下标映射到刚创建的 tasks，
    // 越界下标已在解析阶段剔除，因此这里必然安全。里程碑先 pending，待成员全部
    // DONE 后由调度器推入 gate。
    let created_milestones: Vec<Milestone> = {
        let c = st.db.lock().unwrap();
        let mut ms: Vec<Milestone> = Vec::new();
        for pm in &planned_milestones {
            let m = Milestone {
                id: db::new_id("mst_"),
                project_id: id.clone(),
                name: pm.name.clone(),
                goal: pm.goal.clone(),
                effect: pm.effect.clone(),
                task_ids: vec![],
                status: "pending".into(),
            };
            db::insert_milestone(&c, &m);
            for k in &pm.task_idx {
                if let Some(t) = created.get_mut(*k) {
                    db::set_task_milestone(&c, &t.id, Some(&m.id));
                    t.milestone_id = Some(m.id.clone());
                }
            }
            ms.push(m);
        }
        if !ms.is_empty() {
            db::sync_milestone_task_ids(&c, &id);
        }
        let mut fresh_ms: Vec<Milestone> = Vec::new();
        for m in &ms {
            if let Some(fresh) = db::get_milestone(&c, &m.id) {
                events::push_event(&st, &c, &id, None, "milestone.created",
                    serde_json::json!({"milestone_id": fresh.id, "name": fresh.name,
                        "goal": fresh.goal, "effect": fresh.effect, "task_ids": fresh.task_ids,
                        "source": "planner"}));
                events::send_milestone(&st, &fresh);
                fresh_ms.push(fresh);
            }
        }
        fresh_ms
    };
    {
        let c = st.db.lock().unwrap();
        for t in &created {
            events::push_event(&st, &c, &id, Some(&t.id), "task.created",
                serde_json::json!({"title": t.title, "depends_on": t.depends_on, "source": "planner"}));
        }
    }
    {
        let c = st.db.lock().unwrap();
        let milestones_json: Vec<Value> = created_milestones
            .iter()
            .map(|m| serde_json::json!({"id": m.id, "name": m.name, "goal": m.goal,
                "effect": m.effect, "task_ids": m.task_ids, "status": m.status}))
            .collect();
        events::push_event(&st, &c, &id, None, "planner.proposed",
            serde_json::json!({"engine": "opencode", "session_id": session,
                "opencode_session_id": st.sessions.lock().unwrap().get(&session).clone(),
                "count": created.len(),
                "milestones": milestones_json,
                "tasks": created.iter().map(|t| serde_json::json!({"id": t.id, "title": t.title,
                    "depends_on": t.depends_on, "executor_agent_id": t.executor_agent_id,
                    "acceptor_agent_id": t.acceptor_agent_id, "milestone_id": t.milestone_id})).collect::<Vec<_>>()}));
    }
    st.wake();
    ok(serde_json::json!({"tasks_added": created, "milestones_added": created_milestones}))
}

// ---------- human intervention: planner-aided replanning (DESIGN §9 介入) ----------

async fn intervene_project(
    State(st): State<Arc<App>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Some(note) = body["note"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "note is required");
    };
    let (project, agents, tasks) = {
        let c = st.db.lock().unwrap();
        let Some(p) = db::get_project(&c, &id) else { return err(StatusCode::NOT_FOUND, "project not found"); };
        if p.status == "archived" {
            return err(StatusCode::CONFLICT, "project is archived (read-only)");
        }
        (p, db::list_agents(&c), db::list_tasks(&c, &id))
    };
    let Some(planner) = agents
        .iter()
        .find(|a| a.adapter == "opencode" && a.roles.iter().any(|r| r == "planner"))
    else {
        return err(StatusCode::BAD_REQUEST, "no opencode agent with the 'planner' role is registered; add one to use intervention planning");
    };
    let session = opencode::plan_session(&id);
    {
        let c = st.db.lock().unwrap();
        events::push_event(&st, &c, &id, None, "agent.session_started",
            json!({"agent_id": planner.id, "phase": "plan", "session_id": session, "kind": "intervention",
                   "purpose": "人工介入：与规划者探讨调整方案", "trigger": "human"}));
    }
    tracing::info!(target: "planner", project = %id, note = %truncate(note, 400), "intervention requested");

    let prompt = crate::knowledge::assemble(&st, &id, None, "planner", "intervene", |k| {
        opencode::intervene_prompt(&project, &tasks, note, k)
    });
    let planner_model = opencode::model_of(planner);
    let call = opencode::run_cli(
        &st, None, None, &session,
        planner_model.as_deref(),
        opencode::PLAN_TIMEOUT_MS,
        &prompt,
    );
    let outcome = match call.await {
        Ok(o) => o,
        Err(e) => {
            plan_session_end(&st, &id, &planner.id, &session, "error", Some(e.clone()));
            tracing::warn!(target: "planner", project = %id, "{e}");
            return err(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    };
    plan_session_end(&st, &id, &planner.id, &session, "success", None);

    let Some(v) = extract_json_value(&outcome.text) else {
        tracing::warn!(target: "planner", project = %id, raw = %truncate(&outcome.text, 2000), "intervention JSON parse failed");
        let tail: String = outcome.text.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect();
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("planner output contains no parseable JSON object; output tail: {tail}"));
    };
    let analysis = v["analysis"].as_str().unwrap_or("").to_string();
    let reply = v["reply"].as_str().unwrap_or("").to_string();
    let changes = v["changes"].as_array().cloned().unwrap_or_default();
    if changes.len() > 20 {
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("planner proposed {} changes; more than the 20-change limit", changes.len()));
    }
    tracing::info!(target: "planner", project = %id, changes = changes.len(), "intervention proposal");
    {
        let c = st.db.lock().unwrap();
        events::push_event(&st, &c, &id, None, "replan.proposed",
            json!({"note": note, "analysis": analysis, "reply": reply, "changes": changes,
                   "session_id": session,
                   "opencode_session_id": st.sessions.lock().unwrap().get(&session).clone()}));
    }
    ok(json!({"analysis": analysis, "reply": reply, "changes": changes}))
}

/// 重规划新增/更新任务缺省验收标准时的兜底文案：保证「新任务默认带验收」既有要求、
/// 也有可检验的标准（否则任务会以空标准进入调度/验收）。
pub(crate) fn default_acceptance_criteria(title: &str, description: &str) -> String {
    if description.trim().is_empty() {
        format!("任务「{title}」产出可检验的结果，达成其标题所声明的目标")
    } else {
        format!("任务「{title}」产出可检验的结果，满足其描述所声明的目标")
    }
}

/// 逐项应用重规划变更（人工介入通道 & 里程碑窗口共用）。
/// 单项失败跳过不影响其余项；依赖用任务标题解析（既有任务 or 同批新建任务）；
/// 新增/修改依赖做 DAG 环校验，成环则拒绝该项并汇报原因。
/// `default_milestone`：窗口内新增任务默认挂到该里程碑（可由 change 显式覆盖）。
pub(crate) fn apply_replan_changes(
    st: &Arc<App>,
    id: &str,
    changes: &[Value],
    default_milestone: Option<&str>,
    source: &str,
) -> (Vec<Value>, Vec<Value>) {
    let mut agents = {
        let c = st.db.lock().unwrap();
        db::list_agents(&c)
    };
    // M4.5：重规划新增任务同样不绑定 agent（动态选人；人类可在任务编辑中固定）。
    let _ = &mut agents;

    let mut title_map: std::collections::HashMap<String, String> = {
        let c = st.db.lock().unwrap();
        db::list_tasks(&c, id)
            .into_iter()
            .map(|t| (t.title.trim().to_string(), t.id.clone()))
            .collect()
    };
    let mut added: std::collections::HashMap<String, String> = Default::default();
    let mut applied: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut rot = 0usize;

    for (i, ch) in changes.iter().enumerate() {
        let op = ch["op"].as_str().unwrap_or("");
        match op {
            "add" => {
                // M4.5：动态选人，无需在此检查候选池（执行时按角色+池优先级解析）
                let task = &ch["task"];
                let Some(title) = task["title"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                    skipped.push(json!({"index": i, "op": op, "reason": "task.title missing"}));
                    continue;
                };
                if title_map.contains_key(title) {
                    skipped.push(json!({"index": i, "op": op, "title": title, "reason": "a task with this title already exists"}));
                    continue;
                }
                let description = task["description"].as_str().unwrap_or("").to_string();
                // 窗口内新增任务默认带验收（DESIGN §8：动态追加默认带验收，不豁免）
                let acceptance_required = task["acceptance_required"].as_bool().unwrap_or(true);
                // 「默认带验收」不仅要求 acceptance_required，还必须落一条可检验的验收标准：
                // planner / 上报者若只声明要求、未给标准，这里补一条兜底标准，避免带验收的任务
                // 以空标准进入调度。
                let criteria = task["acceptance_criteria"]
                    .as_str()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        acceptance_required.then(|| default_acceptance_criteria(title, &description))
                    });
                // 归属里程碑：change 显式指定优先，否则用当前窗口的里程碑
                let mut milestone_id: Option<String> = default_milestone.map(String::from);
                if let Some(mv) = task.get("milestone_id") {
                    if mv.is_null() {
                        milestone_id = None;
                    } else if let Some(mid) = mv.as_str() {
                        let c = st.db.lock().unwrap();
                        match db::get_milestone(&c, mid) {
                            Some(m) if m.project_id == id => milestone_id = Some(mid.to_string()),
                            _ => {
                                skipped.push(json!({"index": i, "op": op, "title": title, "reason": format!("milestone {mid} not found in this project")}));
                                continue;
                            }
                        }
                    }
                }
                // resolve deps by title (existing or same-batch)
                let mut dep_ids: Vec<String> = Vec::new();
                let mut bad_dep: Option<String> = None;
                if let Some(arr) = task["depends_on"].as_array() {
                    for d in arr {
                        match d.as_str().map(str::trim) {
                            Some(dt) => {
                                if let Some(rid) = title_map.get(dt).or_else(|| added.get(dt)) {
                                    if !dep_ids.contains(rid) { dep_ids.push(rid.clone()); }
                                } else {
                                    bad_dep = Some(dt.to_string());
                                    break;
                                }
                            }
                            None => { bad_dep = Some(d.to_string()); break; }
                        }
                    }
                }
                if let Some(bad) = bad_dep {
                    skipped.push(json!({"index": i, "op": op, "title": title, "reason": format!("unknown dependency title: {bad}")}));
                    continue;
                }
                let (executor, acceptor) = (None::<String>, None::<String>);
                let _ = rot;
                let t = Task {
                    id: db::new_id("tsk_"),
                    project_id: id.to_string(),
                    milestone_id: milestone_id.clone(),
                    title: title.to_string(),
                    description,
                    depends_on: dep_ids.clone(),
                    assignee_role: "executor".into(),
                    executor_agent_id: executor,
                    acceptor_agent_id: acceptor,
                    acceptance_required,
                    acceptance_criteria: criteria,
                    execution_plan: None,
                    plan_rejection_count: 0,
                    scope_repos: Vec::new(),
                    agent_pinned: false,
                    tried_agents: serde_json::json!({}),
                    status: "planned".into(),
                    version: 0,
                    attempt: 0,
                    rejection_count: 0,
                    failure_count: 0,
                    artifacts: vec![],
                    created_at: db::now(),
                };
                {
                    let c = st.db.lock().unwrap();
                    if db::deps_introduce_cycle(&c, &id, Some(&t.id), &t.depends_on) {
                        skipped.push(json!({"index": i, "op": op, "title": title, "reason": "would introduce a dependency cycle"}));
                        continue;
                    }
                    match insert_task_row(&c, &t) {
                        Ok(()) => {}
                        Err(e) => {
                            skipped.push(json!({"index": i, "op": op, "title": title, "reason": format!("insert failed: {e}")}));
                            continue;
                        }
                    }
                    events::push_event(&st, &c, &id, Some(&t.id), "task.created",
                        json!({"title": t.title, "source": source, "depends_on": t.depends_on}));
                    // 重规划可能在项目 completed 后补入新任务：回到 running 让调度器接管
                    reopen_if_completed(&st, &c, &id);
                }
                events::send_task(&st, &t);
                added.insert(title.to_string(), t.id.clone());
                title_map.entry(title.to_string()).or_insert_with(|| t.id.clone());
                applied.push(json!({"index": i, "op": "add", "task_id": t.id, "title": t.title}));
                rot += 1;
            }
            "update" => {
                let Some(task_id) = ch["task_id"].as_str() else {
                    skipped.push(json!({"index": i, "op": op, "reason": "task_id missing"}));
                    continue;
                };
                let result: Result<Task, String> = (|| {
                    let c = st.db.lock().unwrap();
                    let Some(mut t) = db::get_task(&c, task_id) else { return Err(format!("task {task_id} not found")); };
                    if t.project_id != id { return Err("task belongs to another project".into()); }
                    if t.status == "done" || t.status == "cancelled" { return Err(format!("task {} is terminal ({})", t.title, t.status)); }
                    let f = &ch["fields"];
                    let mut new_deps: Option<Vec<String>> = None;
                    if let Some(arr) = f["depends_on"].as_array() {
                        let mut ids = Vec::new();
                        for d in arr {
                            let dt = d.as_str().map(str::trim).unwrap_or("");
                            let rid = title_map.get(dt).or_else(|| added.get(dt))
                                .ok_or_else(|| format!("unknown dependency title: {dt}"))?;
                            if !ids.contains(rid) { ids.push(rid.clone()); }
                        }
                        new_deps = Some(ids);
                    }
                    if let Some(ref ids) = new_deps {
                        if db::deps_introduce_cycle(&c, &id, Some(&t.id), ids) {
                            return Err("would introduce a dependency cycle".into());
                        }
                        t.depends_on = ids.clone();
                    }
                    if let Some(s) = f["title"].as_str() { t.title = s.trim().to_string(); }
                    if let Some(s) = f["description"].as_str() { t.description = s.to_string(); }
                    if let Some(b) = f["acceptance_required"].as_bool() { t.acceptance_required = b; }
                    if let Some(s) = f["acceptance_criteria"].as_str() {
                        let s = s.trim();
                        t.acceptance_criteria = if s.is_empty() { None } else { Some(s.to_string()) };
                    }
                    // 与新增路径一致：带验收的任务必须落一条可检验的标准（空标准自动补兜底）
                    if t.acceptance_required
                        && t.acceptance_criteria.as_deref().map(str::trim).unwrap_or("").is_empty()
                    {
                        t.acceptance_criteria = Some(default_acceptance_criteria(&t.title, &t.description));
                    }
                    c.execute(
                        "UPDATE tasks SET title=?2, description=?3, depends_on=?4, acceptance_required=?5, acceptance_criteria=?6 WHERE id=?1",
                        rusqlite::params![t.id, t.title, t.description, db::jstring(&Value::Array(t.depends_on.iter().map(|d| Value::String(d.clone())).collect())),
                                t.acceptance_required as i64, t.acceptance_criteria],
                    ).map_err(|e| format!("update failed: {e}"))?;
                    events::push_event(&st, &c, &t.project_id, Some(&t.id), "task.updated",
                        json!({"source": source}));
                    Ok(t)
                })();
                match result {
                    Ok(t) => {
                        events::send_task(&st, &t);
                        title_map.insert(t.title.trim().to_string(), t.id.clone());
                        applied.push(json!({"index": i, "op": "update", "task_id": t.id, "title": t.title}));
                    }
                    Err(reason) => skipped.push(json!({"index": i, "op": op, "reason": reason})),
                }
            }
            "cancel" => {
                let Some(task_id) = ch["task_id"].as_str() else {
                    skipped.push(json!({"index": i, "op": op, "reason": "task_id missing"}));
                    continue;
                };
                let c = st.db.lock().unwrap();
                let Some(t) = db::get_task(&c, task_id) else {
                    skipped.push(json!({"index": i, "op": op, "reason": format!("task {task_id} not found")}));
                    continue;
                };
                if t.project_id != id {
                    skipped.push(json!({"index": i, "op": op, "reason": "task belongs to another project"}));
                    continue;
                }
                if t.status == "done" || t.status == "cancelled" {
                    skipped.push(json!({"index": i, "op": op, "task_id": t.id, "reason": format!("already terminal ({})", t.status)}));
                    continue;
                }
                events::status_changed(&st, &c, &t, "cancelled", source);
                events::send_task(&st, &t);
                drop(c);
                applied.push(json!({"index": i, "op": "cancel", "task_id": t.id, "title": t.title}));
            }
            other => {
                skipped.push(json!({"index": i, "op": other, "reason": "unknown op"}));
            }
        }
    }
    let _ = &mut agents;
    st.wake();
    tracing::info!(target: "planner", project = %id, source = %source, applied = applied.len(), skipped = skipped.len(), "replan changes applied");
    (applied, skipped)
}

/// POST /api/projects/{id}/intervene/apply：用户确认方案后逐项应用
/// （单项失败自动跳过并汇报原因，不影响其他项）。
async fn intervene_apply(
    State(st): State<Arc<App>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    {
        let c = st.db.lock().unwrap();
        let Some(p) = db::get_project(&c, &id) else { return err(StatusCode::NOT_FOUND, "project not found"); };
        if p.status == "archived" { return err(StatusCode::CONFLICT, "project is archived (read-only)"); }
    }
    let Some(changes) = body["changes"].as_array().cloned() else {
        return err(StatusCode::BAD_REQUEST, "changes must be an array");
    };
    if changes.len() > 20 {
        return err(StatusCode::BAD_REQUEST, format!("{} changes proposed; more than the 20-change limit", changes.len()));
    }
    let (applied, skipped) = apply_replan_changes(&st, &id, &changes, None, "intervention");
    ok(json!({"applied": applied, "skipped": skipped}))
}

fn insert_task_row(c: &rusqlite::Connection, t: &Task) -> Result<(), String> {
    c.execute(
        "INSERT INTO tasks (id,project_id,milestone_id,title,description,depends_on,assignee_role,executor_agent_id,acceptor_agent_id,acceptance_required,acceptance_criteria,status,attempt,rejection_count,failure_count,artifacts,created_at,agent_pinned)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        rusqlite::params![
            t.id, t.project_id, t.milestone_id, t.title, t.description,
            db::jstring(&Value::Array(t.depends_on.iter().map(|d| Value::String(d.clone())).collect())),
            t.assignee_role, t.executor_agent_id, t.acceptor_agent_id,
            t.acceptance_required as i64, t.acceptance_criteria,
            t.status, t.attempt, t.rejection_count, t.failure_count,
            db::jstring(&Value::Array(Vec::new())), t.created_at, t.agent_pinned as i64
        ],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

async fn pause_project(
    State(st): State<Arc<App>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let force = body["force"].as_bool().unwrap_or(false);
    let (has_live, other_running) = {
        let c = st.db.lock().unwrap();
        let Some(project) = db::get_project(&c, &id) else {
            return err(StatusCode::NOT_FOUND, "project not found");
        };
        match project.status.as_str() {
            "running" | "pausing" => {}
            _ => return err(StatusCode::CONFLICT, "only running projects can be paused"),
        }
        let other = project
            .engineering_id
            .as_deref()
            .map(|eid| db::list_projects_of(&c, eid).into_iter().any(|x| {
                x.id != id && (x.status == "running" || x.status == "pausing")
            }))
            .unwrap_or(false);
        let live = db::list_tasks(&c, &id)
            .iter()
            .any(|t| st.has_live_handle(&t.id));
        (live, other)
    };
    // 安全网：强制暂停会终止本项目在途会话；若同工程仍有运行中项目（legacy 数据），
    // 不猜测影响范围，交人确认后重试。
    if force && has_live && other_running {
        let c = st.db.lock().unwrap();
        let item = events::new_inbox_item(&st, &c, &id, "project_action_blocked",
            json!({
                "reason": "pause_affected_other_projects",
                "project_id": id,
                "detail": "强制暂停需要终止本项目在途会话，但同工程仍有运行中项目；请先确认影响范围后重试",
            }));
        let inbox_id = item.id;
        drop(c);
        return (StatusCode::CONFLICT, Json(json!({
            "error": "强制暂停受阻：同工程仍有运行中项目，已在收件箱登记，请确认后重试",
            "inbox_id": inbox_id,
        }))).into_response();
    }
    if force && has_live {
        // 强制暂停：只终止**本项目**的在途会话并释放占用，立即进入已暂停。
        let tids: Vec<String> = {
            let c = st.db.lock().unwrap();
            db::list_tasks(&c, &id).into_iter().map(|t| t.id).collect()
        };
        let aborted: Vec<String> = {
            let c = st.db.lock().unwrap();
            db::list_tasks(&c, &id)
                .into_iter()
                .filter(|t| st.has_live_handle(&t.id))
                .map(|t| t.id)
                .collect()
        };
        st.abort_project_handles(&tids);
        {
            let c = st.db.lock().unwrap();
            events::push_event(
                &st, &c, &id, None, "project.pause_applied",
                json!({"mode": "force", "scope": "project", "aborted_attempts": aborted.len(),
                       "task_ids": aborted,
                       "note": "强制暂停只取消本项目的在途会话，其他项目不受影响"}),
            );
            let c = st.db.lock().unwrap();
            for tid in &tids {
                if let Some(t) = db::get_task(&c, tid) {
                    if let Some(a) = t.executor_agent_id.clone() {
                        db::occupancy_remove(&c, &a, &t.id);
                        if let Some(ag) = db::get_agent(&c, &a) { events::send_agent(&st, &ag); }
                    }
                    if let Some(a) = t.acceptor_agent_id.clone() {
                        db::occupancy_remove(&c, &a, &t.id);
                        if let Some(ag) = db::get_agent(&c, &a) { events::send_agent(&st, &ag); }
                    }
                }
            }
        }
        tracing::warn!(project = %id, "force pause: in-flight sessions aborted");
        return set_project_status(&st, &id, "paused", "force_paused");
    }
    if has_live {
        set_project_status(&st, &id, "pausing", "pause_requested")
    } else {
        set_project_status(&st, &id, "paused", "paused")
    }
}

async fn resume_project(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    {
        let c = st.db.lock().unwrap();
        let Some(project) = db::get_project(&c, &id) else {
            return err(StatusCode::NOT_FOUND, "project not found");
        };
        match project.status.as_str() {
            // paused=已暂停；pausing=暂停中（请求撤回，等于取消暂停）
            "paused" | "pausing" => {}
            _ => return err(StatusCode::CONFLICT, "project is not paused"),
        }
        if project.status == "paused" && db::project_requires_serial(&c, &project) {
            if let Some(eid) = project.engineering_id.as_deref() {
                let busy = db::list_projects_of(&c, eid)
                    .into_iter()
                    .any(|x| x.id != id && (x.status == "running" || x.status == "pausing"));
                if busy {
                    return err(StatusCode::CONFLICT,
                        "共享目录工程同一时刻只能运行一个项目：请先暂停或归档其他项目");
                }
            }
        }
    }
    set_project_status(&st, &id, "running", "resumed")
}

async fn cancel_project(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    {
        let c = st.db.lock().unwrap();
        match db::get_project(&c, &id).map(|p| p.status) {
            Some(s) if s == "archived" => return err(StatusCode::CONFLICT, "project already archived"),
            Some(_) => {}
            None => return err(StatusCode::NOT_FOUND, "project not found"),
        }
    }
    let task_ids: Vec<String> = {
        let c = st.db.lock().unwrap();
        db::list_tasks(&c, &id).iter().map(|t| t.id.clone()).collect()
    };
    st.abort_project_handles(&task_ids);
    {
        let c = st.db.lock().unwrap();
        for t in db::list_tasks(&c, &id) {
            if ["planned", "ready", "executing", "awaiting_acceptance", "escalated", "suspended"]
                .contains(&t.status.as_str())
            {
                // free agent occupancy for this task
                let agent_ids: Vec<String> = vec![
                    t.executor_agent_id.clone().unwrap_or_default(),
                    t.acceptor_agent_id.clone().unwrap_or_default(),
                ];
                events::status_changed(&st, &c, &t, "cancelled", "project_cancelled");
                for a in agent_ids {
                    if !a.is_empty() {
                        db::occupancy_remove(&c, &a, &t.id);
                        if let Some(ag) = db::get_agent(&c, &a) {
                            events::send_agent(&st, &ag);
                        }
                    }
                }
            }
        }
        let _ = c.execute("UPDATE projects SET status='archived' WHERE id=?1", rusqlite::params![id]);
        let p = db::get_project(&c, &id).unwrap();
        events::push_event(&st, &c, &id, None, "project.cancelled", json!({"tasks_cancelled": task_ids.len()}));
        events::send_project(&st, &p);
    }
    st.wake();
    ok(db::get_project(&st.db.lock().unwrap(), &id).unwrap())
}

// ---------- milestones (M3) ----------

fn validate_milestone_tasks(c: &rusqlite::Connection, project_id: &str, ids: &[String]) -> Result<(), String> {
    for tid in ids {
        match db::get_task(c, tid) {
            Some(t) if t.project_id == project_id => {}
            Some(_) => return Err(format!("task {tid} belongs to another project")),
            None => return Err(format!("task {tid} not found")),
        }
    }
    Ok(())
}

fn parse_task_ids(body: &Value) -> Vec<String> {
    body["task_ids"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

async fn create_milestone(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let Some(name) = body["name"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from) else {
        return err(StatusCode::BAD_REQUEST, "name is required");
    };
    let task_ids = parse_task_ids(&body);
    let c = st.db.lock().unwrap();
    let Some(p) = db::get_project(&c, &id) else { return err(StatusCode::NOT_FOUND, "project not found"); };
    if p.status == "archived" { return err(StatusCode::CONFLICT, "project is archived (read-only)"); }
    if let Err(e) = validate_milestone_tasks(&c, &id, &task_ids) { return err(StatusCode::BAD_REQUEST, e); }
    let m = Milestone {
        id: db::new_id("mst_"),
        project_id: id.clone(),
        name,
        goal: body["goal"].as_str().map(String::from),
        effect: body["effect"].as_str().map(String::from),
        task_ids: vec![],
        status: "pending".into(),
    };
    db::insert_milestone(&c, &m);
    for tid in &task_ids {
        db::set_task_milestone(&c, tid, Some(&m.id));
    }
    db::sync_milestone_task_ids(&c, &id);
    let m = db::get_milestone(&c, &m.id).unwrap();
    events::push_event(&st, &c, &id, None, "milestone.created",
        json!({"milestone_id": m.id, "name": m.name, "goal": m.goal, "effect": m.effect, "task_ids": m.task_ids}));
    events::send_milestone(&st, &m);
    drop(c);
    st.wake();
    ok(m)
}

async fn get_milestone_detail(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(m) = db::get_milestone(&c, &id) else { return err(StatusCode::NOT_FOUND, "milestone not found"); };
    db::sync_milestone_task_ids(&c, &m.project_id);
    let m = db::get_milestone(&c, &id).unwrap();
    let tasks = db::tasks_of_milestone(&c, &m.project_id, &id);
    ok(json!({"milestone": m, "tasks": tasks}))
}

async fn patch_milestone(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(m) = db::get_milestone(&c, &id) else { return err(StatusCode::NOT_FOUND, "milestone not found"); };
    let Some(p) = db::get_project(&c, &m.project_id) else { return err(StatusCode::NOT_FOUND, "project not found"); };
    if p.status == "archived" { return err(StatusCode::CONFLICT, "project is archived (read-only)"); }
    if let Some(arr) = body["task_ids"].as_array() {
        if m.status != "pending" {
            return err(StatusCode::CONFLICT, "milestone task membership can only change while the milestone is pending");
        }
        let ids: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
        if let Err(e) = validate_milestone_tasks(&c, &m.project_id, &ids) { return err(StatusCode::BAD_REQUEST, e); }
        for old in &m.task_ids {
            if !ids.contains(old) { db::set_task_milestone(&c, old, None); }
        }
        for tid in &ids { db::set_task_milestone(&c, tid, Some(&m.id)); }
        db::sync_milestone_task_ids(&c, &m.project_id);
    }
    let mut m = db::get_milestone(&c, &id).unwrap();
    if let Some(v) = body["name"].as_str().map(str::trim).filter(|s| !s.is_empty()) { m.name = v.to_string(); }
    if !body["goal"].is_null() { m.goal = body["goal"].as_str().map(String::from); }
    if !body["effect"].is_null() { m.effect = body["effect"].as_str().map(String::from); }
    db::update_milestone(&c, &m);
    events::push_event(&st, &c, &m.project_id, None, "milestone.updated",
        json!({"milestone_id": m.id, "name": m.name, "goal": m.goal, "effect": m.effect, "task_ids": m.task_ids}));
    events::send_milestone(&st, &m);
    drop(c);
    st.wake();
    ok(m)
}

/// M3 gate 退回后重开「里程碑窗口」：一条 `replan` 收件箱项
/// （payload.kind="milestone_window"），供人在窗口内追加任务或调整后续规划。
/// 幂等：已存在 open 窗口项时直接复用（重复退回 / 重启恢复安全）。
fn open_milestone_window(st: &Arc<App>, c: &rusqlite::Connection, m: &Milestone, note: &str) -> InboxItem {
    if let Some(item) = db::open_window_item(c, &m.id) {
        return item;
    }
    let task_ids: Vec<String> = db::tasks_of_milestone(c, &m.project_id, &m.id)
        .iter()
        .map(|t| t.id.clone())
        .collect();
    events::push_event(st, c, &m.project_id, None, "milestone.window_opened",
        json!({"milestone_id": m.id, "name": m.name, "note": note, "task_ids": task_ids,
               "detail": format!("里程碑「{}」被退回，已重开重规划窗口：可追加任务或调整后续规划后重新确认", m.name)}));
    events::new_inbox_item(st, c, &m.project_id, "replan",
        json!({
            "kind": "milestone_window",
            "milestone_id": m.id,
            "name": m.name,
            "goal": m.goal,
            "effect": m.effect,
            "task_ids": task_ids,
            "note": note,
            "detail": format!(
                "里程碑「{}」被退回：在窗口内追加任务或调整后续规划；新增任务默认带验收，依赖成环的调整会被拒绝并汇报原因，其余项继续应用",
                m.name
            ),
        }))
}

/// Shared gate adjudication for the direct milestone endpoints (the inbox path
/// is equivalent and additionally closes the gate item via `resolve_inbox`).
fn gate_verdict(st: &Arc<App>, id: &str, pass: bool, note: &str) -> Response {
    let c = st.db.lock().unwrap();
    let Some(m) = db::get_milestone(&c, id) else { return err(StatusCode::NOT_FOUND, "milestone not found"); };
    let Some(p) = db::get_project(&c, &m.project_id) else { return err(StatusCode::NOT_FOUND, "project not found"); };
    if p.status == "archived" { return err(StatusCode::CONFLICT, "project is archived (read-only)"); }
    if m.status != "awaiting_confirm" && m.status != "failed" {
        return err(StatusCode::CONFLICT, format!("milestone is not awaiting confirmation (status={})", m.status));
    }
    let (to, reason, ev, verdict) = if pass {
        ("passed", "gate_confirmed", "milestone.passed", "passed")
    } else {
        ("failed", "gate_rejected", "milestone.failed", "failed")
    };
    let m = events::milestone_status_changed(st, &c, &m, to, reason);
    events::push_event(st, &c, &m.project_id, None, ev,
        json!({"milestone_id": m.id, "name": m.name, "note": note}));
    // 退回 → 里程碑 failed 并重开重规划窗口，允许人在窗口内追加任务/调整后续规划。
    if !pass {
        open_milestone_window(st, &c, &m, note);
    }
    // close the matching open gate inbox item, if any
    if let Some(item) = db::open_gate_item(&c, &m.id) {
        let res = json!({"resolution": verdict, "note": note, "by": "milestone_api"});
        let _ = c.execute(
            "UPDATE inbox SET status='resolved', resolution=?2 WHERE id=?1",
            rusqlite::params![item.id, res.to_string()],
        );
        if let Some(updated) = db::get_inbox(&c, &item.id) { events::send_inbox(st, &updated); }
    }
    drop(c);
    st.wake();
    ok(m)
}

async fn confirm_milestone(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    gate_verdict(&st, &id, true, body["note"].as_str().unwrap_or(""))
}

async fn fail_milestone(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    gate_verdict(&st, &id, false, body["note"].as_str().unwrap_or(""))
}

/// POST /api/milestones/:id/window/apply — 里程碑窗口内应用重规划变更。
/// 复用 `apply_replan_changes`：新增任务默认带验收、依赖按标题解析（含同批新增）、
/// 新增/修改依赖做 DAG 环校验（成环项拒绝并汇报原因，其余项继续应用）。
/// 应用后关闭该里程碑的窗口收件箱项；窗口内的新增任务进入调度。
async fn milestone_window_apply(
    State(st): State<Arc<App>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Some(changes) = body["changes"].as_array().cloned() else {
        return err(StatusCode::BAD_REQUEST, "changes must be an array");
    };
    if changes.len() > 20 {
        return err(StatusCode::BAD_REQUEST, format!("{} changes proposed; more than the 20-change limit", changes.len()));
    }
    let note = body["note"].as_str().unwrap_or("");
    let project_id = {
        let c = st.db.lock().unwrap();
        let Some(m) = db::get_milestone(&c, &id) else { return err(StatusCode::NOT_FOUND, "milestone not found"); };
        let Some(p) = db::get_project(&c, &m.project_id) else { return err(StatusCode::NOT_FOUND, "project not found"); };
        if p.status == "archived" { return err(StatusCode::CONFLICT, "project is archived (read-only)"); }
        if db::open_window_item(&c, &id).is_none() {
            return err(StatusCode::CONFLICT, "no open replanning window for this milestone (reject the gate first)");
        }
        m.project_id
    };

    // 逐项应用：新任务默认挂到该里程碑（可由 change 显式覆盖）
    let (applied, skipped) = apply_replan_changes(&st, &project_id, &changes, Some(&id), "milestone_window");

    {
        let c = st.db.lock().unwrap();
        events::push_event(&st, &c, &project_id, None, "milestone.window_applied",
            json!({"milestone_id": id, "note": note, "changes": changes.len(),
                   "applied": applied, "skipped": skipped}));
        if let Some(item) = db::open_window_item(&c, &id) {
            let res = json!({"resolution": "applied", "note": note, "applied": applied, "skipped": skipped});
            let _ = c.execute(
                "UPDATE inbox SET status='resolved', resolution=?2 WHERE id=?1",
                rusqlite::params![item.id, res.to_string()],
            );
            if let Some(fresh) = db::get_inbox(&c, &item.id) { events::send_inbox(&st, &fresh); }
            events::push_event(&st, &c, &project_id, None, "milestone.window_resolved",
                json!({"milestone_id": id, "inbox_id": item.id,
                       "applied": applied.len(), "skipped": skipped.len()}));
        }
        db::sync_milestone_task_ids(&c, &project_id);
        if let Some(m) = db::get_milestone(&c, &id) { events::send_milestone(&st, &m); }
    }
    st.wake();
    ok(json!({"milestone_id": id, "applied": applied, "skipped": skipped}))
}

// ---------- tasks ----------

fn validate_dep_ids(c: &rusqlite::Connection, project_id: &str, deps: &[String]) -> bool {
    deps.iter().all(|d| {
        db::get_task(c, d)
            .map(|t| t.project_id == project_id && t.id != project_id)
            .unwrap_or(false)
    })
}

fn validate_agent_roles(c: &rusqlite::Connection, executor: Option<&str>, acceptor: Option<&str>) -> Result<(), String> {
    if let Some(e) = executor {
        let Some(a) = db::get_agent(c, e) else { return Err("executor agent not found".into()); };
        if !a.roles.iter().any(|r| r == "executor") { return Err("executor agent lacks 'executor' role".into()); }
    }
    if let Some(a) = acceptor {
        let Some(ag) = db::get_agent(c, a) else { return Err("acceptor agent not found".into()); };
        if !ag.roles.iter().any(|r| r == "acceptor") { return Err("acceptor agent lacks 'acceptor' role".into()); }
    }
    if let (Some(e), Some(a)) = (executor, acceptor) {
        // 设计允许同一实例兼任执行与验收；独立性由界面提醒，不再拒绝。
        let _ = (e, a);
    }
    Ok(())
}

async fn create_task(
    State(st): State<Arc<App>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let title = body["title"].as_str().filter(|s| !s.trim().is_empty());
    let Some(title) = title else { return err(StatusCode::BAD_REQUEST, "title is required"); };
    let executor = body["executor_agent_id"].as_str().map(String::from);
    let acceptor = body["acceptor_agent_id"].as_str().map(String::from);
    let accept_required = body["acceptance_required"].as_bool().unwrap_or(true);
    // M4.5：不强制在此绑定 agent——执行时按「角色 + 项目池优先级」动态解析；
    // 人类显式指定时才固定（agent_pinned）。
    {
        let c = st.db.lock().unwrap();
        let Some(p) = db::get_project(&c, &id) else { return err(StatusCode::NOT_FOUND, "project not found"); };
        if p.status == "archived" { return err(StatusCode::CONFLICT, "project is archived (read-only)"); }
        reopen_if_completed(&st, &c, &id);
        let deps: Vec<String> = body["depends_on"].as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if !validate_dep_ids(&c, &id, &deps) {
            return err(StatusCode::BAD_REQUEST, "depends_on contains unknown tasks of other projects");
        }
        if db::deps_introduce_cycle(&c, &id, None, &deps) {
            return err(StatusCode::BAD_REQUEST, "depends_on introduces a dependency cycle");
        }
        if let Err(e) = validate_agent_roles(&c, executor.as_deref(), acceptor.as_deref()) {
            return err(StatusCode::BAD_REQUEST, e);
        }
        // M2.5: 项目 agent 池约束（显式指定的执行/验收者必须在池内）
        for (label, aid) in [("executor", executor.as_deref()), ("acceptor", acceptor.as_deref())] {
            if let Some(aid) = aid {
                if let Some(pool) = &p.agent_pool {
                    if !pool.iter().any(|x| x == aid) {
                        return err(StatusCode::BAD_REQUEST, format!("{label} agent is not in the project agent pool"));
                    }
                }
            }
        }
        let milestone_id = body["milestone_id"].as_str().map(String::from);
        if let Some(mid) = &milestone_id {
            match db::get_milestone(&c, mid) {
                Some(m) if m.project_id == id => {}
                Some(_) => return err(StatusCode::BAD_REQUEST, "milestone belongs to another project"),
                None => return err(StatusCode::BAD_REQUEST, "milestone not found"),
            }
        }
        let pinned = executor.is_some();
        let t = Task {
            id: db::new_id("tsk_"),
            project_id: id.clone(),
            milestone_id,
            title: title.trim().into(),
            description: body["description"].as_str().unwrap_or("").into(),
            depends_on: deps,
            assignee_role: "executor".into(),
            executor_agent_id: executor,
            acceptor_agent_id: acceptor,
            acceptance_required: accept_required,
            acceptance_criteria: body["acceptance_criteria"].as_str().map(String::from),
            execution_plan: None,
            plan_rejection_count: 0,
            scope_repos: Vec::new(),
            agent_pinned: pinned,
            tried_agents: serde_json::json!({}),
            status: "planned".into(),
            version: 0,
            attempt: 0,
            rejection_count: 0,
            failure_count: 0,
            artifacts: vec![],
            created_at: db::now(),
        };
        c.execute(
            "INSERT INTO tasks (id,project_id,milestone_id,title,description,depends_on,assignee_role,executor_agent_id,acceptor_agent_id,acceptance_required,acceptance_criteria,status,attempt,rejection_count,failure_count,artifacts,created_at,agent_pinned)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            rusqlite::params![
                t.id, t.project_id, t.milestone_id, t.title, t.description,
                db::jstring(&serde_json::to_value(&t.depends_on).unwrap()),
                t.assignee_role, t.executor_agent_id, t.acceptor_agent_id,
                t.acceptance_required as i64, t.acceptance_criteria,
                t.status, t.attempt, t.rejection_count, t.failure_count,
                db::jstring(&serde_json::to_value(&t.artifacts).unwrap()), t.created_at,
                t.agent_pinned as i64
            ],
        ).unwrap();
        if let Some(arr) = body["acceptance_checks"].as_array() {
            for chk in arr {
                if let Err(e) = validate_check(chk) {
                    return err(StatusCode::BAD_REQUEST, e);
                }
            }
            db::set_task_checks(&c, &t.id, arr);
        }
        events::push_event(&st, &c, &id, Some(&t.id), "task.created",
            json!({"title": t.title, "depends_on": t.depends_on}));
        if let Some(mid) = &t.milestone_id {
            db::sync_milestone_task_ids(&c, &id);
            events::push_event(&st, &c, &id, Some(&t.id), "milestone.task_linked",
                json!({"milestone_id": mid, "task_id": t.id}));
        }
        drop(c);
        st.wake();
        ok(t)
    }
}

/// M5：agent 运行归档输出（单条全文，含原始 stdout 流）。
async fn get_agent_output(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let Ok(oid) = id.parse::<i64>() else { return err(StatusCode::BAD_REQUEST, "id must be an integer") };
    let c = st.db.lock().unwrap();
    match db::get_agent_output(&c, oid) {
        Some(v) => ok(v),
        None => err(StatusCode::NOT_FOUND, "agent output not found"),
    }
}

async fn get_task_detail(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(t) = db::get_task(&c, &id) else { return err(StatusCode::NOT_FOUND, "task not found"); };
    let rec = db::recovery_state(&c, &id);
    // M5：归档的 agent 原始输出（模型输出已由 DOMO 获得 → 自行存档，供审计回放与结论复核）
    let outputs: Vec<Value> = db::list_agent_outputs(&c, &id, 10)
        .into_iter()
        .map(|mut o| {
            if let Some(t) = o.get("text").and_then(|v| v.as_str()) {
                if t.len() > 8000 {
                    o["text"] = json!(format!("{}…（完整文本见 /api/agent-outputs/{}）", &t[..8000.min(t.len())], o["id"]));
                }
            }
            o
        })
        .collect();
    let body = json!({
        "task": t, "logs": db::task_logs(&c, &id), "agent_outputs": outputs,
        "events": db::list_events(&c, None, Some(&id)),
        "guidance": db::list_guidance(&c, &id),
        "acceptance_checks": db::get_task_checks(&c, &id),
        "acceptance_evidence": db::latest_acceptance_evidence(&c, &id),
        // M4：三档恢复状态 + 当前在途 worker 句柄数（同一 task 恒 ≤1）
        "recovery": {
            "pending": rec.pending, "tier": rec.tier, "manual": rec.manual,
        },
        "live_handles": st.live_handles(&id) as u64,
    });
    ok(body)
}

async fn patch_task(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    // 阶段一（持锁）：校验并构造更新后的任务
    let (mut t, old_agents, live_swap) = {
        let c = st.db.lock().unwrap();
        let Some(mut t) = db::get_task(&c, &id) else { return err(StatusCode::NOT_FOUND, "task not found"); };
        if db::get_project(&c, &t.project_id).map(|p| p.status) == Some("archived".into()) {
            return err(StatusCode::CONFLICT, "project is archived (read-only)");
        }
        if t.status == "done" || t.status == "cancelled" {
            return err(StatusCode::CONFLICT, "terminal tasks are read-only");
        }
        // 文本/依赖编辑仅限开工前；角色（executor/acceptor）替换允许任何非终态——
        // 挂起/升级任务靠它换上正确的 agent 后再通过收件箱「恢复重派」。
        let role_edit = body["executor_agent_id"].is_string() || body["acceptor_agent_id"].is_string();
        let text_edit = body["title"].is_string() || body["description"].is_string()
            || body["acceptance_criteria"].is_string() || !body["depends_on"].is_null()
            || body.get("milestone_id").is_some();
        if text_edit && t.status != "planned" && t.status != "ready" {
            return err(StatusCode::CONFLICT, "title/description/depends_on/acceptance_criteria are only editable before execution starts (use inbox adjudication for suspended/escalated tasks)");
        }
        if !body["depends_on"].is_null() {
            let new_deps: Vec<String> = body["depends_on"].as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if body["depends_on"].as_array().is_none() {
                return err(StatusCode::BAD_REQUEST, "depends_on must be an array");
            }
            if !validate_dep_ids(&c, &t.project_id, &new_deps) {
                return err(StatusCode::BAD_REQUEST, "depends_on contains unknown tasks");
            }
            if db::deps_introduce_cycle(&c, &t.project_id, Some(&t.id), &new_deps) {
                return err(StatusCode::BAD_REQUEST, "depends_on introduces a dependency cycle");
            }
            t.depends_on = new_deps;
        }
        if let Some(mv) = body.get("milestone_id") {
            if mv.is_null() {
                t.milestone_id = None;
            } else if let Some(mid) = mv.as_str() {
                match db::get_milestone(&c, mid) {
                    Some(m) if m.project_id == t.project_id => { t.milestone_id = Some(mid.to_string()); }
                    Some(_) => return err(StatusCode::BAD_REQUEST, "milestone belongs to another project"),
                    None => return err(StatusCode::BAD_REQUEST, "milestone not found"),
                }
            } else {
                return err(StatusCode::BAD_REQUEST, "milestone_id must be a string or null");
            }
        }
        let old_executor = t.executor_agent_id.clone().filter(|s| !s.is_empty());
        let old_acceptor = t.acceptor_agent_id.clone().filter(|s| !s.is_empty());
        // M4.5：人类显式指定 agent → 固定（agent_pinned）；显式传空串 → 解除固定（回到动态）
        let mut pinned_change: Option<bool> = None;
        if let Some(v) = body["executor_agent_id"].as_str() {
            t.executor_agent_id = if v.is_empty() { None } else { Some(v.to_string()) };
            pinned_change = Some(!v.is_empty());
        }
        if let Some(v) = body["acceptor_agent_id"].as_str() {
            t.acceptor_agent_id = if v.is_empty() { None } else { Some(v.to_string()) };
            pinned_change = Some(!v.is_empty());
        }
        if let Some(p) = pinned_change {
            t.agent_pinned = p;
        }
        if let Err(e) = validate_agent_roles(&c, t.executor_agent_id.as_deref(), t.acceptor_agent_id.as_deref()) {
            return err(StatusCode::BAD_REQUEST, e);
        }
        let roles_changed = t.executor_agent_id != old_executor || t.acceptor_agent_id != old_acceptor;
        // 在途角色替换：先终止在途会话并释放占用（随后由调度器或收件箱重派）
        let live_swap = role_edit && roles_changed && st.has_live_handle(&t.id);
        (t, [old_executor, old_acceptor], live_swap)
    }; // 锁在此释放
    // 阶段二（无锁）：终止在途会话
    if live_swap {
        st.abort_task_handles(&t.id);
    }
    // 阶段三（重新持锁）：落库 + 事件
    {
        let c = st.db.lock().unwrap();
        for old in old_agents.into_iter().flatten() {
            db::occupancy_remove(&c, &old, &t.id);
            if let Some(ag) = db::get_agent(&c, &old) { events::send_agent(&st, &ag); }
        }
        if let Some(v) = body["title"].as_str() { t.title = v.to_string(); }
        if let Some(v) = body["description"].as_str() { t.description = v.to_string(); }
        if let Some(v) = body["acceptance_criteria"].as_str() { t.acceptance_criteria = Some(v.to_string()); }
        let _ = c.execute(
            "UPDATE tasks SET title=?2, description=?3, depends_on=?4, executor_agent_id=?5, acceptor_agent_id=?6, acceptance_criteria=?7, milestone_id=?8, agent_pinned=?9 WHERE id=?1",
            rusqlite::params![
                t.id, t.title, t.description,
                db::jstring(&serde_json::to_value(&t.depends_on).unwrap()),
                t.executor_agent_id, t.acceptor_agent_id, t.acceptance_criteria, t.milestone_id,
                t.agent_pinned as i64
            ],
        );
        if let Some(arr) = body["acceptance_checks"].as_array() {
            for chk in arr {
                if let Err(e) = validate_check(chk) {
                    return err(StatusCode::BAD_REQUEST, e);
                }
            }
            db::set_task_checks(&c, &t.id, arr);
        }
        events::push_event(&st, &c, &t.project_id, Some(&t.id), "task.updated", json!({}));
        if body.get("milestone_id").is_some() {
            db::sync_milestone_task_ids(&c, &t.project_id);
            events::push_event(&st, &c, &t.project_id, Some(&t.id), "milestone.task_linked",
                json!({"milestone_id": t.milestone_id, "task_id": t.id}));
        }
        drop(c);
        st.wake();
    }
    ok(t)
}

async fn add_guidance(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let text = body["text"].as_str().filter(|s| !s.is_empty());
    let Some(text) = text else { return err(StatusCode::BAD_REQUEST, "text is required"); };
    let c = st.db.lock().unwrap();
    let Some(t) = db::get_task(&c, &id) else { return err(StatusCode::NOT_FOUND, "task not found"); };
    if db::get_project(&c, &t.project_id).map(|p| p.status) == Some("archived".into()) {
        return err(StatusCode::CONFLICT, "project is archived (read-only)");
    }
    let g = Guidance { id: db::new_id("gd_"), task_id: id.clone(), text: text.into(), created_at: db::now() };
    c.execute("INSERT INTO guidance (id,task_id,text,created_at) VALUES (?1,?2,?3,?4)",
        rusqlite::params![g.id, g.task_id, g.text, g.created_at]).unwrap();
    // M5：事件 payload 带 guidance_id，便于知识条目回链「产生它的人工指导事件」
    events::push_event(&st, &c, &t.project_id, Some(&t.id), "task.guidance_added",
        json!({"text": g.text, "guidance_id": g.id}));
    ok(g)
}

// ---------- M5 项目知识库（条目 CRUD + 提升为全局） ----------

/// 校验并规范化条目正文 / 适用范围（均须非空）。返回 Err(错误文案)。
fn knowledge_fields(body: &Value) -> Result<(String, Option<String>), String> {
    let b = body["body"].as_str().map(str::trim).filter(|s| !s.is_empty())
        .ok_or("body is required")?;
    let applies = body["applies_to"].as_str().map(str::trim)
        .filter(|s| !s.is_empty()).map(String::from);
    Ok((b.to_string(), applies))
}

/// GET /api/projects/:id/knowledge —— 本项目条目 + 全局条目（列表不写审计，防刷屏）。
async fn list_project_knowledge(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let c = st.db.lock().unwrap();
    if db::get_project(&c, &id).is_none() {
        return err(StatusCode::NOT_FOUND, "project not found");
    }
    ok(db::list_knowledge(&c, &id))
}

/// POST /api/projects/:id/knowledge —— 新建条目（须引用一条已存在的人工指导事件）。
async fn create_knowledge(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let (b, applies) = match knowledge_fields(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let source_event_id = body["source_event_id"].as_i64();
    let Some(source_event_id) = source_event_id else {
        return err(StatusCode::BAD_REQUEST, "source_event_id is required");
    };
    let c = st.db.lock().unwrap();
    if db::get_project(&c, &id).is_none() {
        return err(StatusCode::NOT_FOUND, "project not found");
    }
    if db::get_project(&c, &id).map(|p| p.status) == Some("archived".into()) {
        return err(StatusCode::CONFLICT, "project is archived (read-only)");
    }
    // 来源可追溯：source_event_id 必须指向一条真实存在的人工指导事件
    match db::get_event(&c, source_event_id) {
        None => return err(StatusCode::BAD_REQUEST, "source_event_id does not reference an existing event"),
        Some(ev) if ev.kind != "task.guidance_added" => {
            return err(StatusCode::BAD_REQUEST,
                format!("source_event_id must reference a task.guidance_added event (got {})", ev.kind));
        }
        Some(_) => {}
    }
    let now = db::now();
    let k = KnowledgeEntry {
        id: db::new_id("kn_"),
        project_id: id.clone(),
        scope: "project".into(),
        body: b,
        applies_to: applies,
        source_event_id,
        created_at: now,
        updated_at: now,
    };
    db::insert_knowledge(&c, &k);
    events::push_event(&st, &c, &k.project_id, None, "knowledge.created", knowledge::entry_payload(&k));
    ok(k)
}

/// GET /api/knowledge/:id —— 详情（写 `knowledge.viewed` 审计）。
async fn get_knowledge(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(k) = db::get_knowledge(&c, &id) else {
        return err(StatusCode::NOT_FOUND, "knowledge entry not found");
    };
    events::push_event(&st, &c, &k.project_id, None, "knowledge.viewed", knowledge::entry_payload(&k));
    ok(k)
}

/// PATCH /api/knowledge/:id —— 修改正文 / 适用范围（写 `knowledge.updated`）。
async fn patch_knowledge(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let (b, applies) = match knowledge_fields(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let c = st.db.lock().unwrap();
    let Some(k) = db::get_knowledge(&c, &id) else {
        return err(StatusCode::NOT_FOUND, "knowledge entry not found");
    };
    if db::get_project(&c, &k.project_id).map(|p| p.status) == Some("archived".into()) {
        return err(StatusCode::CONFLICT, "project is archived (read-only)");
    }
    let updated_at = db::now();
    db::update_knowledge(&c, &id, &b, applies.as_deref(), updated_at);
    let mut k2 = k.clone();
    k2.body = b;
    k2.applies_to = applies;
    k2.updated_at = updated_at;
    events::push_event(&st, &c, &k2.project_id, None, "knowledge.updated", json!({
        "entry": knowledge::entry_payload(&k2),
        "prev": knowledge::entry_payload(&k),
    }));
    ok(k2)
}

/// DELETE /api/knowledge/:id —— 删除条目（写 `knowledge.deleted`，payload 为删除前快照）。
async fn delete_knowledge(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(k) = db::get_knowledge(&c, &id) else {
        return err(StatusCode::NOT_FOUND, "knowledge entry not found");
    };
    if db::get_project(&c, &k.project_id).map(|p| p.status) == Some("archived".into()) {
        return err(StatusCode::CONFLICT, "project is archived (read-only)");
    }
    db::delete_knowledge(&c, &id);
    events::push_event(&st, &c, &k.project_id, None, "knowledge.deleted", knowledge::entry_payload(&k));
    ok(k)
}

/// POST /api/knowledge/:id/promote —— 提升为全局条目（跨项目复用；幂等 409）。
async fn promote_knowledge(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(k) = db::get_knowledge(&c, &id) else {
        return err(StatusCode::NOT_FOUND, "knowledge entry not found");
    };
    if k.scope == "global" {
        return err(StatusCode::CONFLICT, "knowledge entry is already global");
    }
    if db::get_project(&c, &k.project_id).map(|p| p.status) == Some("archived".into()) {
        return err(StatusCode::CONFLICT, "project is archived (read-only)");
    }
    let updated_at = db::now();
    db::set_knowledge_scope(&c, &id, "global", updated_at);
    let mut k2 = k.clone();
    k2.scope = "global".into();
    k2.updated_at = updated_at;
    events::push_event(&st, &c, &k2.project_id, None, "knowledge.promoted", json!({
        "entry": knowledge::entry_payload(&k2),
        "from_project": k.project_id,
    }));
    ok(k2)
}

fn human_verdict(st: &Arc<App>, id: &str, accept: bool, comment: Option<&str>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(t) = db::get_task(&c, id) else { return err(StatusCode::NOT_FOUND, "task not found"); };
    if db::get_project(&c, &t.project_id).map(|p| p.status) == Some("archived".into()) {
        return err(StatusCode::CONFLICT, "project is archived (read-only)");
    }
    if t.status != "awaiting_acceptance" {
        return err(StatusCode::CONFLICT, format!("task is not awaiting acceptance (status={})", t.status));
    }
    // human takeover: kill any pending agent-side acceptance session
    drop(c);
    st.abort_task_handles(id);
    let c = st.db.lock().unwrap();
    if let Some(acc) = t.acceptor_agent_id.clone() {
        db::occupancy_remove(&c, &acc, &t.id);
        if let Some(ag) = db::get_agent(&c, &acc) { events::send_agent(st, &ag); }
    }
    drop(c);

    if accept {
        let c = st.db.lock().unwrap();
        events::push_event(st, &c, &t.project_id, Some(&t.id), "acceptance.recorded",
            json!({"verdict": "approved", "by": "human", "comment": comment}));
        let t2 = events::status_changed(st, &c, &t, "done", "human_accepted");
        drop(c);
        st.wake();
        return ok(t2);
    }
    // reject path
    let c = st.db.lock().unwrap();
    let rc = t.rejection_count + 1;
    let _ = c.execute("UPDATE tasks SET rejection_count=?2 WHERE id=?1", rusqlite::params![t.id, rc]);
    let mut f = t.clone();
    f.rejection_count = rc;
    events::push_event(st, &c, &t.project_id, Some(&t.id), "acceptance.rejected",
        json!({"verdict": "rejected", "by": "human", "comment": comment}));
    if rc >= 2 {
        let t3 = events::status_changed(st, &c, &f, "escalated", "rejection_limit");
        events::push_event(st, &c, &t.project_id, Some(&t.id), "escalation.raised",
            json!({"task_id": t.id, "reason": "rejection_limit", "detail": "human rejected twice; escalated"}));
        events::new_inbox_item(st, &c, &t.project_id, "escalation",
            json!({"task_id": t.id, "reason": "rejection_limit", "detail": format!("task '{}' rejected {} times (human)", t.title, rc)}));
        drop(c);
        st.wake();
        ok(t3)
    } else {
        let t2 = events::status_changed(st, &c, &f, "executing", "human_rejected_redo");
        drop(c);
        st.wake();
        ok(t2)
    }
}

async fn task_accept(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    human_verdict(&st, &id, true, body["comment"].as_str())
}

async fn task_reject(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let Some(comment) = body["comment"].as_str() else {
        return err(StatusCode::BAD_REQUEST, "comment is required");
    };
    human_verdict(&st, &id, false, Some(comment))
}


/// POST /api/tasks/{id}/replan：executor/acceptor 上报「发现规划问题」。
///
/// 生成一条 kind=`replan` 的人工收件箱条目（payload.kind=replan_report）；人批准后
/// `replan::scan` 派发规划并落地，否决则关闭请求。同一任务的未决请求幂等去重。
async fn report_task_replan(
    State(st): State<Arc<App>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let c = st.db.lock().unwrap();
    let Some(t) = db::get_task(&c, &id) else {
        return err(StatusCode::NOT_FOUND, "task not found");
    };
    if t.status == "done" || t.status == "cancelled" {
        return err(StatusCode::CONFLICT, format!("task '{}' is terminal ({})", t.title, t.status));
    }
    if db::get_project(&c, &t.project_id).map(|p| p.status) == Some("archived".into()) {
        return err(StatusCode::CONFLICT, "project is archived (read-only)");
    }
    let reason = body["reason"].as_str().map(str::trim).unwrap_or("");
    if reason.is_empty() {
        return err(StatusCode::BAD_REQUEST, "reason is required");
    }
    let proposed_changes = body
        .get("proposed_changes")
        .filter(|v| v.is_array())
        .cloned()
        .unwrap_or_else(|| json!([]));
    let request = json!({
        "reason": reason,
        "detail": body["detail"].as_str().unwrap_or(""),
        "reported_by": body["reported_by"].as_str().unwrap_or("executor"),
        "reporter_agent_id": body["reporter_agent_id"].clone(),
        "task_id": t.id,
        "task_title": t.title,
        "proposed_changes": proposed_changes,
        "snapshot": {
            "status": t.status,
            "milestone_id": t.milestone_id,
            "depends_on": t.depends_on,
            "acceptance_criteria": t.acceptance_criteria,
        },
    });
    reopen_if_completed(&st, &c, &t.project_id);
    let (scope, reasons) = replan::classify(Some(&t), &request);
    events::push_event(&st, &c, &t.project_id, Some(&t.id), "replan.proposed",
        json!({
            "reason": request["reason"],
            "detail": request["detail"],
            "reported_by": request["reported_by"],
            "proposed_changes": request["proposed_changes"],
            "task_id": t.id,
            "task_title": t.title,
            "scope": scope,
        }));
    let (item, deduplicated) = replan::open_request(&st, &c, &t.project_id, &t, request, &scope);
    drop(c);
    st.wake();
    ok(json!({"inbox": item, "deduplicated": deduplicated, "reasons": reasons, "scope": scope}))
}

// ---------- agents ----------

async fn list_agents(
    State(st): State<Arc<App>>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let c = st.db.lock().unwrap();
    // ?all=1 → 含软删除实例（审计名称回溯用；软删除快照由其保留行提供）
    if q.get("all").map(|v| v == "1").unwrap_or(false) {
        let mut stmt = c.prepare("SELECT * FROM agents ORDER BY rowid").unwrap();
        let v = stmt
            .query_map([], db::agent_row)
            .unwrap()
            .filter_map(|x| x.ok())
            .collect::<Vec<_>>();
        return ok(v);
    }
    ok(db::list_agents(&c))
}

async fn create_agent(State(st): State<Arc<App>>, Json(body): Json<Value>) -> Response {
    let name = body["name"].as_str().filter(|s| !s.trim().is_empty());
    let Some(name) = name else { return err(StatusCode::BAD_REQUEST, "name is required"); };
    let adapter = body["adapter"].as_str().unwrap_or("mock");
    if adapter != "mock" && adapter != "opencode" {
        return err(StatusCode::BAD_REQUEST, "adapter must be 'mock' or 'opencode'");
    }
    let roles: Vec<String> = body["roles"].as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if roles.is_empty() || roles.iter().any(|r| !ROLES.contains(&r.as_str())) {
        return err(StatusCode::BAD_REQUEST, format!("roles must be a non-empty subset of {ROLES:?}"));
    }
    let config = body["config"].as_object().cloned().map(Value::Object).unwrap_or_else(|| json!({"duration_ms": 4000}));
    let capabilities: Capabilities = serde_json::from_value(body["capabilities"].clone()).unwrap_or_default();
    if capabilities.max_parallel < 1 { return err(StatusCode::BAD_REQUEST, "max_parallel must be >= 1"); }
    // M2: opencode agents default to resumable sessions (CLI --session support)
    let mut capabilities = capabilities;
    if adapter == "opencode" && !body["capabilities"]["supports_resume"].is_boolean() {
        capabilities.supports_resume = true;
    }
    let a = Agent {
        id: db::new_id("agt_"),
        name: name.trim().into(),
        adapter: adapter.into(),
        roles,
        config,
        capabilities,
        status: "idle".into(),
        current_task_ids: vec![],
    };
    {
        let c = st.db.lock().unwrap();
        c.execute(
            "INSERT INTO agents (id,name,adapter,roles,config,capabilities,status,current_task_ids) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                a.id, a.name, a.adapter,
                db::jstring(&serde_json::to_value(&a.roles).unwrap()),
                db::jstring(&a.config),
                db::jstring(&serde_json::to_value(&a.capabilities).unwrap()),
                a.status, "[]"
            ],
        ).unwrap();
        events::push_event(&st, &c, "-", None, "agent.created",
            json!({"id": a.id, "name": a.name, "roles": a.roles}));
    }
    st.wake();
    ok(a)
}

/// 编辑 agent：name / roles / config / capabilities 部分更新（adapter 不变）。
async fn patch_agent(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let c = st.db.lock().unwrap();
    let Some(mut a) = db::get_agent(&c, &id) else { return err(StatusCode::NOT_FOUND, "agent not found"); };
    if a.status == "deleted" {
        return err(StatusCode::CONFLICT, "agent has been deleted (soft-deleted; audit snapshot remains)");
    }
    if let Some(v) = body["name"].as_str().filter(|s| !s.trim().is_empty()) {
        a.name = v.trim().into();
    }
    if !body["roles"].is_null() {
        let roles: Vec<String> = body["roles"].as_array()
            .map(|r| r.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if roles.is_empty() || roles.iter().any(|r| !ROLES.contains(&r.as_str())) {
            return err(StatusCode::BAD_REQUEST, format!("roles must be a non-empty subset of {ROLES:?}"));
        }
        let roles_set: std::collections::HashSet<String> = roles.iter().cloned().collect();
        // 悬空检查：撤销 executor/acceptor 角色时，不得仍有非终态任务以此身份引用该实例
        //（否则任务会卡在「派发被角色守卫拦下」的悬空状态）。
        for (role, col) in [("executor", "executor_agent_id"), ("acceptor", "acceptor_agent_id")] {
            if a.roles.iter().any(|r| r == role) && !roles_set.contains(&role.to_string()) {
                let mut stmt = c.prepare(&format!(
                    "SELECT id, title, status FROM tasks WHERE {col}=?1 \
                     AND status IN ('planned','ready','executing','awaiting_acceptance','suspended','escalated')",
                )).unwrap();
                let rows = stmt.query_map(rusqlite::params![id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                }).unwrap().collect::<Vec<_>>();
                let refs: Vec<Value> = rows.into_iter()
                    .map(|r| { let (tid, title, status) = r.unwrap(); json!({"task_id": tid, "title": title, "status": status}) })
                    .collect();
                if !refs.is_empty() {
                    return err(StatusCode::CONFLICT, format!(
                        "cannot remove '{role}' role: non-terminal tasks still reference this agent as {role}; reassign them first: {}",
                        serde_json::to_string(&refs).unwrap()));
                }
            }
        }
        a.roles = roles;
    }
    if let Some(cfg) = body["config"].as_object() {
        a.config = Value::Object(cfg.clone());
    }
    if let Some(mp) = body["capabilities"]["max_parallel"].as_i64() {
        if mp < 1 { return err(StatusCode::BAD_REQUEST, "max_parallel must be >= 1"); }
        a.capabilities.max_parallel = mp;
    }
    if let Some(sr) = body["capabilities"]["supports_resume"].as_bool() {
        a.capabilities.supports_resume = sr;
    }
    {
        c.execute(
            "UPDATE agents SET name=?2, roles=?3, config=?4, capabilities=?5 WHERE id=?1",
            rusqlite::params![
                a.id, a.name,
                db::jstring(&serde_json::to_value(&a.roles).unwrap()),
                db::jstring(&a.config),
                db::jstring(&serde_json::to_value(&a.capabilities).unwrap()),
            ],
        ).unwrap();
        events::push_event(&st, &c, "-", Some(&a.id), "agent.updated",
            json!({"id": a.id, "name": a.name, "roles": a.roles, "adapter": a.adapter}));
    }
    if let Some(a) = db::get_agent(&c, &a.id) {
        events::send_agent(&st, &a);
        return ok(a);
    }
    unreachable!()
}

/// 删除 agent：被任何非终态任务引用（执行中/待验收/挂起/升级/待派发）→ 409 拒绝，
/// 只被已完成/已取消任务提及的引用允许保留（历史显示会回退为「已删除 agent」）。
async fn delete_agent(State(st): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let non_terminal = ["planned", "ready", "executing", "awaiting_acceptance", "suspended", "escalated"];
    let (blocked, name) = {
        let c = st.db.lock().unwrap();
        let Some(a) = db::get_agent(&c, &id) else { return err(StatusCode::NOT_FOUND, "agent not found"); };
        let mut blockers: Vec<Value> = Vec::new();
        let mut stmt = c.prepare(
            "SELECT id, title, status FROM tasks WHERE (executor_agent_id=?1 OR acceptor_agent_id=?1)",
        ).unwrap();
        let rows = stmt.query_map(rusqlite::params![id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        }).unwrap().collect::<Vec<_>>();
        for row in rows {
            let (tid, title, status) = row.unwrap();
            if non_terminal.contains(&status.as_str()) {
                blockers.push(json!({"task_id": tid, "title": title, "status": status}));
            }
        }
        if !blockers.is_empty() {
            (Some(blockers), a.name)
        } else {
            // 软删除：保留行（status='deleted'）供审计快照回溯；清掉占用并标记
            let _ = c.execute(
                "UPDATE agents SET status='deleted', current_task_ids='[]' WHERE id=?1",
                rusqlite::params![id],
            );
            events::push_event(&st, &c, "-", Some(&id), "agent.deleted",
                json!({"id": id, "name": a.name, "adapter": a.adapter}));
            (None, a.name)
        }
    };
    match blocked {
        Some(b) => err(StatusCode::CONFLICT, format!(
            "agent '{name}' is referenced by non-terminal tasks; reassign the tasks (inbox adjudication / task edit) first: {}",
            serde_json::to_string(&b).unwrap())),
        None => {
            tracing::info!(agent = %id, "agent deleted");
            ok(json!({"deleted": id}))
        }
    }
}

// ---------- inbox ----------

/// 为收件箱条目附加裁决上下文（兼容历史数据：读时计算而非只依赖创建 payload）。
/// escalation 类附带任务全貌、最近失败原因、涉及 agent 及其配置，帮助人判断
/// 问题是出在执行端（→ 修配置/换执行者）还是验收端（→ 换验收者/放宽标准）。
fn inbox_context(c: &rusqlite::Connection, item: &InboxItem) -> Option<Value> {
    if item.kind != "escalation" { return None; }
    let task_id = item.payload["task_id"].as_str()?;
    let t = db::get_task(c, task_id)?;
    // 最近一次 run 级失败（执行失败 / 验收退回 / 验收基建失败）及其事件类型
    let last_failure = c.query_row(
        "SELECT type, payload FROM events WHERE task_id=?1 AND type IN \
         ('task.execution_failed','acceptance.rejected','acceptance.run_failed') ORDER BY ts DESC LIMIT 1",
        rusqlite::params![task_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    ).ok().and_then(|(typ, raw)| {
        serde_json::from_str::<Value>(&raw).ok().map(|p| {
            let msg = p.get("error").and_then(|e| e.as_str())
                .or_else(|| p.get("comment").and_then(|e| e.as_str()))
                .map(|s| json!(s))
                .unwrap_or(json!(null));
            json!({"type": typ, "error": msg})
        })
    });
    let last_failure_type = last_failure.as_ref()
        .and_then(|f| f.get("type")).and_then(|t| t.as_str())
        .map(String::from)
        .or_else(|| item.payload["last_failure_type"].as_str().map(String::from));
    let last_error = last_failure.as_ref()
        .and_then(|f| f.get("error")).cloned()
        .filter(|e| !e.is_null())
        .or_else(|| {
            let v = item.payload["last_error"].clone();
            if v.is_null() { None } else { Some(v) }
        });
    let agent_json = |id: &Option<String>| -> Option<Value> {
        id.as_deref()
            .filter(|s| !s.is_empty())
            .and_then(|aid| db::get_agent(c, aid))
            .map(|a| json!({
                "id": a.id, "name": a.name, "adapter": a.adapter,
                "roles": a.roles, "status": a.status,
                "model": a.config.get("model").cloned().unwrap_or(json!(null)),
                "duration_ms": a.config.get("duration_ms").cloned().unwrap_or(json!(null)),
            }))
    };
    // 指引按真实失败环节归类：三档恢复耗尽 / 执行失败 / 验收基建失败 / 验收退回，其次才看 reason
    let phase_hint = if item.payload["reason"].as_str() == Some(crate::recovery::INBOX_REASON) {
        "三档恢复耗尽（M4）：原生会话恢复与交接摘要重建都失败，任务已挂起。可先核对 payload 中两档失败原因——\
         若是执行环境问题（工作区/模型/限流），修复后重启服务会自动再进一轮三档恢复，或直接「恢复重派」；\
         也可「更换执行者」后重派；确认无法自动续作时用「放弃」清理下游。"
    } else {
        let hint_key = last_failure_type.as_deref().unwrap_or(match item.payload["reason"].as_str() {
            Some("rejection_limit") => "acceptance.rejected",
            Some("acceptance_failure_limit") => "acceptance.run_failed",
            _ => "task.execution_failed",
        });
        match hint_key {
            "acceptance.run_failed" =>
                "验收端运行失败（执行已成功、成果保留；验收者 agent 本身跑不起来，如限流/超时/配置错误）。换验收者（任务编辑或「更换验收者」）后「恢复重派」；放宽标准/换执行者都解决不了这类问题。",
            "acceptance.rejected" =>
                "验收端退回升级（执行已成功，验收者给出了理由）。可与验收标准核对后「放宽标准」，或「更换验收者」重审，或按退回意见让执行者返工后「恢复重派」。",
            _ =>
                "执行端失败（从未进入验收）。常见原因：执行者 agent 配置问题（如 model 无效、超时过短）→ 修复配置后「恢复重派」，或「更换执行者」；换验收者解决不了执行问题。",
        }
    };
    Some(json!({
        "task": {
            "id": t.id, "title": t.title, "status": t.status,
            "description": t.description,
            "acceptance_criteria": t.acceptance_criteria,
            "attempt": t.attempt, "failure_count": t.failure_count,
            "rejection_count": t.rejection_count,
        },
        "executor": agent_json(&t.executor_agent_id),
        "acceptor": agent_json(&t.acceptor_agent_id),
        "last_failure_type": last_failure_type,
        "last_error": last_error,
        "phase_hint": phase_hint,
    }))
}

/// 列表（每条 open escalation 附 context 字段）
/// 收件箱条目的可用动作（按待决类型下发，前端据此渲染，不再写死）。
fn inbox_actions(i: &crate::models::InboxItem) -> Vec<Value> {
    if i.status != "open" {
        return vec![];
    }
    let act = |id: &str, label: &str| json!({"id": id, "label": label});
    if i.kind == "milestone_gate" {
        return vec![act("pass", "确认通过"), act("reject", "退回重规划")];
    }
    match i.kind.as_str() {
        "escalation" => {
            let reason = i.payload.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            if reason == "merge_conflict" {
                return vec![
                    json!({"id":"resume","label":"恢复重派"}),
                    json!({"id":"abandon","label":"放弃任务"}),
                ];
            }
            vec![
                json!({"id":"resume","label":"恢复重派"}),
                json!({"id":"reassign","label":"更换执行者","needs_agent":"executor"}),
                json!({"id":"reassign_acceptor","label":"更换验收者","needs_agent":"acceptor"}),
                json!({"id":"relax","label":"放宽标准"}),
                json!({"id":"abandon","label":"放弃任务"}),
            ]
        }
        "project_action_blocked" => vec![
            json!({"id":"retry_pause","label":"重试强制暂停"}),
            json!({"id":"close","label":"暂不处理"}),
        ],
        "replan" => {
            let kind = i.payload.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if kind == "milestone_window" {
                vec![] // 窗口有自己的编辑/应用界面
            } else if kind == "replan_report" {
                let planning = i.payload.get("planning").and_then(|v| v.as_str()).unwrap_or("");
                if planning == "pending" || planning == "applied" {
                    vec![]
                } else {
                    vec![json!({"id":"approve","label":"批准"}), json!({"id":"deny","label":"否决"})]
                }
            } else {
                vec![json!({"id":"close","label":"确认处理"})]
            }
        }
        _ => vec![json!({"id":"close","label":"确认处理"})],
    }
}

async fn list_inbox(State(st): State<Arc<App>>) -> Response {
    let c = st.db.lock().unwrap();
    let items = db::list_inbox(&c);
    let out: Vec<Value> = items
        .into_iter()
        .map(|i| {
            let mut v = serde_json::to_value(&i).unwrap_or(json!({}));
            if i.status == "open" {
                if let Some(ctx) = inbox_context(&c, &i) {
                    v["context"] = ctx;
                }
            }
            if i.status == "open" {
                v["actions"] = json!(inbox_actions(&i));
            }
            v
        })
        .collect();
    ok(out)
}

async fn resolve_inbox(State(st): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let resolution = body["resolution"].as_str();
    let Some(resolution) = resolution else { return err(StatusCode::BAD_REQUEST, "resolution is required"); };
    let note = body["note"].as_str().unwrap_or("");
    let c = st.db.lock().unwrap();
    let Some(item) = db::get_inbox(&c, &id) else { return err(StatusCode::NOT_FOUND, "inbox item not found"); };
    if item.status != "open" { return err(StatusCode::CONFLICT, "inbox item already resolved"); }

    let project_id = item.project_id.clone();
    let mut payload_out: Value = json!({"resolution": resolution, "note": note});
    let mut if_error: Option<Response> = None;

    // resolve only applies to escalation items with a task target; replan items just close.
    let task_id = item.payload["task_id"].as_str().map(String::from);
    let task = task_id.as_ref().and_then(|t| db::get_task(&c, t));

    if item.kind == "milestone_gate" {
        // M3 gate adjudication: pass → milestone.passed (downstream unblocked);
        // reject → milestone.failed (downstream stays frozen for replanning).
        let Some(mid) = item.payload["milestone_id"].as_str().map(String::from) else {
            drop(c);
            return err(StatusCode::NOT_FOUND, "milestone_gate payload has no milestone_id");
        };
        let Some(m) = db::get_milestone(&c, &mid) else {
            drop(c);
            return err(StatusCode::NOT_FOUND, "milestone not found");
        };
        if m.status != "awaiting_confirm" && m.status != "failed" {
            drop(c);
            return err(StatusCode::CONFLICT, format!("milestone is not awaiting confirmation (status={})", m.status));
        }
        match resolution {
            "pass" | "confirm" | "approve" | "approved" => {
                events::milestone_status_changed(&st, &c, &m, "passed", "gate_confirmed");
                events::push_event(&st, &c, &m.project_id, None, "milestone.passed",
                    json!({"milestone_id": m.id, "name": m.name, "note": note}));
                payload_out["milestone_id"] = json!(m.id);
                payload_out["verdict"] = json!("passed");
            }
            "fail" | "reject" | "rejected" => {
                let m2 = events::milestone_status_changed(&st, &c, &m, "failed", "gate_rejected");
                events::push_event(&st, &c, &m.project_id, None, "milestone.failed",
                    json!({"milestone_id": m.id, "name": m.name, "note": note}));
                // 退回 → 重开重规划窗口（幂等）；窗口项作为独立 open 条目供人操作。
                let window = open_milestone_window(&st, &c, &m2, note);
                payload_out["milestone_id"] = json!(m.id);
                payload_out["verdict"] = json!("failed");
                payload_out["window_inbox_id"] = json!(window.id);
            }
            other => {
                if_error = Some(err(StatusCode::BAD_REQUEST,
                    format!("unknown milestone gate resolution: {other} (use pass|reject)")));
            }
        }
        drop(c);
    } else if item.kind == "project_action_blocked" {
        match resolution {
            "retry_pause" => {
                let Some(p) = db::get_project(&c, &project_id) else {
                    drop(c);
                    return err(StatusCode::NOT_FOUND, "project not found");
                };
                if p.status != "running" && p.status != "pausing" {
                    drop(c);
                    return err(StatusCode::CONFLICT, "project is not running");
                }
                if let Some(eid) = p.engineering_id.as_deref() {
                    let busy = db::list_projects_of(&c, eid)
                        .into_iter()
                        .any(|x| x.id != project_id && (x.status == "running" || x.status == "pausing"));
                    if busy {
                        drop(c);
                        return err(StatusCode::CONFLICT, "同工程仍有运行中项目；请先处理后再重试");
                    }
                }
                let tids: Vec<String> = db::list_tasks(&c, &project_id).into_iter().map(|t| t.id).collect();
                let aborted: Vec<String> = db::list_tasks(&c, &project_id)
                    .into_iter().filter(|t| st.has_live_handle(&t.id)).map(|t| t.id).collect();
                drop(c);
                st.abort_project_handles(&tids);
                let c = st.db.lock().unwrap();
                events::push_event(&st, &c, &project_id, None, "project.pause_applied",
                    json!({"mode": "force", "scope": "project", "aborted_attempts": aborted.len(),
                           "task_ids": aborted, "source": "inbox_retry"}));
                for tid in &tids {
                    if let Some(t) = db::get_task(&c, tid) {
                        for a in [t.executor_agent_id.clone(), t.acceptor_agent_id.clone()].into_iter().flatten() {
                            db::occupancy_remove(&c, &a, &t.id);
                            if let Some(ag) = db::get_agent(&c, &a) { events::send_agent(&st, &ag); }
                        }
                    }
                }
                let _ = c.execute("UPDATE projects SET status='paused' WHERE id=?1", rusqlite::params![project_id]);
                if let Some(p) = db::get_project(&c, &project_id) { events::send_project(&st, &p); }
                events::push_event(&st, &c, &project_id, None, "project.force_paused", json!({"source": "inbox"}));
                payload_out["project_status"] = json!("paused");
                drop(c);
            }
            "close" => drop(c),
            other => {
                if_error = Some(err(StatusCode::BAD_REQUEST, format!("unknown project action: {other}")));
                drop(c);
            }
        }
    } else if item.kind == "replan"
        && (item.payload.get("kind").and_then(|v| v.as_str()) == Some("replan_report")
            || (item.payload.get("task_id").is_some()
                && item.payload.get("milestone_id").is_none()
                && item.payload.get("downstream").is_none()
                && item.payload.get("abandoned_task_id").is_none()))
    {
        // 重规划请求：所有批准/否决都由人做（没有决策者 agent）。
        match resolution {
            "approve" | "approved" | "accept" | "accepted" | "pass" => {
                let mut p = item.payload.clone();
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("approved".into(), json!(true));
                    obj.insert("approved_by".into(), json!("human"));
                    obj.insert("note".into(), json!(note));
                    obj.insert("planning".into(), json!("pending"));
                }
                let _ = c.execute(
                    "UPDATE inbox SET payload=?2 WHERE id=?1",
                    rusqlite::params![item.id, p.to_string()],
                );
                events::push_event(&st, &c, &project_id,
                    item.payload.get("task_id").and_then(|v| v.as_str()),
                    "replan.approved", json!({"inbox_id": item.id, "by": "human", "note": note}));
                payload_out["planning"] = json!("pending");
            }
            "deny" | "denied" | "reject" | "rejected" => {
                if let Some(t) = task.as_ref() {
                    events::log_line(&st, &c, &t.id, "重规划请求被人否决，维持原规划");
                }
                events::push_event(&st, &c, &project_id,
                    item.payload.get("task_id").and_then(|v| v.as_str()),
                    "replan.rejected", json!({"inbox_id": item.id, "by": "human", "note": note}));
                payload_out["planning"] = json!("rejected");
            }
            other => {
                if_error = Some(err(StatusCode::BAD_REQUEST,
                    format!("unknown replan resolution: {other} (use approve|deny)")));
            }
        }
        drop(c);
    } else if item.kind != "escalation" {
        // milestone window / other: just close (release the lock before the tail re-lock)
        drop(c);
    } else {
        let Some(t) = task else {
            drop(c);
            return err(StatusCode::NOT_FOUND, "escalation payload has no valid task");
        };
        if t.status != "escalated" && t.status != "suspended" {
            drop(c);
            return err(StatusCode::CONFLICT, format!("task is not in an escalation state (status={})", t.status));
        }
        match resolution {
            "resume" => {
                // clear counters, re-dispatch original agent
                let _ = c.execute("UPDATE tasks SET rejection_count=0, failure_count=0 WHERE id=?1", rusqlite::params![t.id]);
                db::task_tried_clear(&c, &t.id); // M4.5：人工恢复 → 重新给池内所有 agent 机会
                let fresh = db::get_task(&c, &t.id).unwrap_or(t.clone());
                events::status_changed(&st, &c, &fresh, "executing", "escalation_resumed");
                drop(c);
            }
            "reassign" => {
                let wanted = body["executor_agent_id"].as_str().map(String::from)
                    .or_else(|| body["agent_id"].as_str().map(String::from));
                let candidate = match wanted {
                    Some(a) => match db::get_agent(&c, &a) {
                        None => {
                            drop(c);
                            return err(StatusCode::NOT_FOUND, "agent not found");
                        }
                        Some(ag) if !ag.roles.iter().any(|r| r == "executor") => {
                            drop(c);
                            return err(StatusCode::BAD_REQUEST, format!("agent '{}' lacks the 'executor' role", ag.name));
                        }
                        Some(ag) => Some(ag),
                    },
                    None => {
                        // auto: pick another idle executor-capable instance
                        let mut best: Option<Agent> = None;
                        for a in db::list_agents(&c) {
                            if a.roles.iter().any(|r| r == "executor")
                                && a.current_task_ids.is_empty()
                                && a.id != t.executor_agent_id.clone().unwrap_or_default()
                            {
                                best = Some(a);
                                break;
                            }
                        }
                        best
                    }
                };
                match candidate {
                    Some(ag) => {
                        let _ = c.execute("UPDATE tasks SET executor_agent_id=?2, rejection_count=0, failure_count=0 WHERE id=?1",
                            rusqlite::params![t.id, ag.id]);
                        let fresh = db::get_task(&c, &t.id).unwrap_or(t.clone());
                        let _ = events::status_changed(&st, &c, &fresh, "executing", "escalation_reassigned");
                        payload_out["executor_agent_id"] = json!(ag.id);
                        drop(c);
                    }
                    None => {
                        // 没有空闲执行者：不落已处理，明确报错让用户在 UI 指定 agent",
                        drop(c);
                        return err(StatusCode::CONFLICT,
                            "no idle executor-capable agent available; task left suspended — pick a specific agent instead");
                    }
                }
            }
            "reassign_acceptor" => {
                // 更换验收者：仅适用于验收退回升级（执行已成功，成果保留）。
                if t.status != "escalated" {
                    drop(c);
                    return err(StatusCode::CONFLICT,
                        "reassigning acceptor only applies to tasks escalated by acceptance rejections (execution result is preserved)");
                }
                let wanted = body["agent_id"].as_str().map(String::from)
                    .or_else(|| body["executor_agent_id"].as_str().map(String::from));
                let candidate = match wanted {
                    Some(a) => {
                        let Some(ag) = db::get_agent(&c, &a) else {
                            drop(c);
                            return err(StatusCode::NOT_FOUND, "agent not found");
                        };
                        if !ag.roles.iter().any(|r| r == "acceptor") {
                            drop(c);
                            return err(StatusCode::BAD_REQUEST, "candidate agent lacks 'acceptor' role");
                        }
                        if ag.id == t.executor_agent_id.clone().unwrap_or_default() {
                            drop(c);
                            return err(StatusCode::BAD_REQUEST, "acceptor must differ from the executor");
                        }
                        Some(ag)
                    }
                    None => {
                        // auto: pick another acceptor-capable instance
                        let mut best: Option<Agent> = None;
                        for a in db::list_agents(&c) {
                            if a.roles.iter().any(|r| r == "acceptor")
                                && a.id != t.acceptor_agent_id.clone().unwrap_or_default()
                                && a.id != t.executor_agent_id.clone().unwrap_or_default()
                            {
                                best = Some(a);
                                break;
                            }
                        }
                        best
                    }
                };
                let Some(ag) = candidate else {
                    drop(c);
                    return err(StatusCode::CONFLICT, "no other acceptor-capable agent available; register one first");
                };
                // abort any pending accept session of the old acceptor, free occupancy
                drop(c);
                st.abort_task_handles(&t.id);
                let c = st.db.lock().unwrap();
                if let Some(old) = t.acceptor_agent_id.clone() {
                    db::occupancy_remove(&c, &old, &t.id);
                    if let Some(ag2) = db::get_agent(&c, &old) { events::send_agent(&st, &ag2); }
                }
                let _ = c.execute(
                    "UPDATE tasks SET acceptor_agent_id=?2, rejection_count=0 WHERE id=?1",
                    rusqlite::params![t.id, ag.id],
                );
                let fresh = db::get_task(&c, &t.id).unwrap_or(t.clone());
                events::status_changed(&st, &c, &fresh, "awaiting_acceptance", "escalation_acceptor_reassigned");
                payload_out["agent_id"] = json!(ag.id);
                drop(c);
            }
            "relax" => {
                // lower the bar: clear rejections, redo
                let _ = c.execute("UPDATE tasks SET rejection_count=0 WHERE id=?1", rusqlite::params![t.id]);
                let fresh = db::get_task(&c, &t.id).unwrap_or(t.clone());
                let _ = events::status_changed(&st, &c, &fresh, "executing", "acceptance_relaxed");
                drop(c);
            }
            "abandon" => {
                events::status_changed(&st, &c, &t, "cancelled", "escalation_abandoned");
                let agent_ids: Vec<String> = vec![
                    t.executor_agent_id.clone().unwrap_or_default(),
                    t.acceptor_agent_id.clone().unwrap_or_default(),
                ];
                for a in agent_ids {
                    if !a.is_empty() {
                        db::occupancy_remove(&c, &a, &t.id);
                        if let Some(ag) = db::get_agent(&c, &a) { events::send_agent(&st, &ag); }
                    }
                }
                // downstream dependents -> one aggregated replan inbox item
                let all = db::list_tasks(&c, &t.project_id);
                let mut downstream: Vec<String> = Vec::new();
                let mut frontier = vec![t.id.clone()];
                let mut seen = std::collections::HashSet::new();
                while let Some(n) = frontier.pop() {
                    for other in &all {
                        if other.depends_on.iter().any(|d| d == &n) && seen.insert(other.id.clone()) {
                            frontier.push(other.id.clone());
                            if other.id != t.id {
                                downstream.push(other.id.clone());
                            }
                        }
                    }
                }
                if !downstream.is_empty() {
                    events::push_event(&st, &c, &t.project_id, Some(&t.id), "replan.proposed",
                        json!({"abandoned_task": t.id, "downstream": downstream}));
                    events::new_inbox_item(&st, &c, &t.project_id, "replan",
                        json!({
                            "detail": format!("task '{}' abandoned; {} downstream tasks need replanning", t.title, downstream.len()),
                            "abandoned_task_id": t.id,
                            "downstream": downstream
                        }));
                }
                drop(c);
            }
            other => {
                let resp = err(StatusCode::BAD_REQUEST, format!("unknown resolution: {other}"));
                if_error = Some(resp);
            }
        }
    }
    if let Some(r) = if_error { return r; }

    // mark inbox item resolved
    let c = st.db.lock().unwrap(); // may have been dropped in branches; re-lock
    let _ = c.execute(
        "UPDATE inbox SET status='resolved', resolution=?2 WHERE id=?1",
        rusqlite::params![id, payload_out.to_string()],
    );
    let resolved = db::get_inbox(&c, &id).unwrap();
    let resolved_ev = if resolved.kind == "milestone_gate" { "milestone.gate_resolved" } else { "escalation.resolved" };
    events::push_event(&st, &c, &project_id, None, resolved_ev,
        json!({"inbox_id": id, "resolution": payload_out}));
    events::send_inbox(&st, &resolved);
    drop(c);
    st.wake();
    ok(resolved)
}

// ---------- events & stream ----------

async fn list_events(State(st): State<Arc<App>>, Query(q): Query<std::collections::HashMap<String, String>>) -> Response {
    let c = st.db.lock().unwrap();
    // 审计分页/筛选参数（order=desc 倒序；from/to 为毫秒时间戳；type 支持逗号分隔多值；
    // limit 缺省不限制——任务详情时间线等旧调用方不受影响）
    let order_desc = q.get("order").map(|s| s == "desc").unwrap_or(false);
    let type_filter: Vec<String> = q
        .get("type")
        .map(|s| s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
        .unwrap_or_default();
    let type_prefixes: Vec<String> = q
        .get("type_prefix")
        .map(|s| s.split(',').map(|t| t.trim().trim_end_matches('*').to_string()).filter(|t| !t.is_empty()).collect())
        .unwrap_or_default();
    let from = q.get("from").and_then(|s| s.parse::<i64>().ok());
    let to = q.get("to").and_then(|s| s.parse::<i64>().ok());
    let limit = q.get("limit").and_then(|s| s.parse::<i64>().ok()).map(|l| l.clamp(1, 500));
    let offset = q.get("offset").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0).max(0);
    match limit {
        Some(limit) => {
            let (evs, total) = db::list_events_paged(
                &c,
                q.get("project_id").map(|s| s.as_str()),
                &type_filter,
                &type_prefixes,
                from,
                to,
                order_desc,
                limit,
                offset,
            );
            ok(json!({"events": evs, "total": total, "limit": limit, "offset": offset}))
        }
        None => {
            // 不分页时同样尊重 type/type_prefix 过滤（审计契约：task.recovered 等可直接检索）
            let mut evs = db::list_events(
                &c,
                q.get("project_id").map(|s| s.as_str()),
                q.get("task_id").map(|s| s.as_str()),
            );
            if !type_filter.is_empty() {
                evs.retain(|e| type_filter.contains(&e.kind));
            }
            if !type_prefixes.is_empty() {
                evs.retain(|e| type_prefixes.iter().any(|p| e.kind.starts_with(p.as_str())));
            }
            if let Some(f) = from {
                evs.retain(|e| e.ts >= f);
            }
            if let Some(t) = to {
                evs.retain(|e| e.ts <= t);
            }
            ok(evs)
        }
    }
}

fn snapshot(st: &Arc<App>) -> Value {
    let c = st.db.lock().unwrap();
    json!({
        "engineerings": db::list_engineerings(&c),
        "projects": db::list_projects(&c),
        "agents": db::list_agents(&c),
        "inbox": db::list_inbox(&c),
    })
}

async fn stream(State(st): State<Arc<App>>) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    // subscribe BEFORE snapshot so nothing between the two is lost (duplicates are fine)
    let rx = st.tx.subscribe();
    let snap = snapshot(&st);
    let init = futures::stream::iter(vec![Ok(sse_of("snapshot", snap))]);
    let live = BroadcastStream::new(rx).filter_map(|m| async move { m.ok() })
        .map(|(kind, v)| Ok(sse_of(&kind, v)));
    Sse::new(init.chain(live)).keep_alive(KeepAlive::default())
}

fn sse_of(kind: &str, v: Value) -> SseEvent {
    SseEvent::default().event(kind.to_string()).data(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 复现真实故障形态：LLM 把说明文字与 JSON 混排，甚至夹带第二个伪 JSON。
    #[test]
    fn extract_json_survives_prose_mixed_output() {
        let raw = "好的，以下是规划：\n任务依赖与状态，新增任务/改依赖后刷新可见；3) 看板/详情显示状态 {状态: 待执行}；\n{\"tasks\":[{\"title\":\"T1\",\
            \"description\":\"d\",\"depends_on\":[],\"acceptance_required\":true,\"acceptance_criteria\":\"c\"}]}";
        let v = extract_json_value(raw).expect("should extract embedded JSON");
        assert_eq!(v["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(v["tasks"][0]["title"], "T1");
    }

    #[test]
    fn extract_json_handles_fenced_block() {
        let raw = "方案如下：\n```json\n{\"tasks\":[{\"title\":\" fenced \",\"depends_on\":[]}]}\n```\n以上。";
        let v = extract_json_value(raw).expect("fenced JSON");
        assert_eq!(v["tasks"][0]["title"], " fenced ");
    }

    #[test]
    fn extract_json_picks_longest_candidate() {
        let raw = "前置说明 {a: 1}。然后是真正答案 {\"tasks\":[{\"title\":\"x\",\"depends_on\":[]}],\
            \"analysis\":\"why\"}";
        let v = extract_json_value(raw).expect("longest");
        assert!(v.get("tasks").is_some());
        assert!(v.get("analysis").is_some());
    }

    #[test]
    fn extract_json_rejects_garbage() {
        assert!(extract_json_value("完全没有 JSON 的输出").is_none());
        assert!(extract_json_value("{只有括号而不完整").is_none());
    }

    #[test]
    fn parse_plan_rejects_non_array_tasks() {
        assert!(parse_plan_json("{\"tasks\":\"not array\"}").is_err());
    }

    #[test]
    fn parse_plan_reads_milestones_and_drops_invalid() {
        let raw = "{\"tasks\":[{\"title\":\"A\",\"depends_on\":[]},{\"title\":\"B\",\"depends_on\":[0]}],\
            \"milestones\":[{\"name\":\"阶段一\",\"goal\":\"g1\",\"tasks\":[0]},\
            {\"name\":\"阶段二\",\"tasks\":[1,9]},{\"name\":\"\",\"tasks\":[0]}]}";
        let (tasks, ms) = parse_plan_json(raw).unwrap();
        assert_eq!(tasks.len(), 2);
        // the empty-name milestone is dropped; out-of-range index 9 is filtered
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0].name, "阶段一");
        assert_eq!(ms[0].task_idx, vec![0]);
        assert_eq!(ms[1].name, "阶段二");
        assert_eq!(ms[1].task_idx, vec![1]);
    }

    #[test]
    fn parse_plan_reads_acceptance_checks_and_rejects_invalid() {
        let raw = "{\"tasks\":[{\"title\":\"A\",\"depends_on\":[],\"acceptance_checks\":[\
            {\"type\":\"file_exists\",\"path\":\"out.txt\"},\
            {\"type\":\"command\",\"cmd\":\"true\",\"expect_exit\":0}]}]}";
        let (tasks, _) = parse_plan_json(raw).unwrap();
        assert_eq!(tasks[0].checks.len(), 2);
        assert_eq!(tasks[0].checks[0]["type"], "file_exists");

        let bad = "{\"tasks\":[{\"title\":\"A\",\"acceptance_checks\":[{\"type\":\"http\",\"path\":\"/\"}]}]}";
        assert!(parse_plan_json(bad).is_err());

        let not_array = "{\"tasks\":[{\"title\":\"A\",\"acceptance_checks\":\"nope\"}]}";
        assert!(parse_plan_json(not_array).is_err());
    }

    fn inbox_item(kind: &str, payload: Value) -> InboxItem {
        InboxItem {
            id: "inb_test".into(), project_id: "prj".into(), kind: kind.into(),
            payload, status: "open".into(), resolution: None, created_at: 0,
        }
    }

    #[test]
    fn inbox_actions_are_backend_driven_per_type() {
        let gate = inbox_actions(&inbox_item("milestone_gate", serde_json::json!({"milestone_id": "m1"})));
        assert_eq!(gate.iter().map(|a| a["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["pass", "reject"]);

        let report = inbox_actions(&inbox_item("replan", serde_json::json!({"kind": "replan_report"})));
        assert_eq!(report.iter().map(|a| a["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["approve", "deny"]);

        let pending = inbox_actions(&inbox_item("replan", serde_json::json!({"kind": "replan_report", "planning": "pending"})));
        assert!(pending.is_empty());

        let window = inbox_actions(&inbox_item("replan", serde_json::json!({"kind": "milestone_window", "milestone_id": "m1"})));
        assert!(window.is_empty());

        let blocked = inbox_actions(&inbox_item("project_action_blocked", serde_json::json!({"reason": "pause_affected_other_projects"})));
        assert_eq!(blocked.iter().map(|a| a["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["retry_pause", "close"]);
    }

    #[test]
    fn parse_plan_groups_by_inline_milestone_name() {
        let raw = "{\"tasks\":[{\"title\":\"A\",\"depends_on\":[],\"milestone\":\"P1\"},\
            {\"title\":\"B\",\"depends_on\":[0],\"milestone\":\"P1\"},\
            {\"title\":\"C\",\"depends_on\":[0]}]}";
        let (tasks, ms) = parse_plan_json(raw).unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].name, "P1");
        assert_eq!(ms[0].task_idx, vec![0, 1]);
    }
}

