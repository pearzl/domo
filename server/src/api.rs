//! HTTP API and server-sent events.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::path::PathBuf;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::{BroadcastStream, WatchStream};
use tokio_stream::StreamExt;

use crate::agents::kill_job_dir;
use crate::domain::{self, deps_complete, now, short_id};
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::replan;
use crate::state::AppState;
use crate::usage;
use crate::version;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
    fn internal(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("{error:#}"),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self::internal(error)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        Self::internal(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/agents", get(list_agents).post(create_agent))
        .route("/api/agents/{id}", patch(update_agent).delete(delete_agent))
        .route("/api/agents/{id}/usage/reset", post(reset_agent_usage))
        .route(
            "/api/team/default",
            get(get_default_team).post(add_default_team_agent),
        )
        .route(
            "/api/team/default/{role}/{agent_id}",
            patch(update_default_team_agent).delete(delete_default_team_agent),
        )
        .route("/api/usage", get(usage_report))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/{id}", get(get_project))
        .route(
            "/api/projects/{id}/agents",
            get(get_project_agents).post(add_project_agent),
        )
        .route(
            "/api/projects/{id}/agents/{role}/{agent_id}",
            patch(update_project_agent),
        )
        .route(
            "/api/projects/{id}/analysis/messages",
            post(post_analysis_message),
        )
        .route(
            "/api/projects/{id}/analysis/confirm",
            post(confirm_analysis),
        )
        .route(
            "/api/projects/{id}/analysis/policy",
            patch(update_analysis_policy),
        )
        .route("/api/projects/{id}/pause", post(pause_project))
        .route("/api/projects/{id}/resume", post(resume_project))
        .route("/api/projects/{id}/archive", post(archive_project))
        .route("/api/projects/{id}/retry", post(retry_project))
        .route("/api/projects/{id}/replan", post(replan_project))
        .route("/api/projects/{id}/report", post(submit_report))
        .route("/api/projects/{id}/merge", post(merge_project))
        .route("/api/projects/{id}/tasks", post(create_task))
        .route("/api/projects/{id}/tasks/{task_id}/retry", post(retry_task))
        .route(
            "/api/projects/{id}/tasks/{task_id}/guidance",
            post(save_task_guidance),
        )
        .route(
            "/api/projects/{id}/tasks/{task_id}/agents",
            get(get_task_agent_pool)
                .post(add_task_agent)
                .patch(set_task_agents),
        )
        .route(
            "/api/projects/{id}/tasks/{task_id}/agents/{role}/{agent_id}",
            patch(update_task_agent).delete(delete_task_agent),
        )
        .route("/api/projects/{id}/tasks/{task_id}/pause", post(pause_task))
        .route(
            "/api/projects/{id}/tasks/{task_id}/resume",
            post(resume_task),
        )
        .route(
            "/api/projects/{id}/tasks/{task_id}/queue",
            patch(reorder_task_queue).post(reorder_task_queue),
        )
        .route(
            "/api/projects/{id}/tasks/{task_id}/milestone",
            post(set_task_milestone),
        )
        .route(
            "/api/projects/{id}/tasks/{task_id}/milestone/accept",
            post(accept_milestone),
        )
        .route(
            "/api/projects/{id}/tasks/{task_id}/milestone/reject",
            post(reject_milestone),
        )
        .route(
            "/api/projects/{id}/tasks/{task_id}/arbitration",
            get(task_arbitration).post(submit_arbitration),
        )
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/jobs/{id}/output", get(get_job_output))
        .route("/api/events", get(list_events))
        .route("/api/events/stream", get(stream_events))
        .route("/api/{*rest}", any(api_not_found))
        .with_state(state)
}

async fn api_not_found() -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: "接口不存在".into(),
        }),
    )
}

async fn health() -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": version::VERSION,
        "git_hash": version::git_hash(),
    }))
}

async fn list_agents(State(state): State<AppState>) -> Result<Json<Vec<AgentView>>, ApiError> {
    let rows =
        sqlx::query_as::<_, AgentRow>("SELECT * FROM agents ORDER BY created_at ASC, id ASC")
            .fetch_all(&state.db)
            .await?;
    let mut views = Vec::new();
    for row in rows {
        views.push(agent_view(&state, row).await?);
    }
    Ok(Json(views))
}

async fn agent_view(state: &AppState, row: AgentRow) -> Result<AgentView, ApiError> {
    let usage_window = usage::agent_usage(
        &state.db,
        &row.id,
        row.usage_limit_json.as_deref(),
        row.usage_reset_at.as_deref(),
        chrono::Utc::now(),
    )
    .await?;
    let mut view = AgentView::from_row(row);
    view.usage_window = usage_window;
    Ok(view)
}

async fn create_agent(
    State(state): State<AppState>,
    Json(request): Json<CreateAgentRequest>,
) -> Result<Json<AgentView>, ApiError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad("Agent 名称不能为空"));
    }
    let kind = validate_kind(&request.kind)?;
    let roles = validate_roles(request.roles.unwrap_or_else(|| {
        vec![
            ROLE_PLANNER.to_string(),
            ROLE_EXECUTOR.to_string(),
            ROLE_VERIFIER.to_string(),
        ]
    }))?;
    let command = request.command.unwrap_or_default();
    if kind == AGENT_CLI && command.is_empty() {
        return Err(ApiError::bad("CLI Agent 必须配置命令模板"));
    }
    let availability = match request.availability {
        Some(config) => {
            usage::validate_availability(&config).map_err(ApiError::bad)?;
            Some(serde_json::to_string(&config).map_err(|error| ApiError::internal(error.into()))?)
        }
        None => None,
    };
    let usage_limit = match request.usage_limit {
        Some(config) => {
            usage::validate_usage_limit(&config).map_err(ApiError::bad)?;
            Some(serde_json::to_string(&config).map_err(|error| ApiError::internal(error.into()))?)
        }
        None => None,
    };
    let id = domain::new_id("agt");
    let timestamp = now();
    sqlx::query(
        "INSERT INTO agents (id, name, kind, model, roles, priority, max_concurrency, enabled, \
         command_json, model_args_json, availability_json, usage_limit_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(&kind)
    .bind(request.model.unwrap_or_default().trim())
    .bind(serde_json::to_string(&roles).unwrap_or_else(|_| "[]".into()))
    .bind(request.max_concurrency.unwrap_or(1).max(1))
    .bind(if request.enabled.unwrap_or(true) {
        1
    } else {
        0
    })
    .bind(serde_json::to_string(&command).unwrap_or_else(|_| "[]".into()))
    .bind(
        serde_json::to_string(&request.model_args.unwrap_or_default())
            .unwrap_or_else(|_| "[]".into()),
    )
    .bind(availability)
    .bind(usage_limit)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&state.db)
    .await?;
    let row = crate::db::load_agent(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Agent 不存在"))?;
    Ok(Json(agent_view(&state, row).await?))
}

async fn update_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateAgentRequest>,
) -> Result<Json<AgentView>, ApiError> {
    let Some(existing) = crate::db::load_agent(&state.db, &id).await? else {
        return Err(ApiError::not_found("Agent 不存在"));
    };
    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&existing.name)
        .to_string();
    let kind = match request.kind {
        Some(value) => validate_kind(&value)?,
        None => existing.kind.clone(),
    };
    let roles = match request.roles {
        Some(value) => {
            serde_json::to_string(&validate_roles(value)?).unwrap_or_else(|_| "[]".into())
        }
        None => existing.roles.clone(),
    };
    let command = request
        .command
        .map(|value| serde_json::to_string(&value).unwrap_or_else(|_| "[]".into()))
        .unwrap_or_else(|| existing.command_json.clone().unwrap_or_else(|| "[]".into()));
    if kind == AGENT_CLI && command == "[]" {
        return Err(ApiError::bad("CLI Agent 必须配置命令模板"));
    }
    let model_args = request
        .model_args
        .map(|value| serde_json::to_string(&value).unwrap_or_else(|_| "[]".into()))
        .unwrap_or_else(|| {
            existing
                .model_args_json
                .clone()
                .unwrap_or_else(|| "[]".into())
        });
    let model = request.model.unwrap_or(existing.model.clone());
    let max_concurrency = request
        .max_concurrency
        .unwrap_or(existing.max_concurrency)
        .max(1);
    let enabled = request
        .enabled
        .map(|value| if value { 1 } else { 0 })
        .unwrap_or(existing.enabled);
    let availability_json = match request.availability {
        None => existing.availability_json.clone(),
        Some(None) => None,
        Some(Some(config)) => {
            usage::validate_availability(&config).map_err(ApiError::bad)?;
            Some(serde_json::to_string(&config).map_err(|error| ApiError::internal(error.into()))?)
        }
    };
    let usage_limit_json = match request.usage_limit {
        None => existing.usage_limit_json.clone(),
        Some(None) => None,
        Some(Some(config)) => {
            usage::validate_usage_limit(&config).map_err(ApiError::bad)?;
            Some(serde_json::to_string(&config).map_err(|error| ApiError::internal(error.into()))?)
        }
    };
    sqlx::query(
        "UPDATE agents SET name = ?, kind = ?, model = ?, roles = ?, \
         max_concurrency = ?, enabled = ?, command_json = ?, model_args_json = ?, \
         availability_json = ?, usage_limit_json = ?, \
         cooldown_until = NULL, last_error = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(&name)
    .bind(&kind)
    .bind(model.trim())
    .bind(&roles)
    .bind(max_concurrency)
    .bind(enabled)
    .bind(&command)
    .bind(&model_args)
    .bind(&availability_json)
    .bind(&usage_limit_json)
    .bind(now())
    .bind(&id)
    .execute(&state.db)
    .await?;
    state.notify_scheduler();
    let row = crate::db::load_agent(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Agent 不存在"))?;
    Ok(Json(agent_view(&state, row).await?))
}

async fn get_default_team(
    State(state): State<AppState>,
) -> Result<Json<ProjectAgentPool>, ApiError> {
    Ok(Json(crate::db::load_default_team_pool(&state.db).await?))
}

async fn add_default_team_agent(
    State(state): State<AppState>,
    Json(request): Json<AddProjectAgentRequest>,
) -> Result<Json<ProjectAgentPool>, ApiError> {
    let role = validate_role(request.role.trim())?;
    let Some(agent) = crate::db::load_agent(&state.db, request.agent_id.trim()).await? else {
        return Err(ApiError::not_found("Agent 不存在"));
    };
    if !parse_list(&agent.roles).iter().any(|item| item == &role) {
        return Err(ApiError::bad(format!(
            "Agent {} 不能承担{}角色",
            agent.name,
            role_label(&role)
        )));
    }
    let exists: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM default_team WHERE role = ? AND agent_id = ?")
            .bind(&role)
            .bind(&agent.id)
            .fetch_one(&state.db)
            .await?;
    if exists > 0 {
        return Err(ApiError::bad("Agent 已在默认团队中"));
    }
    let priority: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(priority), -1) + 1 FROM default_team WHERE role = ?",
    )
    .bind(&role)
    .fetch_one(&state.db)
    .await?;
    let timestamp = now();
    sqlx::query(
        "INSERT INTO default_team \
         (role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&role)
    .bind(&agent.id)
    .bind(&agent.name)
    .bind(priority)
    .bind(agent.enabled)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        None,
        None,
        "default_team_updated",
        format!("默认团队加入{} Agent {}", role_label(&role), agent.name),
    )
    .await;
    state.notify_scheduler();
    Ok(Json(crate::db::load_default_team_pool(&state.db).await?))
}

async fn update_default_team_agent(
    State(state): State<AppState>,
    Path((role, agent_id)): Path<(String, String)>,
    Json(request): Json<ProjectAgentUpdateRequest>,
) -> Result<Json<ProjectAgentPool>, ApiError> {
    let role = validate_role(role.trim())?;
    let lookup = sqlx::query_as::<_, DefaultTeamRow>(
        "SELECT * FROM default_team WHERE role = ? AND agent_id = ?",
    )
    .bind(&role)
    .bind(&agent_id)
    .fetch_optional(&state.db)
    .await?;
    let current = match lookup {
        Some(row) => row,
        // position 调整要求条目必须在该角色下，此时按参数错误处理。
        None if request.position.is_some() => return Err(ApiError::bad("默认团队中没有该 Agent")),
        None => return Err(ApiError::not_found("默认团队中没有该 Agent")),
    };
    if let Some(position) = request.position {
        move_default_team_agent_to(&state, &role, &current.agent_id, position).await?;
    }
    if let Some(direction) = request.direction.as_deref().map(str::trim) {
        if !matches!(direction, "up" | "down") {
            return Err(ApiError::bad("direction 必须是 up 或 down"));
        }
        let target = if direction == "up" {
            sqlx::query_as::<_, DefaultTeamRow>(
                "SELECT * FROM default_team WHERE role = ? \
                 AND priority < ? ORDER BY priority DESC LIMIT 1",
            )
            .bind(&role)
            .bind(current.priority)
            .fetch_optional(&state.db)
            .await?
        } else {
            sqlx::query_as::<_, DefaultTeamRow>(
                "SELECT * FROM default_team WHERE role = ? \
                 AND priority > ? ORDER BY priority ASC LIMIT 1",
            )
            .bind(&role)
            .bind(current.priority)
            .fetch_optional(&state.db)
            .await?
        };
        if let Some(target) = target {
            let timestamp = now();
            let mut tx = state.db.begin().await?;
            sqlx::query(
                "UPDATE default_team SET priority = ?, updated_at = ? \
                 WHERE role = ? AND agent_id = ?",
            )
            .bind(target.priority)
            .bind(&timestamp)
            .bind(&role)
            .bind(&current.agent_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE default_team SET priority = ?, updated_at = ? \
                 WHERE role = ? AND agent_id = ?",
            )
            .bind(current.priority)
            .bind(&timestamp)
            .bind(&role)
            .bind(&target.agent_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
    }
    if let Some(enabled) = request.enabled {
        sqlx::query(
            "UPDATE default_team SET enabled = ?, updated_at = ? \
             WHERE role = ? AND agent_id = ?",
        )
        .bind(if enabled { 1 } else { 0 })
        .bind(now())
        .bind(&role)
        .bind(&current.agent_id)
        .execute(&state.db)
        .await?;
    }
    emit(
        &state.db,
        &state.bus,
        None,
        None,
        "default_team_updated",
        format!(
            "默认团队更新{}角色配置（{}）",
            role_label(&role),
            current.agent_id
        ),
    )
    .await;
    state.notify_scheduler();
    Ok(Json(crate::db::load_default_team_pool(&state.db).await?))
}

async fn delete_default_team_agent(
    State(state): State<AppState>,
    Path((role, agent_id)): Path<(String, String)>,
) -> Result<Json<ProjectAgentPool>, ApiError> {
    let role = validate_role(role.trim())?;
    let result = sqlx::query("DELETE FROM default_team WHERE role = ? AND agent_id = ?")
        .bind(&role)
        .bind(&agent_id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("默认团队中没有该 Agent"));
    }
    renumber_default_team_role(&state, &role).await?;
    emit(
        &state.db,
        &state.bus,
        None,
        None,
        "default_team_updated",
        format!("默认团队移除{} Agent {}", role_label(&role), agent_id),
    )
    .await;
    state.notify_scheduler();
    Ok(Json(crate::db::load_default_team_pool(&state.db).await?))
}

/// 移除条目后把该角色的优先级重排为 0..n-1，保持编号连续。
async fn renumber_default_team_role(state: &AppState, role: &str) -> Result<(), ApiError> {
    let members: Vec<(String,)> = sqlx::query_as(
        "SELECT agent_id FROM default_team WHERE role = ? ORDER BY priority ASC, agent_id ASC",
    )
    .bind(role)
    .fetch_all(&state.db)
    .await?;
    let timestamp = now();
    let mut tx = state.db.begin().await?;
    for (priority, (agent_id,)) in members.iter().enumerate() {
        sqlx::query(
            "UPDATE default_team SET priority = ?, updated_at = ? WHERE role = ? AND agent_id = ?",
        )
        .bind(priority as i64)
        .bind(&timestamp)
        .bind(role)
        .bind(agent_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 把角色内指定 Agent 移动到 0 基目标位置，并在事务内把该角色优先级重排为 0..n-1。
/// 其余角色的条目不参与本次重排。
async fn move_default_team_agent_to(
    state: &AppState,
    role: &str,
    agent_id: &str,
    position: i64,
) -> Result<(), ApiError> {
    let members: Vec<(String,)> = sqlx::query_as(
        "SELECT agent_id FROM default_team WHERE role = ? ORDER BY priority ASC, agent_id ASC",
    )
    .bind(role)
    .fetch_all(&state.db)
    .await?;
    let reordered = plan_role_reorder(members, "默认团队", agent_id, position)?;
    let timestamp = now();
    let mut tx = state.db.begin().await?;
    for (priority, id) in reordered.iter().enumerate() {
        sqlx::query(
            "UPDATE default_team SET priority = ?, updated_at = ? WHERE role = ? AND agent_id = ?",
        )
        .bind(priority as i64)
        .bind(&timestamp)
        .bind(role)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 通用落位计划：校验 position 越界与 Agent 归属，返回移动后的 agent 顺序。
/// `members` 必须已经按目标角色当前顺序排列。
fn plan_role_reorder(
    members: Vec<(String,)>,
    scope: &str,
    agent_id: &str,
    position: i64,
) -> Result<Vec<String>, ApiError> {
    if position < 0 || position as usize >= members.len() {
        return Err(ApiError::bad(format!(
            "{scope}该角色的 position 超出条目范围"
        )));
    }
    let Some(index) = members.iter().position(|(id,)| id == agent_id) else {
        return Err(ApiError::bad(format!("{scope}该角色下没有该 Agent")));
    };
    let mut reordered: Vec<String> = members.into_iter().map(|(id,)| id).collect();
    let moved = reordered.remove(index);
    reordered.insert(position as usize, moved);
    Ok(reordered)
}

async fn reset_agent_usage(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<AgentView>, ApiError> {
    let Some(row) = crate::db::load_agent(&state.db, &id).await? else {
        return Err(ApiError::not_found("Agent 不存在"));
    };
    sqlx::query("UPDATE agents SET usage_reset_at = ?, updated_at = ? WHERE id = ?")
        .bind(now())
        .bind(now())
        .bind(&id)
        .execute(&state.db)
        .await?;
    emit(
        &state.db,
        &state.bus,
        None,
        None,
        "agent_usage_reset",
        format!("Agent {} 的用量窗口已重置", row.name),
    )
    .await;
    state.notify_scheduler();
    let row = crate::db::load_agent(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Agent 不存在"))?;
    Ok(Json(agent_view(&state, row).await?))
}

async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let affected_roles: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT role FROM default_team WHERE agent_id = ?")
            .bind(&id)
            .fetch_all(&state.db)
            .await?;
    sqlx::query("DELETE FROM agents WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await?;
    sqlx::query("DELETE FROM default_team WHERE agent_id = ?")
        .bind(&id)
        .execute(&state.db)
        .await?;
    for role in affected_roles {
        renumber_default_team_role(&state, &role).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProjectSummary>>, ApiError> {
    let rows = sqlx::query_as::<_, ProjectRow>(
        "SELECT * FROM projects ORDER BY updated_at DESC, created_at DESC",
    )
    .fetch_all(&state.db)
    .await?;
    let mut projects = Vec::new();
    for row in rows {
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND COALESCE(status, '') != ?",
        )
        .bind(&row.id)
        .bind(STATUS_CANCELED)
        .fetch_one(&state.db)
        .await?;
        let completed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage = 'completed'",
        )
        .bind(&row.id)
        .fetch_one(&state.db)
        .await?;
        projects.push(ProjectSummary {
            id: row.id,
            name: row.name,
            goal: row.goal,
            repo_path: row.repo_path,
            base_branch: row.base_branch,
            branch: row.branch,
            stage: row.stage,
            status: row.status,
            blocked_reason: row.blocked_reason,
            task_total: total,
            task_completed: completed,
            replan_reason: row.replan_reason,
            replan_source: row.replan_source,
            confirm_mode: row.confirm_mode,
            completion_mode: row.completion_mode,
            merged_at: row.merged_at,
            merged_commit: row.merged_commit,
            created_at: row.created_at,
            updated_at: row.updated_at,
        });
    }
    Ok(Json(projects))
}

async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad("项目名称不能为空"));
    }
    let raw_repo = request.repo_path.trim();
    let repo_input = if let Some(rest) = raw_repo.strip_prefix("~/") {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest)
    } else {
        PathBuf::from(raw_repo)
    };
    if !repo_input.is_dir() {
        return Err(ApiError::bad("仓库路径不存在"));
    }
    let repo = git::repo_root(&repo_input)
        .await
        .map_err(|error| ApiError::bad(format!("{error:#}")))?;
    let base = match request.base_branch.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => git::current_branch(&repo)
            .await
            .map_err(|error| ApiError::bad(format!("{error:#}")))?,
    };
    git::validate_base(&repo, &base)
        .await
        .map_err(|error| ApiError::bad(format!("base 分支不可用: {error:#}")))?;
    let project_id = domain::new_id("prj");
    let branch = format!("domo/p-{}", short_id(&project_id));
    if git::has_branch(&repo, &branch).await {
        return Err(ApiError::bad(format!("分支 {branch} 已存在")));
    }
    let workspace = state.cfg.workspace_root.join(&project_id);
    let repo_worktree = workspace.join("repo");
    {
        let _guard = state.git_lock.lock().await;
        git::create_project_worktree(&repo, &base, &repo_worktree, &branch)
            .await
            .map_err(|error| ApiError::bad(format!("创建项目 worktree 失败: {error:#}")))?;
    }
    let timestamp = now();
    let result = sqlx::query(
        "INSERT INTO projects (id, name, goal, repo_path, base_branch, branch, workspace, \
         stage, status, blocked_reason, confirm_mode, completion_mode, created_at, updated_at) \
         VALUES (?, ?, '', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&project_id)
    .bind(name)
    .bind(repo.to_string_lossy().to_string())
    .bind(&base)
    .bind(&branch)
    .bind(workspace.to_string_lossy().to_string())
    .bind(STAGE_ANALYSIS)
    .bind(STATUS_BLOCKED)
    .bind("等待输入项目目标")
    .bind(CONFIRM_MANUAL)
    .bind(COMPLETION_TASKS)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&state.db)
    .await;
    if let Err(error) = result {
        let _ = git::remove_worktree(&repo, &repo_worktree).await;
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        return Err(error.into());
    }
    crate::db::ensure_project_agents(&state.db, &project_id).await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        None,
        "project_created",
        format!("项目创建，等待输入目标（base 分支 {base}）"),
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectDetail>, ApiError> {
    load_detail(&state, &id).await.map(Json)
}

async fn get_project_agents(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectAgentPool>, ApiError> {
    require_project(&state, &id).await?;
    let pool = crate::db::load_project_agent_pool(&state.db, &id).await?;
    Ok(Json(pool))
}

async fn add_project_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<AddProjectAgentRequest>,
) -> Result<Json<ProjectAgentPool>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    let role = validate_role(request.role.trim())?;
    let Some(agent) = crate::db::load_agent(&state.db, request.agent_id.trim()).await? else {
        return Err(ApiError::not_found("Agent 不存在"));
    };
    if !parse_list(&agent.roles).iter().any(|item| item == &role) {
        return Err(ApiError::bad(format!(
            "Agent {} 不能承担{}角色",
            agent.name,
            role_label(&role)
        )));
    }
    crate::db::ensure_project_agents(&state.db, &id).await?;
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_agents WHERE project_id = ? AND role = ? AND agent_id = ?",
    )
    .bind(&id)
    .bind(&role)
    .bind(&agent.id)
    .fetch_one(&state.db)
    .await?;
    if exists > 0 {
        return Err(ApiError::bad("Agent 已在项目池中"));
    }
    let priority: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(priority), -1) + 1 FROM project_agents WHERE project_id = ? AND role = ?",
    )
    .bind(&id)
    .bind(&role)
    .fetch_one(&state.db)
    .await?;
    let timestamp = now();
    sqlx::query(
        "INSERT INTO project_agents \
         (project_id, role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&role)
    .bind(&agent.id)
    .bind(&agent.name)
    .bind(priority)
    .bind(agent.enabled)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        "project_agents_updated",
        format!(
            "项目 Agent 池加入{} Agent {}",
            role_label(&role),
            agent.name
        ),
    )
    .await;
    state.notify_scheduler();
    let pool = crate::db::load_project_agent_pool(&state.db, &id).await?;
    Ok(Json(pool))
}

async fn update_project_agent(
    State(state): State<AppState>,
    Path((project_id, role, agent_id)): Path<(String, String, String)>,
    Json(request): Json<ProjectAgentUpdateRequest>,
) -> Result<Json<ProjectAgentPool>, ApiError> {
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    let role = validate_role(role.trim())?;
    crate::db::ensure_project_agents(&state.db, &project_id).await?;
    let lookup = sqlx::query_as::<_, ProjectAgentRow>(
        "SELECT * FROM project_agents WHERE project_id = ? AND role = ? AND agent_id = ?",
    )
    .bind(&project_id)
    .bind(&role)
    .bind(&agent_id)
    .fetch_optional(&state.db)
    .await?;
    let current = match lookup {
        Some(row) => row,
        // position 调整要求条目必须在该角色下，此时按参数错误处理。
        None if request.position.is_some() => {
            return Err(ApiError::bad("项目 Agent 池中没有该 Agent"))
        }
        None => return Err(ApiError::not_found("项目 Agent 池中没有该 Agent")),
    };
    if let Some(position) = request.position {
        move_project_agent_to(&state, &project_id, &role, &current.agent_id, position).await?;
    }
    if let Some(direction) = request.direction.as_deref().map(str::trim) {
        if !matches!(direction, "up" | "down") {
            return Err(ApiError::bad("direction 必须是 up 或 down"));
        }
        let target = if direction == "up" {
            sqlx::query_as::<_, ProjectAgentRow>(
                "SELECT * FROM project_agents WHERE project_id = ? AND role = ? \
                 AND priority < ? ORDER BY priority DESC LIMIT 1",
            )
            .bind(&project_id)
            .bind(&role)
            .bind(current.priority)
            .fetch_optional(&state.db)
            .await?
        } else {
            sqlx::query_as::<_, ProjectAgentRow>(
                "SELECT * FROM project_agents WHERE project_id = ? AND role = ? \
                 AND priority > ? ORDER BY priority ASC LIMIT 1",
            )
            .bind(&project_id)
            .bind(&role)
            .bind(current.priority)
            .fetch_optional(&state.db)
            .await?
        };
        if let Some(target) = target {
            let timestamp = now();
            let mut tx = state.db.begin().await?;
            sqlx::query(
                "UPDATE project_agents SET priority = ?, updated_at = ? \
                 WHERE project_id = ? AND role = ? AND agent_id = ?",
            )
            .bind(target.priority)
            .bind(&timestamp)
            .bind(&project_id)
            .bind(&role)
            .bind(&current.agent_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE project_agents SET priority = ?, updated_at = ? \
                 WHERE project_id = ? AND role = ? AND agent_id = ?",
            )
            .bind(current.priority)
            .bind(&timestamp)
            .bind(&project_id)
            .bind(&role)
            .bind(&target.agent_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
    }
    if let Some(enabled) = request.enabled {
        sqlx::query(
            "UPDATE project_agents SET enabled = ?, updated_at = ? \
             WHERE project_id = ? AND role = ? AND agent_id = ?",
        )
        .bind(if enabled { 1 } else { 0 })
        .bind(now())
        .bind(&project_id)
        .bind(&role)
        .bind(&current.agent_id)
        .execute(&state.db)
        .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        None,
        "project_agents_updated",
        format!(
            "项目 Agent 池更新{}角色配置（{}）",
            role_label(&role),
            current.agent_id
        ),
    )
    .await;
    state.notify_scheduler();
    let pool = crate::db::load_project_agent_pool(&state.db, &project_id).await?;
    Ok(Json(pool))
}

/// 把项目池角色内指定 Agent 移动到 0 基目标位置，并在事务内把该角色优先级重排为 0..n-1。
/// 其余角色的条目不参与本次重排。
async fn move_project_agent_to(
    state: &AppState,
    project_id: &str,
    role: &str,
    agent_id: &str,
    position: i64,
) -> Result<(), ApiError> {
    let members: Vec<(String,)> = sqlx::query_as(
        "SELECT agent_id FROM project_agents WHERE project_id = ? AND role = ? \
         ORDER BY priority ASC, agent_id ASC",
    )
    .bind(project_id)
    .bind(role)
    .fetch_all(&state.db)
    .await?;
    let reordered = plan_role_reorder(members, "项目 Agent 池", agent_id, position)?;
    let timestamp = now();
    let mut tx = state.db.begin().await?;
    for (priority, id) in reordered.iter().enumerate() {
        sqlx::query(
            "UPDATE project_agents SET priority = ?, updated_at = ? \
             WHERE project_id = ? AND role = ? AND agent_id = ?",
        )
        .bind(priority as i64)
        .bind(&timestamp)
        .bind(project_id)
        .bind(role)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 任务池只允许执行者与验收者两个角色；规划者始终使用项目池。
fn validate_task_pool_role(role: &str) -> Result<String, ApiError> {
    match role {
        ROLE_EXECUTOR | ROLE_VERIFIER => Ok(role.to_string()),
        ROLE_PLANNER => Err(ApiError::bad("任务 Agent 池只有执行者与验收者两个角色")),
        other => Err(ApiError::bad(format!("未知角色: {other}"))),
    }
}

fn ensure_task_pool_mutable(task: &TaskRow) -> Result<(), ApiError> {
    if task.stage == TASK_STAGE_COMPLETED {
        return Err(ApiError::bad("任务已完成，任务 Agent 池只读"));
    }
    if task.status.as_deref() == Some(STATUS_CANCELED) {
        return Err(ApiError::bad("任务已取消，任务 Agent 池只读"));
    }
    Ok(())
}

async fn get_task_agent_pool(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> Result<Json<TaskAgentPool>, ApiError> {
    let task = load_project_task(&state, &project_id, &task_id).await?;
    let pool = crate::db::load_task_agent_pool(&state.db, &project_id, &task.id).await?;
    Ok(Json(pool))
}

async fn add_task_agent(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<AddProjectAgentRequest>,
) -> Result<Json<TaskAgentPool>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档，任务 Agent 池只读"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    ensure_task_pool_mutable(&task)?;
    let role = validate_task_pool_role(request.role.trim())?;
    let Some(agent) = crate::db::load_agent(&state.db, request.agent_id.trim()).await? else {
        return Err(ApiError::not_found("Agent 不存在"));
    };
    if !parse_list(&agent.roles).iter().any(|item| item == &role) {
        return Err(ApiError::bad(format!(
            "Agent {} 不能承担{}角色",
            agent.name,
            role_label(&role)
        )));
    }
    crate::db::ensure_task_agents(&state.db, &project_id, &task.id).await?;
    // 加入候选只允许项目池中具备该角色且尚未在任务池中的 Agent。
    let in_project: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_agents WHERE project_id = ? AND role = ? AND agent_id = ?",
    )
    .bind(&project_id)
    .bind(&role)
    .bind(&agent.id)
    .fetch_one(&state.db)
    .await?;
    if in_project == 0 {
        return Err(ApiError::bad("Agent 不在项目 Agent 池的该角色中"));
    }
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM task_agents WHERE task_id = ? AND role = ? AND agent_id = ?",
    )
    .bind(&task.id)
    .bind(&role)
    .bind(&agent.id)
    .fetch_one(&state.db)
    .await?;
    if exists > 0 {
        return Err(ApiError::bad("Agent 已在任务池中"));
    }
    let priority: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(priority), -1) + 1 FROM task_agents WHERE task_id = ? AND role = ?",
    )
    .bind(&task.id)
    .bind(&role)
    .fetch_one(&state.db)
    .await?;
    let timestamp = now();
    sqlx::query(
        "INSERT INTO task_agents \
         (task_id, role, agent_id, agent_name, priority, enabled, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&task.id)
    .bind(&role)
    .bind(&agent.id)
    .bind(&agent.name)
    .bind(priority)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "task_agents_updated",
        format!(
            "任务 {} 加入{} Agent {}",
            task.key,
            role_label(&role),
            agent.name
        ),
    )
    .await;
    state.notify_scheduler();
    let pool = crate::db::load_task_agent_pool(&state.db, &project_id, &task.id).await?;
    Ok(Json(pool))
}

async fn update_task_agent(
    State(state): State<AppState>,
    Path((project_id, task_id, role, agent_id)): Path<(String, String, String, String)>,
    Json(request): Json<ProjectAgentUpdateRequest>,
) -> Result<Json<TaskAgentPool>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档，任务 Agent 池只读"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    ensure_task_pool_mutable(&task)?;
    let role = validate_task_pool_role(role.trim())?;
    crate::db::ensure_task_agents(&state.db, &project_id, &task.id).await?;
    let current = sqlx::query_as::<_, TaskAgentRow>(
        "SELECT * FROM task_agents WHERE task_id = ? AND role = ? AND agent_id = ?",
    )
    .bind(&task.id)
    .bind(&role)
    .bind(&agent_id)
    .fetch_optional(&state.db)
    .await?;
    let current = match current {
        Some(row) => row,
        // position 调整要求条目必须在该角色下，此时按参数错误处理。
        None if request.position.is_some() => {
            return Err(ApiError::bad("任务 Agent 池中没有该 Agent"))
        }
        None => return Err(ApiError::not_found("任务 Agent 池中没有该 Agent")),
    };
    if let Some(position) = request.position {
        move_task_agent_to(&state, &task.id, &role, &current.agent_id, position).await?;
    }
    if let Some(direction) = request.direction.as_deref().map(str::trim) {
        if !matches!(direction, "up" | "down") {
            return Err(ApiError::bad("direction 必须是 up 或 down"));
        }
        let target = if direction == "up" {
            sqlx::query_as::<_, TaskAgentRow>(
                "SELECT * FROM task_agents WHERE task_id = ? AND role = ? \
                 AND priority < ? ORDER BY priority DESC LIMIT 1",
            )
            .bind(&task.id)
            .bind(&role)
            .bind(current.priority)
            .fetch_optional(&state.db)
            .await?
        } else {
            sqlx::query_as::<_, TaskAgentRow>(
                "SELECT * FROM task_agents WHERE task_id = ? AND role = ? \
                 AND priority > ? ORDER BY priority ASC LIMIT 1",
            )
            .bind(&task.id)
            .bind(&role)
            .bind(current.priority)
            .fetch_optional(&state.db)
            .await?
        };
        if let Some(target) = target {
            let timestamp = now();
            let mut tx = state.db.begin().await?;
            sqlx::query(
                "UPDATE task_agents SET priority = ?, updated_at = ? \
                 WHERE task_id = ? AND role = ? AND agent_id = ?",
            )
            .bind(target.priority)
            .bind(&timestamp)
            .bind(&task.id)
            .bind(&role)
            .bind(&current.agent_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE task_agents SET priority = ?, updated_at = ? \
                 WHERE task_id = ? AND role = ? AND agent_id = ?",
            )
            .bind(current.priority)
            .bind(&timestamp)
            .bind(&task.id)
            .bind(&role)
            .bind(&target.agent_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
    }
    if let Some(enabled) = request.enabled {
        sqlx::query(
            "UPDATE task_agents SET enabled = ?, updated_at = ? \
             WHERE task_id = ? AND role = ? AND agent_id = ?",
        )
        .bind(if enabled { 1 } else { 0 })
        .bind(now())
        .bind(&task.id)
        .bind(&role)
        .bind(&current.agent_id)
        .execute(&state.db)
        .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "task_agents_updated",
        format!(
            "任务 {} 更新{}角色配置（{}）",
            task.key,
            role_label(&role),
            current.agent_id
        ),
    )
    .await;
    state.notify_scheduler();
    let pool = crate::db::load_task_agent_pool(&state.db, &project_id, &task.id).await?;
    Ok(Json(pool))
}

async fn delete_task_agent(
    State(state): State<AppState>,
    Path((project_id, task_id, role, agent_id)): Path<(String, String, String, String)>,
) -> Result<Json<TaskAgentPool>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档，任务 Agent 池只读"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    ensure_task_pool_mutable(&task)?;
    let role = validate_task_pool_role(role.trim())?;
    crate::db::ensure_task_agents(&state.db, &project_id, &task.id).await?;
    let result =
        sqlx::query("DELETE FROM task_agents WHERE task_id = ? AND role = ? AND agent_id = ?")
            .bind(&task.id)
            .bind(&role)
            .bind(&agent_id)
            .execute(&state.db)
            .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("任务 Agent 池中没有该 Agent"));
    }
    renumber_task_agent_role(&state, &task.id, &role).await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "task_agents_updated",
        format!(
            "任务 {} 移除{} Agent {}",
            task.key,
            role_label(&role),
            agent_id
        ),
    )
    .await;
    state.notify_scheduler();
    let pool = crate::db::load_task_agent_pool(&state.db, &project_id, &task.id).await?;
    Ok(Json(pool))
}

/// 把任务池角色内指定 Agent 移动到 0 基目标位置，并在事务内把该角色优先级
/// 重排为 0..n-1，其他角色不受影响。
async fn move_task_agent_to(
    state: &AppState,
    task_id: &str,
    role: &str,
    agent_id: &str,
    position: i64,
) -> Result<(), ApiError> {
    let members: Vec<(String,)> = sqlx::query_as(
        "SELECT agent_id FROM task_agents WHERE task_id = ? AND role = ? \
         ORDER BY priority ASC, agent_id ASC",
    )
    .bind(task_id)
    .bind(role)
    .fetch_all(&state.db)
    .await?;
    let reordered = plan_role_reorder(members, "任务 Agent 池", agent_id, position)?;
    let timestamp = now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    for (priority, id) in reordered.iter().enumerate() {
        sqlx::query(
            "UPDATE task_agents SET priority = ?, updated_at = ? \
             WHERE task_id = ? AND role = ? AND agent_id = ?",
        )
        .bind(priority as i64)
        .bind(&timestamp)
        .bind(task_id)
        .bind(role)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 删除任务池条目后把该角色的优先级重排为 0..n-1，保持编号连续。
async fn renumber_task_agent_role(
    state: &AppState,
    task_id: &str,
    role: &str,
) -> Result<(), ApiError> {
    let members: Vec<(String,)> = sqlx::query_as(
        "SELECT agent_id FROM task_agents WHERE task_id = ? AND role = ? \
         ORDER BY priority ASC, agent_id ASC",
    )
    .bind(task_id)
    .bind(role)
    .fetch_all(&state.db)
    .await?;
    let timestamp = now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    for (priority, (agent_id,)) in members.iter().enumerate() {
        sqlx::query(
            "UPDATE task_agents SET priority = ?, updated_at = ? \
             WHERE task_id = ? AND role = ? AND agent_id = ?",
        )
        .bind(priority as i64)
        .bind(&timestamp)
        .bind(task_id)
        .bind(role)
        .bind(agent_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 旧的任务 Agent 覆盖接口，作为兼容入口保留：指定 Agent 映射为“只含该
/// Agent 的单 Agent 池”，清除覆盖映射为“重置为项目池快照”。旧字段只是
/// 只读兼容展示，调度只看任务池。
async fn set_task_agents(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<TaskAgentsRequest>,
) -> Result<Json<TaskView>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    // 兼容入口同样先完成两角色的懒初始化，避免只写入一个角色。
    crate::db::ensure_task_agents(&state.db, &project_id, &task.id).await?;
    let mut changes: Vec<(&str, &str)> = Vec::new();
    if let Some(value) = &request.executor_agent_id {
        changes.push((ROLE_EXECUTOR, value.as_deref().unwrap_or("")));
    }
    if let Some(value) = &request.verifier_agent_id {
        changes.push((ROLE_VERIFIER, value.as_deref().unwrap_or("")));
    }
    for (role, raw) in &changes {
        let mut snapshot = String::new();
        if !raw.is_empty() {
            snapshot = resolve_task_agent_from_single(&state, raw, role).await?;
        }
        if snapshot.is_empty() {
            crate::db::reset_task_agent_role(&state.db, &project_id, &task.id, role).await?;
        } else {
            crate::db::set_single_task_agent(&state.db, &task.id, role, &snapshot).await?;
        }
    }
    if !changes.is_empty() {
        let executor = if changes.iter().any(|(role, _)| role == &ROLE_EXECUTOR) {
            request
                .executor_agent_id
                .as_ref()
                .and_then(|value| value.as_ref())
                .cloned()
        } else {
            task.executor_agent_id.clone()
        };
        let verifier = if changes.iter().any(|(role, _)| role == &ROLE_VERIFIER) {
            request
                .verifier_agent_id
                .as_ref()
                .and_then(|value| value.as_ref())
                .cloned()
        } else {
            task.verifier_agent_id.clone()
        };
        sqlx::query(
            "UPDATE tasks SET executor_agent_id = ?, verifier_agent_id = ?, updated_at = ? \
             WHERE id = ?",
        )
        .bind(&executor)
        .bind(&verifier)
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project_id),
            Some(&task.id),
            "task_agents_updated",
            format!(
                "任务 {} 的 Agent 覆盖已更新（执行：{}，验收：{}）",
                task.key,
                executor.as_deref().unwrap_or("跟随项目池"),
                verifier.as_deref().unwrap_or("跟随项目池")
            ),
        )
        .await;
    }
    state.notify_scheduler();
    let row = load_project_task(&state, &project_id, &task.id).await?;
    Ok(Json(TaskView::from_row(row)))
}

async fn resolve_task_agent_from_single(
    state: &AppState,
    agent_id: &str,
    role: &str,
) -> Result<String, ApiError> {
    let Some(agent) = crate::db::load_agent(&state.db, agent_id).await? else {
        return Err(ApiError::not_found(format!("Agent {agent_id} 不存在")));
    };
    if !parse_list(&agent.roles).iter().any(|item| item == role) {
        return Err(ApiError::bad(format!(
            "Agent {} 不能承担{}角色",
            agent.name,
            role_label(role)
        )));
    }
    Ok(agent.id)
}

async fn create_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<CreateTaskRequest>,
) -> Result<Json<TaskView>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档，不能再添加任务"));
    }
    crate::db::ensure_project_agents(&state.db, &id).await?;
    if !matches!(
        project.stage.as_str(),
        STAGE_ANALYSIS | STAGE_EXECUTING | STAGE_SUMMARIZING | STAGE_COMPLETED
    ) {
        return Err(ApiError::bad("当前阶段不允许添加任务"));
    }
    let rows = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM tasks WHERE project_id = ? AND COALESCE(status, '') != ? ORDER BY seq ASC",
    )
    .bind(&id)
    .bind(STATUS_CANCELED)
    .fetch_all(&state.db)
    .await?;
    let active: Vec<domain::NormalizedTask> = rows
        .iter()
        .map(|row| domain::NormalizedTask {
            key: row.key.clone(),
            title: row.title.clone(),
            description: row.description.clone(),
            acceptance: parse_list(&row.acceptance),
            deps: parse_list(&row.deps),
        })
        .collect();
    let task = domain::validate_manual_task(
        &active,
        &request.key,
        &request.title,
        &request.description,
        request.acceptance,
        request.deps,
    )
    .map_err(ApiError::bad)?;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let max_seq: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM tasks WHERE project_id = ?")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
    let max_queue: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(queue_order), 0) FROM tasks WHERE project_id = ?")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
    let task_id = domain::new_id("tsk");
    let timestamp = now();
    sqlx::query(
        "INSERT INTO tasks (id, project_id, seq, queue_order, key, title, description, \
         acceptance, deps, milestone, stage, status, attempts, rejections, verifier_issues, \
         created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, '[]', ?, ?)",
    )
    .bind(&task_id)
    .bind(&id)
    .bind(max_seq + 1)
    .bind(max_queue + 1)
    .bind(&task.key)
    .bind(&task.title)
    .bind(&task.description)
    .bind(string_list_json(&task.acceptance))
    .bind(string_list_json(&task.deps))
    .bind(if request.milestone { 1 } else { 0 })
    .bind(TASK_STAGE_PENDING)
    .bind(STATUS_QUEUED)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;
    // 新任务与任务池快照在同一事务里创建；旧字段为空就是纯项目池快照。
    crate::db::ensure_task_agents_tx(&mut tx, &task_id).await?;
    // A project in the analysis, summarizing, or completed stage moves back to
    // executing so the DAG schedules the added task. A report that would not
    // cover every task is dropped and written again after the new work finishes.
    let reopened = project.completion_mode == COMPLETION_DIRECT
        || !matches!(project.stage.as_str(), STAGE_ANALYSIS | STAGE_EXECUTING);
    let had_report = project.report.is_some();
    if reopened {
        sqlx::query(
            "UPDATE projects SET stage = ?, status = CASE WHEN status IN \
             ('pausing', 'paused', 'archived') THEN status ELSE ? END, blocked_reason = NULL, \
             report = NULL, report_feedback = NULL, completion_mode = ?, merged_at = NULL, \
             merged_commit = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(STAGE_EXECUTING)
        .bind(STATUS_RUNNING)
        .bind(COMPLETION_TASKS)
        .bind(&timestamp)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        Some(&task_id),
        "task_created",
        format!(
            "人工添加任务 {}：{}（依赖 {}）",
            task.key,
            task.title,
            if task.deps.is_empty() {
                "无".to_string()
            } else {
                task.deps.join("、")
            }
        ),
    )
    .await;
    if reopened {
        emit(
            &state.db,
            &state.bus,
            Some(&id),
            None,
            "project_reopened",
            if had_report {
                format!(
                    "人工添加任务 {}，项目回到执行阶段，原报告作废待重写",
                    task.key
                )
            } else {
                format!(
                    "人工添加任务 {}，项目从{}进入执行阶段",
                    task.key,
                    stage_label(&project.stage)
                )
            },
        )
        .await;
    }
    state.notify_scheduler();
    let row = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
        .bind(&task_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(TaskView::from_row(row)))
}

async fn pause_task(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    body: Option<Json<TaskPauseRequest>>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let request = body.map(|Json(value)| value).unwrap_or_default();
    let force = request.forced();
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    let workspace = PathBuf::from(&project.workspace);
    {
        let _guard = state.schedule_lock.lock().await;
        let task = load_project_task(&state, &project_id, &task_id).await?;
        if task.stage == TASK_STAGE_COMPLETED {
            return Err(ApiError::bad("任务已完成"));
        }
        if task.status.as_deref() == Some(STATUS_CANCELED) {
            return Err(ApiError::bad("任务已取消"));
        }
        if force {
            let running = sqlx::query_as::<_, JobRow>(
                "SELECT * FROM jobs WHERE task_id = ? AND status = 'running'",
            )
            .bind(&task.id)
            .fetch_all(&state.db)
            .await?;
            for job in &running {
                crate::agents::kill_job_dir(&workspace.join("jobs").join(&job.id));
                let finished_at = now();
                let duration_ms = job
                    .started_at
                    .as_deref()
                    .and_then(|started| usage::elapsed_ms(Some(started), &finished_at));
                sqlx::query(
                    "UPDATE jobs SET status = ?, error = ?, duration_ms = COALESCE(duration_ms, ?), \
                     finished_at = ? WHERE id = ? AND status = 'running'",
                )
                .bind(JOB_STATUS_CANCELED)
                .bind("任务被强制暂停，作业已终止")
                .bind(duration_ms)
                .bind(&finished_at)
                .bind(&job.id)
                .execute(&state.db)
                .await?;
            }
            sqlx::query(
                "UPDATE tasks SET status = ?, agent_id = NULL, waiting_reason = NULL, updated_at = ? \
                 WHERE id = ? AND stage != ?",
            )
            .bind(STATUS_PAUSED)
            .bind(now())
            .bind(&task.id)
            .bind(TASK_STAGE_COMPLETED)
            .execute(&state.db)
            .await?;
        } else {
            let running: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM jobs WHERE task_id = ? AND status = 'running'",
            )
            .bind(&task.id)
            .fetch_one(&state.db)
            .await?;
            let status = if running > 0 {
                STATUS_PAUSING
            } else {
                STATUS_PAUSED
            };
            sqlx::query(
                "UPDATE tasks SET status = ?, waiting_reason = NULL, updated_at = ? \
                 WHERE id = ? AND stage != ?",
            )
            .bind(status)
            .bind(now())
            .bind(&task.id)
            .bind(TASK_STAGE_COMPLETED)
            .execute(&state.db)
            .await?;
        }
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        if force {
            "task_paused_forced"
        } else {
            "task_pausing"
        },
        if force {
            format!("任务 {} 已强制暂停，在途作业已终止", task.key)
        } else {
            format!("任务 {} 暂停请求已提交", task.key)
        },
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

async fn resume_task(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    {
        let _guard = state.schedule_lock.lock().await;
        let task = load_project_task(&state, &project_id, &task_id).await?;
        if !matches!(
            task.status.as_deref(),
            Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
        ) {
            return Err(ApiError::bad("任务当前没有暂停"));
        }
        sqlx::query(
            "UPDATE tasks SET status = ?, waiting_reason = NULL, updated_at = ? \
             WHERE id = ? AND status IN (?, ?)",
        )
        .bind(STATUS_QUEUED)
        .bind(now())
        .bind(&task.id)
        .bind(STATUS_PAUSING)
        .bind(STATUS_PAUSED)
        .execute(&state.db)
        .await?;
        if !matches!(
            project.status.as_deref(),
            Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
        ) {
            let human_gate: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND id != ? AND stage != ? \
                 AND status = ? AND human_gate IS NOT NULL",
            )
            .bind(&project_id)
            .bind(&task.id)
            .bind(TASK_STAGE_COMPLETED)
            .bind(STATUS_BLOCKED)
            .fetch_one(&state.db)
            .await?;
            if human_gate == 0 {
                sqlx::query(
                    "UPDATE projects SET status = ?, blocked_reason = NULL, updated_at = ? \
                     WHERE id = ? AND status = ?",
                )
                .bind(STATUS_RUNNING)
                .bind(now())
                .bind(&project_id)
                .bind(STATUS_BLOCKED)
                .execute(&state.db)
                .await?;
            }
        }
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "task_resumed",
        format!("任务 {} 已恢复，回到原阶段重新排队", task.key),
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

async fn reorder_task_queue(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<TaskQueueRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let direction = request.direction.as_deref().unwrap_or("").trim();
    if request.position.is_none() && !matches!(direction, "up" | "down") {
        return Err(ApiError::bad("direction 必须是 up 或 down"));
    }
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    let _guard = state.schedule_lock.lock().await;
    let task = load_project_task(&state, &project_id, &task_id).await?;
    if task.stage != TASK_STAGE_PENDING
        || task.attempts > 0
        || task.rejections > 0
        || task.status.as_deref() == Some(STATUS_CANCELED)
    {
        return Err(ApiError::bad("只有尚未开始的任务可以调整排队顺序"));
    }
    let mut tx = state.db.begin().await?;
    let mut queue = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM tasks WHERE project_id = ? AND stage = ? AND attempts = 0 \
         AND rejections = 0 AND COALESCE(status, '') != ? ORDER BY queue_order ASC, seq ASC",
    )
    .bind(&project_id)
    .bind(TASK_STAGE_PENDING)
    .bind(STATUS_CANCELED)
    .fetch_all(&mut *tx)
    .await?;
    let Some(index) = queue.iter().position(|row| row.id == task.id) else {
        return Err(ApiError::bad("任务当前不在可调整的队列中"));
    };
    let target = if let Some(position) = request.position {
        let target = (position.max(1) as usize - 1).min(queue.len() - 1);
        if target == index {
            return Err(ApiError::bad("任务排队位置没有变化"));
        }
        let row = queue.remove(index);
        queue.insert(target, row);
        target
    } else {
        let target = match direction {
            "up" if index > 0 => index - 1,
            "down" if index + 1 < queue.len() => index + 1,
            _ => return Err(ApiError::bad("任务已经在队列边界")),
        };
        queue.swap(index, target);
        target
    };
    let timestamp = now();
    for (position, row) in queue.iter().enumerate() {
        sqlx::query(
            "UPDATE tasks SET queue_order = ?, updated_at = ? \
             WHERE id = ? AND stage = ? AND attempts = 0 AND COALESCE(status, '') != ?",
        )
        .bind((position + 1) as i64)
        .bind(&timestamp)
        .bind(&row.id)
        .bind(TASK_STAGE_PENDING)
        .bind(STATUS_CANCELED)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "task_queue_reordered",
        format!(
            "任务 {} 排队顺序已调整到第 {} 位{}",
            task.key,
            target + 1,
            match direction {
                "up" => "（向上）",
                "down" => "（向下）",
                _ => "",
            }
        ),
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

fn confirm_mode(value: &str) -> Result<&'static str, ApiError> {
    match value.trim() {
        CONFIRM_MANUAL => Ok(CONFIRM_MANUAL),
        CONFIRM_AUTO => Ok(CONFIRM_AUTO),
        _ => Err(ApiError::bad("确认方式必须是 manual 或 auto")),
    }
}

async fn running_project_job(
    state: &AppState,
    project_id: &str,
    role: &str,
) -> Result<bool, ApiError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE project_id = ? AND role = ? AND status = 'running'",
    )
    .bind(project_id)
    .bind(role)
    .fetch_one(&state.db)
    .await?;
    Ok(count > 0)
}

async fn post_analysis_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<AnalysisMessageRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let message = request.message.trim();
    if message.is_empty() {
        return Err(ApiError::bad("消息内容不能为空"));
    }
    let mode = request
        .confirm_mode
        .as_deref()
        .map(confirm_mode)
        .transpose()?;
    let project = require_project(&state, &id).await?;
    if project.stage != STAGE_ANALYSIS {
        return Err(ApiError::bad("当前阶段不允许输入项目分析消息"));
    }
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if matches!(
        project.status.as_deref(),
        Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
    ) {
        return Err(ApiError::bad("项目已暂停，请先恢复"));
    }
    if running_project_job(&state, &id, JOB_ROLE_ANALYSIS).await? {
        return Err(ApiError::bad("规划者正在处理上一条消息"));
    }
    {
        let _guard = state.schedule_lock.lock().await;
        sqlx::query(
            "INSERT INTO messages (id, project_id, role, content, created_at) \
             VALUES (?, ?, 'user', ?, ?)",
        )
        .bind(domain::new_id("msg"))
        .bind(&id)
        .bind(message)
        .bind(now())
        .execute(&state.db)
        .await?;
        // 项目目标只在首次输入时写入；后续消息属于分析对话或方案修订，
        // 目标由方案中的 goal 字段在确认时更新。
        sqlx::query(
            "UPDATE projects SET goal = CASE WHEN TRIM(goal) = '' THEN ? ELSE goal END, \
             confirm_mode = COALESCE(?, confirm_mode), proposal = NULL, confirmed_at = NULL, \
             status = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(message)
        .bind(mode)
        .bind(STATUS_RUNNING)
        .bind(now())
        .bind(&id)
        .execute(&state.db)
        .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        "analysis_message",
        "收到用户消息，规划者开始项目分析",
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &id).await.map(Json)
}

async fn update_analysis_policy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<AnalysisPolicyRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let mode = confirm_mode(&request.mode)?;
    let project = require_project(&state, &id).await?;
    if project.stage != STAGE_ANALYSIS {
        return Err(ApiError::bad("只有项目分析阶段可以调整确认方式"));
    }
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if running_project_job(&state, &id, JOB_ROLE_ANALYSIS).await? {
        return Err(ApiError::bad("规划者正在处理当前消息"));
    }
    {
        let _guard = state.schedule_lock.lock().await;
        sqlx::query("UPDATE projects SET confirm_mode = ?, updated_at = ? WHERE id = ?")
            .bind(mode)
            .bind(now())
            .bind(&id)
            .execute(&state.db)
            .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        "analysis_policy_updated",
        format!(
            "确认方式已设为{}",
            if mode == CONFIRM_AUTO {
                "自动确认"
            } else {
                "人工确认"
            }
        ),
    )
    .await;
    load_detail(&state, &id).await.map(Json)
}

async fn confirm_analysis(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(_request): Json<ConfirmAnalysisRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.stage != STAGE_ANALYSIS {
        return Err(ApiError::bad("当前阶段不允许确认项目方案"));
    }
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if matches!(
        project.status.as_deref(),
        Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
    ) {
        return Err(ApiError::bad("项目已暂停，请先恢复"));
    }
    if running_project_job(&state, &id, JOB_ROLE_ANALYSIS).await? {
        return Err(ApiError::bad("规划者正在更新项目方案，请稍后再确认"));
    }
    let proposal_raw = project
        .proposal
        .as_deref()
        .ok_or_else(|| ApiError::bad("规划者还没有给出可确认的项目方案"))?;
    let proposal: AnalysisProposal =
        serde_json::from_str(proposal_raw).map_err(|_| ApiError::bad("项目方案格式不合法"))?;
    if !matches!(proposal.kind.as_str(), "plan" | "direct") {
        return Err(ApiError::bad("当前项目方案不能确认"));
    }
    let goal = proposal
        .goal
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(project.goal.trim())
        .to_string();
    if goal.is_empty() {
        return Err(ApiError::bad("目标内容为空"));
    }
    if proposal.kind == "direct" {
        let active_tasks: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage != ? \
             AND COALESCE(status, '') != ?",
        )
        .bind(&id)
        .bind(TASK_STAGE_COMPLETED)
        .bind(STATUS_CANCELED)
        .fetch_one(&state.db)
        .await?;
        if active_tasks > 0 {
            return Err(ApiError::bad(
                "项目已有任务，不能直接完成；请继续规划或删除任务",
            ));
        }
        {
            let _guard = state.schedule_lock.lock().await;
            sqlx::query(
                "UPDATE projects SET goal = ?, stage = ?, status = ?, completion_mode = ?, \
                 confirmed_at = ?, blocked_reason = NULL, report = NULL, report_feedback = NULL, \
                 updated_at = ? WHERE id = ?",
            )
            .bind(&goal)
            .bind(STAGE_EXECUTING)
            .bind(STATUS_RUNNING)
            .bind(COMPLETION_DIRECT)
            .bind(now())
            .bind(now())
            .bind(&id)
            .execute(&state.db)
            .await?;
        }
        emit(
            &state.db,
            &state.bus,
            Some(&id),
            None,
            "analysis_confirmed",
            "方案已确认，开始直接完成",
        )
        .await;
    } else {
        let tasks = domain::validate_plan(
            proposal
                .tasks
                .iter()
                .map(|task| domain::PlanTask {
                    key: task.key.clone(),
                    title: task.title.clone(),
                    description: task.description.clone(),
                    acceptance: task.acceptance.clone(),
                    deps: task.deps.clone(),
                })
                .collect(),
        )
        .map_err(ApiError::bad)?;
        crate::runner::apply_analysis_plan(
            &state,
            &project,
            &goal,
            &tasks,
            &proposal.summary,
            proposal_raw,
        )
        .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&id),
            None,
            "analysis_confirmed",
            "方案已确认，开始执行任务",
        )
        .await;
    }
    state.notify_scheduler();
    load_detail(&state, &id).await.map(Json)
}

async fn pause_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<ProjectPauseRequest>>,
) -> Result<Json<Value>, ApiError> {
    let request = body.map(|Json(value)| value).unwrap_or_default();
    let force = request.forced();
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    let workspace = PathBuf::from(&project.workspace);
    {
        let _guard = state.schedule_lock.lock().await;
        if force {
            let running = sqlx::query_as::<_, JobRow>(
                "SELECT * FROM jobs WHERE project_id = ? AND status = 'running'",
            )
            .bind(&id)
            .fetch_all(&state.db)
            .await?;
            for job in &running {
                crate::agents::kill_job_dir(&workspace.join("jobs").join(&job.id));
                let finished_at = now();
                let duration_ms = job
                    .started_at
                    .as_deref()
                    .and_then(|started| usage::elapsed_ms(Some(started), &finished_at));
                sqlx::query(
                    "UPDATE jobs SET status = ?, error = ?, duration_ms = COALESCE(duration_ms, ?), \
                     finished_at = ? WHERE id = ? AND status = 'running'",
                )
                .bind(JOB_STATUS_CANCELED)
                .bind("项目被强制暂停，作业已终止")
                .bind(duration_ms)
                .bind(&finished_at)
                .bind(&job.id)
                .execute(&state.db)
                .await?;
            }
            sqlx::query(
                "UPDATE tasks SET status = ?, agent_id = NULL, waiting_reason = NULL, updated_at = ? \
                 WHERE project_id = ? AND stage != ? AND status != ?",
            )
            .bind(STATUS_PAUSED)
            .bind(now())
            .bind(&id)
            .bind(TASK_STAGE_COMPLETED)
            .bind(STATUS_CANCELED)
            .execute(&state.db)
            .await?;
            sqlx::query(
                "UPDATE projects SET status = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
            )
            .bind(STATUS_PAUSED)
            .bind(now())
            .bind(&id)
            .execute(&state.db)
            .await?;
        } else {
            sqlx::query("UPDATE projects SET status = ?, updated_at = ? WHERE id = ?")
                .bind(STATUS_PAUSING)
                .bind(now())
                .bind(&id)
                .execute(&state.db)
                .await?;
        }
    }
    if force {
        emit(
            &state.db,
            &state.bus,
            Some(&id),
            None,
            "project_paused_forced",
            "项目已强制暂停，所有在途作业已终止",
        )
        .await;
    } else {
        emit(
            &state.db,
            &state.bus,
            Some(&id),
            None,
            "project_pausing",
            "项目暂停请求已提交，等待在途作业结束",
        )
        .await;
    }
    state.notify_scheduler();
    Ok(Json(json!({"ok": true, "forced": force})))
}

async fn resume_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档，无法恢复"));
    }
    if !matches!(
        project.status.as_deref(),
        Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
    ) {
        return Err(ApiError::bad("项目当前没有暂停"));
    }
    let last_message_role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM messages WHERE project_id = ? ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(&project.id)
    .fetch_optional(&state.db)
    .await?;
    let replan_pending = replan::pending(&state.db, &project).await?;
    let waiting_human = (project.stage == STAGE_ANALYSIS
        && !replan_pending
        && last_message_role.as_deref() != Some("user"))
        || (project.stage == STAGE_SUMMARIZING && project.report.is_some());
    {
        let _guard = state.schedule_lock.lock().await;
        sqlx::query(
            "UPDATE tasks SET status = ?, waiting_reason = NULL, updated_at = ? \
             WHERE project_id = ? AND stage != ? AND status IN (?, ?)",
        )
        .bind(STATUS_QUEUED)
        .bind(now())
        .bind(&id)
        .bind(TASK_STAGE_COMPLETED)
        .bind(STATUS_PAUSED)
        .bind(STATUS_PAUSING)
        .execute(&state.db)
        .await?;
        sqlx::query(
            "UPDATE projects SET status = ?, blocked_reason = CASE WHEN ? THEN blocked_reason \
             ELSE NULL END, updated_at = ? WHERE id = ?",
        )
        .bind(if waiting_human {
            STATUS_BLOCKED
        } else {
            STATUS_RUNNING
        })
        .bind(waiting_human)
        .bind(now())
        .bind(&id)
        .execute(&state.db)
        .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        "project_resumed",
        "项目已恢复",
    )
    .await;
    state.notify_scheduler();
    Ok(Json(json!({"ok": true})))
}

async fn archive_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    {
        let _guard = state.schedule_lock.lock().await;
        sqlx::query("UPDATE projects SET status = ?, updated_at = ? WHERE id = ?")
            .bind(STATUS_ARCHIVED)
            .bind(now())
            .bind(&id)
            .execute(&state.db)
            .await?;
        let running = sqlx::query_as::<_, JobRow>(
            "SELECT * FROM jobs WHERE project_id = ? AND status = 'running'",
        )
        .bind(&id)
        .fetch_all(&state.db)
        .await?;
        for job in running {
            let dir = PathBuf::from(&project.workspace).join("jobs").join(&job.id);
            kill_job_dir(&dir);
        }
    }
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        "project_archived",
        "项目已归档，所有在途作业已终止",
    )
    .await;
    state.notify_scheduler();
    Ok(Json(json!({"ok": true})))
}

async fn submit_report(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ReportRequest>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    let feedback = request
        .feedback
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let event_kind: Option<&'static str>;
    let event_message: String;
    let mut replan_reason: Option<String> = None;
    {
        let _guard = state.schedule_lock.lock().await;
        match request.action.as_str() {
            // 多轮评议：先记录意见，由规划者在下一轮重写时参考。
            "comment" => {
                if project.stage != STAGE_SUMMARIZING {
                    return Err(ApiError::bad("项目不在总结阶段，无法对报告发表意见"));
                }
                require_opinion(&feedback)?;
                record_report_opinion(&state, &id, &feedback).await?;
                event_kind = None;
                event_message = String::new();
            }
            "accept" => {
                if project.stage != STAGE_SUMMARIZING || project.report.is_none() {
                    return Err(ApiError::bad("当前没有等待验收的项目报告"));
                }
                if !feedback.is_empty() {
                    let round = record_report_opinion(&state, &id, &feedback).await?;
                    event_message = format!("项目报告已通过（附第 {round} 条意见），项目完成");
                } else {
                    event_message = "项目报告已通过，项目完成".into();
                }
                sqlx::query(
                    "UPDATE projects SET stage = ?, status = NULL, blocked_reason = NULL, \
                     report_feedback = NULL, updated_at = ? WHERE id = ?",
                )
                .bind(STAGE_COMPLETED)
                .bind(now())
                .bind(&id)
                .execute(&state.db)
                .await?;
                event_kind = Some("project_completed");
            }
            // 处置一：要求重新撰写（可多轮重复，意见持久化 + 触发重写作业）。
            "reject" => {
                if project.stage != STAGE_SUMMARIZING || project.report.is_none() {
                    return Err(ApiError::bad("当前没有等待验收的项目报告"));
                }
                require_opinion(&feedback)?;
                let round = record_report_opinion(&state, &id, &feedback).await?;
                if project.completion_mode == COMPLETION_DIRECT {
                    sqlx::query(
                        "UPDATE projects SET stage = ?, status = ?, report = NULL, \
                         report_feedback = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
                    )
                    .bind(STAGE_EXECUTING)
                    .bind(STATUS_RUNNING)
                    .bind(&feedback)
                    .bind(now())
                    .bind(&id)
                    .execute(&state.db)
                    .await?;
                    event_kind = Some("report_rejected");
                    event_message = format!(
                        "项目报告被要求重做（第 {round} 条意见），重新直接完成：{feedback}"
                    );
                } else {
                    sqlx::query(
                        "UPDATE projects SET stage = ?, status = ?, report = NULL, \
                         report_feedback = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
                    )
                    .bind(STAGE_SUMMARIZING)
                    .bind(STATUS_RUNNING)
                    .bind(&feedback)
                    .bind(now())
                    .bind(&id)
                    .execute(&state.db)
                    .await?;
                    event_kind = Some("report_rejected");
                    event_message = format!("项目报告被要求重写（第 {round} 条意见）：{feedback}");
                }
            }
            // 处置二：要求重新规划，项目回到项目分析阶段（复用重规划路径）。
            "replan" => {
                if project.stage != STAGE_SUMMARIZING || project.report.is_none() {
                    return Err(ApiError::bad("当前没有等待验收的项目报告"));
                }
                require_opinion(&feedback)?;
                let round = record_report_opinion(&state, &id, &feedback).await?;
                event_kind = None;
                event_message = format!(
                    "项目报告被要求重新规划（第 {round} 条意见），项目回到项目分析阶段：{feedback}"
                );
                replan_reason = Some(feedback.clone());
            }
            _ => {
                return Err(ApiError::bad(
                    "action 必须是 accept、reject、replan 或 comment",
                ))
            }
        }
    }
    // replan::request 自己加调度锁并发出 replan_requested / plan_superseded 事件，
    // 因此放在锁外调用。
    if let Some(reason) = replan_reason {
        replan::request(&state, &id, &reason, REPLAN_SOURCE_HUMAN).await?;
        state.notify_scheduler();
        return Ok(Json(json!({"ok": true, "action": "replan"})));
    }
    if let Some(event_kind) = event_kind {
        emit(
            &state.db,
            &state.bus,
            Some(&id),
            None,
            event_kind,
            event_message,
        )
        .await;
    }
    state.notify_scheduler();
    Ok(Json(json!({"ok": true, "action": request.action})))
}

/// 意见不能为空：评议记录必须可回溯，空意见不会产生任何处置。
fn require_opinion(feedback: &str) -> Result<(), ApiError> {
    if feedback.trim().is_empty() {
        return Err(ApiError::bad("必须填写意见"));
    }
    Ok(())
}

/// 把一条人工意见写入 messages（角色 review），并返回它在评议记录中的序号。
async fn record_report_opinion(
    state: &AppState,
    project_id: &str,
    feedback: &str,
) -> Result<i64, ApiError> {
    let existing: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE project_id = ? AND role = ?")
            .bind(project_id)
            .bind(REPORT_MESSAGE_ROLE)
            .fetch_one(&state.db)
            .await?;
    crate::runner::insert_message(&state.db, project_id, REPORT_MESSAGE_ROLE, feedback)
        .await
        .map_err(ApiError::internal)?;
    emit(
        &state.db,
        &state.bus,
        Some(project_id),
        None,
        "report_opinion",
        format!(
            "人工提交报告意见（第 {} 条）：{}",
            existing + 1,
            preview_opinion(feedback)
        ),
    )
    .await;
    Ok(existing + 1)
}

fn preview_opinion(feedback: &str) -> String {
    let first = feedback.lines().next().unwrap_or(feedback);
    let mut text: String = first.chars().take(80).collect();
    if first.chars().count() > 80 {
        text.push('…');
    }
    text
}

/// Rebases the project branch onto its base branch and fast-forwards the base
/// branch. Only completed projects can be merged, and any failure leaves both
/// branch refs at their pre-merge commits.
async fn merge_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.stage != STAGE_COMPLETED {
        return Err(ApiError::bad("项目尚未完成，只有完成阶段的项目可以合入"));
    }
    let _guard = state.git_lock.lock().await;
    let project = require_project(&state, &id).await?;
    if project.stage != STAGE_COMPLETED {
        return Err(ApiError::bad("项目尚未完成，只有完成阶段的项目可以合入"));
    }
    let repo = PathBuf::from(&project.repo_path);
    let worktree = PathBuf::from(&project.workspace).join("repo");
    let base_branch = project.base_branch.clone();
    let project_branch = project.branch.clone();

    let base_before = git::commit_of(&repo, &base_branch)
        .await
        .map_err(|error| ApiError::bad(format!("无法读取目标分支 {base_branch}：{error:#}")))?;
    let project_before = git::commit_of(&repo, &project_branch)
        .await
        .map_err(|error| ApiError::bad(format!("无法读取项目分支 {project_branch}：{error:#}")))?;

    if git::is_ancestor(&repo, &project_before, &base_before).await? {
        return finish_project_merge(
            &state,
            &project,
            "up_to_date",
            format!("已是最新：{base_branch} 已包含项目分支 {project_branch} 的全部提交"),
            &project_before,
        )
        .await;
    }

    let base_worktree = git::worktree_for_branch(&repo, &base_branch).await?;
    if let Some(path) = base_worktree.as_deref() {
        ensure_base_worktree_clean(path, &base_branch).await?;
    }

    if let Err(error) = git::rebase_onto(&worktree, &base_branch).await {
        restore_project_branch(&repo, &worktree, &project_branch, &project_before).await;
        return Err(ApiError::bad(format!(
            "合入失败：{error:#}；项目分支与目标分支均保持在操作前状态"
        )));
    }

    let project_after = git::commit_of(&repo, &project_branch).await?;
    let base_now = git::commit_of(&repo, &base_branch).await?;
    if base_now != base_before {
        restore_project_branch(&repo, &worktree, &project_branch, &project_before).await;
        return Err(ApiError::bad(format!(
            "目标分支 {base_branch} 在合入期间发生变化，已取消合入"
        )));
    }
    if !git::is_ancestor(&repo, &base_before, &project_after).await? {
        restore_project_branch(&repo, &worktree, &project_branch, &project_before).await;
        return Err(ApiError::bad("rebase 结果未包含目标分支，已取消合入"));
    }
    if git::count_merge_commits(&repo, &base_before, &project_after).await? > 0 {
        restore_project_branch(&repo, &worktree, &project_branch, &project_before).await;
        return Err(ApiError::bad("rebase 结果包含合并提交，已取消合入"));
    }
    if git::is_ancestor(&repo, &project_after, &base_before).await? {
        return finish_project_merge(
            &state,
            &project,
            "up_to_date",
            format!("已是最新：{base_branch} 已包含项目分支 {project_branch} 的全部提交"),
            &project_after,
        )
        .await;
    }

    if let Some(path) = base_worktree.as_deref() {
        if let Err(error) = ensure_base_worktree_clean(path, &base_branch).await {
            restore_project_branch(&repo, &worktree, &project_branch, &project_before).await;
            return Err(error);
        }
        if let Err(error) = git::merge_ff_only(path, &project_branch).await {
            restore_fast_forward_failure(path, &repo, &base_branch, &base_before).await;
            restore_project_branch(&repo, &worktree, &project_branch, &project_before).await;
            return Err(ApiError::bad(format!(
                "快进目标分支 {base_branch} 失败，已回滚项目分支：{error:#}"
            )));
        }
    } else if let Err(error) =
        git::update_branch_ref(&repo, &base_branch, &project_after, &base_before).await
    {
        restore_project_branch(&repo, &worktree, &project_branch, &project_before).await;
        return Err(ApiError::bad(format!(
            "快进目标分支 {base_branch} 失败，已回滚项目分支：{error:#}"
        )));
    }

    let base_after = git::commit_of(&repo, &base_branch).await?;
    if base_after != project_after {
        if let Some(path) = base_worktree.as_deref() {
            restore_fast_forward_failure(path, &repo, &base_branch, &base_before).await;
        }
        restore_project_branch(&repo, &worktree, &project_branch, &project_before).await;
        return Err(ApiError::bad(format!(
            "目标分支 {base_branch} 未快进到项目分支提交，已回滚"
        )));
    }

    finish_project_merge(
        &state,
        &project,
        "merged",
        format!(
            "已合入：{} 已 rebase 到 {base_branch} 并快进，提交 {}",
            project_branch,
            short_commit(&project_after)
        ),
        &project_after,
    )
    .await
}

async fn ensure_base_worktree_clean(path: &std::path::Path, branch: &str) -> Result<(), ApiError> {
    let status = git::status_porcelain(path)
        .await
        .map_err(ApiError::internal)?;
    if status.trim().is_empty() {
        return Ok(());
    }
    let summary = status
        .lines()
        .take(6)
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    Err(ApiError::bad(format!(
        "目标分支 {branch} 在 {} 检出且有未提交改动，已取消合入。请先提交或清理改动后重试。\n{summary}",
        path.display()
    )))
}

async fn restore_project_branch(
    repo: &std::path::Path,
    worktree: &std::path::Path,
    branch: &str,
    original: &str,
) {
    git::abort_rebase(worktree).await;
    match git::commit_of(repo, branch).await {
        Ok(current) if current == original => {}
        Ok(_) => {
            if let Err(error) = git::reset_hard_to(worktree, original).await {
                tracing::error!(
                    branch,
                    error = %format!("{error:#}"),
                    "回滚项目分支失败"
                );
            }
        }
        Err(error) => {
            tracing::error!(
                branch,
                error = %format!("{error:#}"),
                "读取项目分支失败，无法回滚"
            );
        }
    }
}

async fn restore_fast_forward_failure(
    base_worktree: &std::path::Path,
    repo: &std::path::Path,
    branch: &str,
    original: &str,
) {
    match git::commit_of(repo, branch).await {
        Ok(current) if current == original => {}
        Ok(_) => {
            if let Err(error) = git::reset_hard_to(base_worktree, original).await {
                tracing::error!(
                    branch,
                    error = %format!("{error:#}"),
                    "回滚目标分支失败"
                );
            }
        }
        Err(error) => {
            tracing::error!(
                branch,
                error = %format!("{error:#}"),
                "读取目标分支失败，无法回滚"
            );
        }
    }
}

async fn finish_project_merge(
    state: &AppState,
    project: &ProjectRow,
    status: &str,
    message: String,
    commit: &str,
) -> Result<Json<Value>, ApiError> {
    let (merged_at, merged_commit) = if status == "up_to_date" {
        match (&project.merged_at, &project.merged_commit) {
            (Some(at), Some(existing)) => (at.clone(), existing.clone()),
            _ => (now(), commit.to_string()),
        }
    } else {
        (now(), commit.to_string())
    };
    sqlx::query(
        "UPDATE projects SET merged_at = ?, merged_commit = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&merged_at)
    .bind(&merged_commit)
    .bind(&merged_at)
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    if status == "merged" {
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            None,
            "project_merged",
            format!(
                "项目分支 {} 已 rebase 到 {} 并合入，提交 {}",
                project.branch,
                project.base_branch,
                short_commit(&merged_commit)
            ),
        )
        .await;
    }
    Ok(Json(json!({
        "status": status,
        "message": message,
        "merged_at": merged_at,
        "merged_commit": merged_commit,
        "project": load_detail(state, &project.id).await?,
    })))
}

fn short_commit(commit: &str) -> String {
    commit.chars().take(7).collect()
}

async fn retry_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    {
        let _guard = state.schedule_lock.lock().await;
        match project.stage.as_str() {
            STAGE_ANALYSIS => {
                let last_message_role: Option<String> = sqlx::query_scalar(
                    "SELECT role FROM messages WHERE project_id = ? \
                     ORDER BY created_at DESC, id DESC LIMIT 1",
                )
                .bind(&id)
                .fetch_optional(&state.db)
                .await?;
                let replan_pending = replan::pending(&state.db, &project).await?;
                let waiting_user = !replan_pending
                    && (project.proposal.is_some() || last_message_role.as_deref() != Some("user"));
                let reason = if project.proposal.is_some() {
                    "等待用户确认方案"
                } else {
                    "等待用户答复"
                };
                sqlx::query(
                    "UPDATE projects SET status = ?, blocked_reason = ?, avoid_agent_id = NULL, \
                     avoid_agent_role = NULL, updated_at = ? WHERE id = ?",
                )
                .bind(if waiting_user {
                    STATUS_BLOCKED
                } else {
                    STATUS_RUNNING
                })
                .bind(if waiting_user { Some(reason) } else { None })
                .bind(now())
                .bind(&id)
                .execute(&state.db)
                .await?;
                clear_project_agent_backoff(&state, &id).await?;
            }
            STAGE_EXECUTING => {
                if project.completion_mode == COMPLETION_DIRECT {
                    sqlx::query(
                        "UPDATE projects SET status = ?, blocked_reason = NULL, avoid_agent_id = NULL, \
                         avoid_agent_role = NULL, updated_at = ? WHERE id = ?",
                    )
                    .bind(STATUS_RUNNING)
                    .bind(now())
                    .bind(&id)
                    .execute(&state.db)
                    .await?;
                    clear_project_agent_backoff(&state, &id).await?;
                } else {
                    sqlx::query(
                        "UPDATE tasks SET status = ?, attempts = 0, rejections = 0, agent_id = NULL, \
                         last_agent_id = NULL, avoid_agent_id = NULL, avoid_agent_role = NULL, \
                         last_error = NULL, last_failure_kind = NULL, \
                         waiting_reason = NULL, updated_at = ? \
                         WHERE project_id = ? AND status = ?",
                    )
                    .bind(STATUS_QUEUED)
                    .bind(now())
                    .bind(&id)
                    .bind(STATUS_BLOCKED)
                    .execute(&state.db)
                    .await?;
                    clear_project_agent_backoff(&state, &id).await?;
                    sqlx::query(
                        "UPDATE projects SET status = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
                    )
                    .bind(STATUS_RUNNING)
                    .bind(now())
                    .bind(&id)
                    .execute(&state.db)
                    .await?;
                }
            }
            STAGE_SUMMARIZING => {
                if project.report.is_some() {
                    return Err(ApiError::bad("报告正在等待人工验收"));
                }
                sqlx::query(
                    "UPDATE projects SET status = ?, blocked_reason = NULL, avoid_agent_id = NULL, \
                     avoid_agent_role = NULL, updated_at = ? WHERE id = ?",
                )
                .bind(STATUS_RUNNING)
                .bind(now())
                .bind(&id)
                .execute(&state.db)
                .await?;
                clear_project_agent_backoff(&state, &id).await?;
            }
            _ => return Err(ApiError::bad("当前阶段没有可重试的内容")),
        }
    }
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        "project_retry",
        "已请求重试项目",
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &id).await.map(Json)
}

async fn replan_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ReplanRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    if !matches!(project.stage.as_str(), STAGE_EXECUTING | STAGE_SUMMARIZING) {
        return Err(ApiError::bad("只有执行或总结阶段可以重新规划"));
    }
    let reason = request
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad("重新规划必须填写原因"))?;
    let outcome = replan::request(&state, &id, reason, REPLAN_SOURCE_HUMAN).await?;
    let Some(summary) = outcome else {
        return Err(ApiError::bad("项目状态已变化，未执行重新规划"));
    };
    tracing::info!(
        project_id = %id,
        canceled_tasks = summary.canceled_tasks,
        retained_tasks = summary.retained_tasks,
        canceled_jobs = summary.canceled_jobs,
        "项目已重新规划"
    );
    state.notify_scheduler();
    load_detail(&state, &id).await.map(Json)
}

async fn save_task_guidance(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<TaskGuidanceRequest>,
) -> Result<Json<TaskView>, ApiError> {
    let guidance = request
        .guidance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad("人工指导不能为空"))?
        .to_string();
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    let Some(task) =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ? AND project_id = ?")
            .bind(&task_id)
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await?
    else {
        return Err(ApiError::not_found("任务不存在"));
    };
    if task.stage == TASK_STAGE_COMPLETED {
        return Err(ApiError::bad("任务已完成，不能再添加指导"));
    }
    if task.status.as_deref() == Some(STATUS_CANCELED) {
        return Err(ApiError::bad("任务已取消，不能再添加指导"));
    }
    let timestamp = now();
    sqlx::query("UPDATE tasks SET guidance = ?, updated_at = ? WHERE id = ?")
        .bind(&guidance)
        .bind(&timestamp)
        .bind(&task.id)
        .execute(&state.db)
        .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "guidance",
        format!("任务 {} 收到人工指导：{guidance}", task.key),
    )
    .await;
    let row = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
        .bind(&task.id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(TaskView::from_row(row)))
}

async fn set_task_milestone(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<MilestoneRequest>,
) -> Result<Json<TaskView>, ApiError> {
    let milestone = match (request.milestone, request.action.as_deref()) {
        (Some(value), _) => value,
        (None, Some("set" | "enable")) => true,
        (None, Some("clear" | "cancel" | "disable")) => false,
        _ => return Err(ApiError::bad("必须指定 milestone 布尔值或 set/clear 动作")),
    };
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    if task.stage == TASK_STAGE_COMPLETED {
        return Err(ApiError::bad("任务已完成，不能修改里程碑标记"));
    }
    if task.status.as_deref() == Some(STATUS_CANCELED) {
        return Err(ApiError::bad("任务已取消，不能修改里程碑标记"));
    }
    if task.human_gate.as_deref() == Some(HUMAN_GATE_MILESTONE) {
        return Err(ApiError::bad("任务正在等待人工验收，不能修改里程碑标记"));
    }
    sqlx::query("UPDATE tasks SET milestone = ?, updated_at = ? WHERE id = ?")
        .bind(if milestone { 1 } else { 0 })
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        if milestone {
            "milestone_set"
        } else {
            "milestone_cleared"
        },
        if milestone {
            format!("任务 {} 已标记为里程碑，验收通过后需要人工确认", task.key)
        } else {
            format!("任务 {} 已取消里程碑标记", task.key)
        },
    )
    .await;
    let row = load_project_task(&state, &project_id, &task.id).await?;
    Ok(Json(TaskView::from_row(row)))
}

async fn accept_milestone(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<MilestoneAcceptRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let note = request
        .reason
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let (project, task) = {
        let _guard = state.schedule_lock.lock().await;
        let project = require_project(&state, &project_id).await?;
        if project.status.as_deref() == Some(STATUS_ARCHIVED) {
            return Err(ApiError::bad("项目已归档"));
        }
        if project.stage == STAGE_COMPLETED {
            return Err(ApiError::bad("项目已完成"));
        }
        let task = load_project_task(&state, &project_id, &task_id).await?;
        if task.status.as_deref() != Some(STATUS_BLOCKED)
            || task.human_gate.as_deref() != Some(HUMAN_GATE_MILESTONE)
        {
            return Err(ApiError::bad("任务不在等待人工验收状态"));
        }
        sqlx::query(
            "UPDATE tasks SET human_gate = 'milestone_accepting', updated_at = ? WHERE id = ?",
        )
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
        (project, task)
    };

    let Some(branch) = task.branch.clone() else {
        restore_milestone_gate(&state, &task.id, "任务缺少执行分支，无法完成人工验收").await?;
        return Err(ApiError::bad("任务缺少执行分支，无法完成人工验收"));
    };
    let workspace = PathBuf::from(&project.workspace);
    let message = format!("domo(task-{}): {}", task.key, task.title);
    let merge = {
        let _guard = state.git_lock.lock().await;
        git::squash_merge(&workspace.join("repo"), &branch, &message).await
    };
    if let Err(error) = merge {
        let reason = format!("人工验收通过但合并失败：{error:#}");
        restore_milestone_gate(&state, &task.id, &reason).await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project_id),
            Some(&task.id),
            "milestone_merge_conflict",
            format!("任务 {} 人工验收合并失败：{reason}", task.key),
        )
        .await;
        return Err(ApiError::internal(error));
    }

    {
        let _guard = state.schedule_lock.lock().await;
        let fresh_project = require_project(&state, &project_id).await?;
        sqlx::query(
            "UPDATE tasks SET stage = ?, status = NULL, human_gate = NULL, last_error = NULL, \
             forced_agent_id = NULL, forced_agent_role = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(TASK_STAGE_COMPLETED)
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
        sqlx::query(
            "UPDATE projects SET status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
             THEN status ELSE ? END, blocked_reason = CASE WHEN status IN ('pausing', 'paused') \
             THEN ? ELSE NULL END, updated_at = ? WHERE id = ?",
        )
        .bind(STATUS_RUNNING)
        .bind("人工验收已通过，等待项目恢复后继续")
        .bind(now())
        .bind(&project_id)
        .execute(&state.db)
        .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project_id),
            Some(&task.id),
            "milestone_accepted",
            format!(
                "任务 {} 人工验收通过{}，已合并到项目交付分支",
                task.key,
                if note.is_empty() {
                    String::new()
                } else {
                    format!("：{note}")
                }
            ),
        )
        .await;
        emit(
            &state.db,
            &state.bus,
            Some(&project_id),
            Some(&task.id),
            "task_completed",
            format!("任务 {} 人工验收通过并合并到项目分支", task.key),
        )
        .await;
        if fresh_project.status.as_deref() == Some(STATUS_ARCHIVED) {
            tracing::warn!(project_id = %project_id, "项目在人工验收合并期间被归档");
        }
    }
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

async fn restore_milestone_gate(
    state: &AppState,
    task_id: &str,
    reason: &str,
) -> Result<(), ApiError> {
    let _guard = state.schedule_lock.lock().await;
    sqlx::query(
        "UPDATE tasks SET human_gate = ?, status = ?, last_error = ?, updated_at = ? WHERE id = ?",
    )
    .bind(HUMAN_GATE_MILESTONE)
    .bind(STATUS_BLOCKED)
    .bind(reason)
    .bind(now())
    .bind(task_id)
    .execute(&state.db)
    .await?;
    Ok(())
}

async fn reject_milestone(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<MilestoneRejectRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let reason = request.reason.trim().to_string();
    if reason.is_empty() {
        return Err(ApiError::bad("拒绝人工验收时必须填写理由"));
    }
    let guidance = request
        .guidance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let disposition = milestone_reject_disposition(request.action.as_deref())?;
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    if task.status.as_deref() != Some(STATUS_BLOCKED)
        || task.human_gate.as_deref() != Some(HUMAN_GATE_MILESTONE)
    {
        return Err(ApiError::bad("任务不在等待人工验收状态"));
    }
    if disposition == MILESTONE_DISPOSITION_REPLAN {
        sqlx::query("UPDATE tasks SET human_gate = ?, updated_at = ? WHERE id = ?")
            .bind(HUMAN_GATE_MILESTONE_REPLANNING)
            .bind(now())
            .bind(&task.id)
            .execute(&state.db)
            .await?;
        drop(_guard);
        return replan_rejected_milestone(&state, project, task, &reason, guidance).await;
    }
    let issues = append_issue(&task.verifier_issues, &reason);
    let guidance = guidance.unwrap_or_else(|| task.guidance.clone());
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, human_gate = NULL, agent_id = NULL, \
         last_error = NULL, verifier_issues = ?, guidance = ?, forced_agent_id = NULL, \
         forced_agent_role = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_EXECUTING)
    .bind(STATUS_QUEUED)
    .bind(&issues)
    .bind(&guidance)
    .bind(now())
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    let paused = matches!(
        project.status.as_deref(),
        Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
    );
    sqlx::query(
        "UPDATE projects SET status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = CASE WHEN ? THEN ? ELSE NULL END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(STATUS_RUNNING)
    .bind(paused)
    .bind("人工验收已拒绝，等待项目恢复后重新执行")
    .bind(now())
    .bind(&project_id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "milestone_rejected",
        format!("任务 {} 人工验收被拒绝，退回执行：{reason}", task.key),
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

fn milestone_reject_disposition(action: Option<&str>) -> Result<&'static str, ApiError> {
    let Some(action) = action.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(MILESTONE_DISPOSITION_RETRY);
    };
    match action.to_ascii_lowercase().as_str() {
        "retry" | "rework" | "reject" | "execute" | "execution" | "requeue" => {
            Ok(MILESTONE_DISPOSITION_RETRY)
        }
        "replan" | "replanning" | "replan_project" | "plan" => Ok(MILESTONE_DISPOSITION_REPLAN),
        _ => Err(ApiError::bad(
            "不支持的里程碑拒绝处置，请选择 retry 或 replan",
        )),
    }
}

async fn replan_rejected_milestone(
    state: &AppState,
    project: ProjectRow,
    task: TaskRow,
    reason: &str,
    guidance: Option<String>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let mut replan_reason = format!("人工里程碑验收拒绝（任务 {}）：{reason}", task.key);
    if let Some(guidance) = guidance.as_deref() {
        replan_reason.push_str(&format!("；指导意见：{guidance}"));
    }
    let outcome = replan::request(state, &project.id, &replan_reason, REPLAN_SOURCE_HUMAN).await;
    let summary = match outcome {
        Ok(Some(summary)) => summary,
        Ok(None) => {
            restore_milestone_gate(state, &task.id, "项目状态已变化，未执行重新规划").await?;
            return Err(ApiError::bad("项目状态已变化，未执行重新规划"));
        }
        Err(error) => {
            let message = format!("重新规划失败：{error:#}");
            restore_milestone_gate(state, &task.id, &message).await?;
            return Err(ApiError::internal(error));
        }
    };
    sqlx::query(
        "UPDATE tasks SET human_gate = NULL, updated_at = ? WHERE id = ? AND human_gate = ?",
    )
    .bind(now())
    .bind(&task.id)
    .bind(HUMAN_GATE_MILESTONE_REPLANNING)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        "milestone_rejected",
        format!(
            "任务 {} 人工验收被拒绝，要求重新规划：{reason}{}",
            task.key,
            guidance
                .as_deref()
                .map(|value| format!("；指导意见：{value}"))
                .unwrap_or_default()
        ),
    )
    .await;
    tracing::info!(
        project_id = %project.id,
        task_key = %task.key,
        canceled_tasks = summary.canceled_tasks,
        retained_tasks = summary.retained_tasks,
        canceled_jobs = summary.canceled_jobs,
        "里程碑人工验收要求重新规划"
    );
    state.notify_scheduler();
    load_detail(state, &project.id).await.map(Json)
}

async fn task_arbitration(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    let role = arbitration_role(&task);
    let agents = role_agents(&state, role).await?;
    let jobs =
        sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE task_id = ? ORDER BY created_at ASC")
            .bind(&task.id)
            .fetch_all(&state.db)
            .await?;
    let agent_names: HashMap<String, String> = agents
        .iter()
        .map(|agent| (agent.id.clone(), agent.name.clone()))
        .collect();
    Ok(Json(json!({
        "project": {
            "id": project.id,
            "name": project.name,
            "stage": project.stage,
            "status": project.status,
            "blocked_reason": project.blocked_reason,
        },
        "task": {
            "id": task.id,
            "key": task.key,
            "title": task.title,
            "stage": task.stage,
            "status": task.status,
            "last_error": task.last_error,
            "failure_summary": crate::failure::summarize(task.last_error.as_deref()),
            "last_failure_kind": task.last_failure_kind,
            "waiting_reason": task.waiting_reason,
            "attempts": task.attempts,
            "rejections": task.rejections,
            "milestone": task.milestone != 0,
            "human_gate": task.human_gate,
            "forced_agent_id": task.forced_agent_id,
            "forced_agent_role": task.forced_agent_role,
        },
        "role": role,
        "agents": agents.iter().map(|agent| json!({
            "id": agent.id,
            "name": agent.name,
            "kind": agent.kind,
            "enabled": agent.enabled != 0,
            "cooldown_until": agent.cooldown_until,
            "last_error": agent.last_error,
            "failure_kind": agent.failure_kind,
            "failure_summary": crate::failure::summarize(agent.last_error.as_deref()),
            "transient_failures": agent.transient_failures,
        })).collect::<Vec<_>>(),
        "jobs": jobs.iter().map(|job| json!({
            "id": job.id,
            "role": job.role,
            "agent_id": job.agent_id,
            "agent_name": job.agent_id.as_ref().and_then(|id| agent_names.get(id).cloned()),
            "status": job.status,
            "attempt": job.attempt,
            "error": job.error,
            "failure_summary": crate::failure::summarize(job.error.as_deref()),
            "exit_code": job.exit_code,
            "failure_kind": job.failure_kind,
            "input_tokens": job.input_tokens,
            "output_tokens": job.output_tokens,
            "cache_read_tokens": job.cache_read_tokens,
            "cache_write_tokens": job.cache_write_tokens,
            "reasoning_tokens": job.reasoning_tokens,
            "cost": job.cost,
            "duration_ms": job.duration_ms,
            "usage_known": job.usage_known != 0,
            "started_at": job.started_at,
            "finished_at": job.finished_at,
            "created_at": job.created_at,
        })).collect::<Vec<_>>(),
        "actions": [ARBITRATION_RETRY, ARBITRATION_SWITCH_AGENT, ARBITRATION_GUIDE],
    })))
}

async fn submit_arbitration(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<ArbitrationRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let action = request.action.trim().to_string();
    if !matches!(
        action.as_str(),
        ARBITRATION_RETRY | ARBITRATION_SWITCH_AGENT | ARBITRATION_GUIDE
    ) {
        return Err(ApiError::bad("action 必须是 retry、switch_agent 或 guide"));
    }
    let guidance = request
        .guidance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if action == ARBITRATION_GUIDE && guidance.is_none() {
        return Err(ApiError::bad("指导内容不能为空"));
    }
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    if task.stage == TASK_STAGE_COMPLETED {
        return Err(ApiError::bad("任务已完成"));
    }
    if task.status.as_deref() != Some(STATUS_BLOCKED) {
        return Err(ApiError::bad("只有阻塞中的任务可以裁决"));
    }
    if task.human_gate.as_deref() == Some(HUMAN_GATE_MILESTONE) {
        return Err(ApiError::bad("任务正在等待人工验收，请使用人工验收操作"));
    }
    let role = arbitration_role(&task);
    let mut forced_agent_id: Option<String> = None;
    let mut forced_agent_role: Option<String> = None;
    let mut event_kind = "arbitration_retry";
    let mut event_message = format!("人工裁决：重试任务 {}", task.key);
    match action.as_str() {
        ARBITRATION_SWITCH_AGENT => {
            let agent_id = request
                .agent_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ApiError::bad("更换 Agent 时必须指定 agent_id"))?;
            let Some(agent) = crate::db::load_agent(&state.db, agent_id).await? else {
                return Err(ApiError::not_found("Agent 不存在"));
            };
            if !parse_list(&agent.roles).iter().any(|item| item == role) {
                return Err(ApiError::bad(format!(
                    "Agent {} 不能承担{}角色",
                    agent.name,
                    role_label(role)
                )));
            }
            forced_agent_id = Some(agent.id.clone());
            forced_agent_role = Some(role.to_string());
            event_kind = "arbitration_switch_agent";
            event_message = format!(
                "人工裁决：任务 {} 下一次{}作业强制使用 {}",
                task.key,
                role_label(role),
                agent.name
            );
        }
        ARBITRATION_GUIDE => {
            let text = guidance.clone().unwrap_or_default();
            event_kind = "arbitration_guide";
            event_message = format!("人工裁决：任务 {} 收到指导并重新排队：{text}", task.key);
        }
        _ => {}
    }
    let reset_counts = matches!(action.as_str(), ARBITRATION_RETRY | ARBITRATION_GUIDE);
    let next_guidance = guidance.unwrap_or_else(|| task.guidance.clone());
    let paused = matches!(
        project.status.as_deref(),
        Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
    );
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE tasks SET status = ?, attempts = ?, rejections = ?, agent_id = NULL, \
         last_agent_id = CASE WHEN ? THEN NULL ELSE last_agent_id END, \
         avoid_agent_id = CASE WHEN ? THEN NULL ELSE avoid_agent_id END, \
         avoid_agent_role = CASE WHEN ? THEN NULL ELSE avoid_agent_role END, \
         last_error = NULL, \
         last_failure_kind = NULL, waiting_reason = NULL, human_gate = NULL, guidance = ?, \
         forced_agent_id = ?, forced_agent_role = ?, \
         updated_at = ? WHERE id = ? AND status = ?",
    )
    .bind(STATUS_QUEUED)
    .bind(if reset_counts { 0 } else { task.attempts })
    .bind(if reset_counts { 0 } else { task.rejections })
    .bind(reset_counts)
    .bind(reset_counts)
    .bind(reset_counts)
    .bind(&next_guidance)
    .bind(&forced_agent_id)
    .bind(&forced_agent_role)
    .bind(now())
    .bind(&task.id)
    .bind(STATUS_BLOCKED)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE projects SET status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = CASE WHEN ? THEN ? ELSE NULL END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(STATUS_RUNNING)
    .bind(paused)
    .bind("人工裁决已记录，等待项目恢复后生效")
    .bind(now())
    .bind(&project_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    clear_project_agent_backoff(&state, &project_id).await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        event_kind,
        event_message,
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

/// Human retry overrides automatic backoff for every agent in the project pool.
async fn clear_project_agent_backoff(state: &AppState, project_id: &str) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE agents SET cooldown_until = NULL, failure_kind = NULL, transient_failures = 0, \
         updated_at = ? WHERE id IN (SELECT agent_id FROM project_agents WHERE project_id = ?)",
    )
    .bind(now())
    .bind(project_id)
    .execute(&state.db)
    .await?;
    Ok(())
}

async fn load_project_task(
    state: &AppState,
    project_id: &str,
    task_id: &str,
) -> Result<TaskRow, ApiError> {
    sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ? AND project_id = ?")
        .bind(task_id)
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("任务不存在"))
}

async fn role_agents(state: &AppState, role: &str) -> Result<Vec<AgentRow>, ApiError> {
    let rows =
        sqlx::query_as::<_, AgentRow>("SELECT * FROM agents ORDER BY created_at ASC, id ASC")
            .fetch_all(&state.db)
            .await?;
    Ok(rows
        .into_iter()
        .filter(|agent| parse_list(&agent.roles).iter().any(|item| item == role))
        .collect())
}

fn arbitration_role(task: &TaskRow) -> &'static str {
    if task.stage == TASK_STAGE_VERIFYING {
        ROLE_VERIFIER
    } else {
        ROLE_EXECUTOR
    }
}

fn role_label(role: &str) -> &'static str {
    match role {
        ROLE_EXECUTOR => "执行",
        ROLE_VERIFIER => "验收",
        ROLE_PLANNER => "规划",
        _ => "未知",
    }
}

fn stage_label(stage: &str) -> &'static str {
    match stage {
        STAGE_ANALYSIS => "项目分析",
        STAGE_EXECUTING => "执行",
        STAGE_SUMMARIZING => "总结",
        STAGE_COMPLETED => "完成",
        _ => "未知",
    }
}

fn parse_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn append_issue(existing: &str, issue: &str) -> String {
    let mut issues: Vec<String> = parse_list(existing);
    if !issues.iter().any(|item| item == issue) {
        issues.push(issue.to_string());
    }
    while issues.len() > 20 {
        issues.remove(0);
    }
    serde_json::to_string(&issues).unwrap_or_else(|_| "[]".into())
}

async fn retry_task(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    let Some(task) =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ? AND project_id = ?")
            .bind(&task_id)
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await?
    else {
        return Err(ApiError::not_found("任务不存在"));
    };
    if task.stage == TASK_STAGE_COMPLETED {
        return Err(ApiError::bad("任务已完成"));
    }
    if task.status.as_deref() != Some(STATUS_BLOCKED) {
        return Err(ApiError::bad("只有阻塞中的任务可以重试"));
    }
    {
        let _guard = state.schedule_lock.lock().await;
        sqlx::query(
            "UPDATE tasks SET status = ?, attempts = 0, rejections = 0, agent_id = NULL, \
             last_agent_id = NULL, avoid_agent_id = NULL, avoid_agent_role = NULL, \
             last_error = NULL, last_failure_kind = NULL, \
             waiting_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(STATUS_QUEUED)
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
        clear_project_agent_backoff(&state, &project_id).await?;
        sqlx::query(
            "UPDATE projects SET status = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(STATUS_RUNNING)
        .bind(now())
        .bind(&project_id)
        .execute(&state.db)
        .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "task_retry",
        format!("人工要求重试任务 {}", task.key),
    )
    .await;
    state.notify_scheduler();
    Ok(Json(json!({"ok": true})))
}

async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<JobView>, ApiError> {
    let job = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("作业不存在"))?;
    let agent_name = match &job.agent_id {
        Some(agent_id) => crate::db::load_agent(&state.db, agent_id)
            .await?
            .map(|agent| agent.name),
        None => None,
    };
    let pid = match job.pid {
        Some(pid) => Some(pid),
        None => job_pid_file(&state, &job).await,
    };
    let failure_summary = crate::failure::summarize(job.error.as_deref());
    Ok(Json(JobView {
        id: job.id,
        project_id: job.project_id,
        task_id: job.task_id,
        role: job.role,
        agent_id: job.agent_id,
        agent_name,
        status: job.status,
        attempt: job.attempt,
        prompt: job.prompt,
        result: job.result,
        stdout: job.stdout,
        stderr: job.stderr,
        error: job.error,
        exit_code: job.exit_code,
        pid,
        failure_kind: job.failure_kind,
        failure_summary,
        input_tokens: job.input_tokens,
        output_tokens: job.output_tokens,
        cache_read_tokens: job.cache_read_tokens,
        cache_write_tokens: job.cache_write_tokens,
        reasoning_tokens: job.reasoning_tokens,
        cost: job.cost,
        duration_ms: job
            .duration_ms
            .or_else(|| job_duration(&job.started_at, &job.finished_at)),
        usage_known: job.usage_known != 0,
        started_at: job.started_at,
        finished_at: job.finished_at,
        created_at: job.created_at,
    }))
}

async fn job_pid_file(state: &AppState, job: &JobRow) -> Option<i64> {
    let project = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
        .bind(&job.project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()?;
    let path = PathBuf::from(project.workspace)
        .join("jobs")
        .join(&job.id)
        .join("pid");
    let raw = tokio::fs::read_to_string(path).await.ok()?;
    raw.trim().split(':').next()?.parse::<i64>().ok()
}

#[derive(Debug, serde::Serialize)]
struct JobOutput {
    stdout: String,
    stderr: String,
    pid: Option<i64>,
    running: bool,
}

async fn get_job_output(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<JobOutput>, ApiError> {
    let job = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("作业不存在"))?;
    let project = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
        .bind(&job.project_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("项目不存在"))?;
    let dir = PathBuf::from(project.workspace).join("jobs").join(&job.id);
    let stdout = match tokio::fs::read_to_string(dir.join("stdout.log")).await {
        Ok(text) => text,
        Err(_) => job.stdout.clone().unwrap_or_default(),
    };
    let stderr = match tokio::fs::read_to_string(dir.join("stderr.log")).await {
        Ok(text) => text,
        Err(_) => job.stderr.clone().unwrap_or_default(),
    };
    let pid = match job.pid {
        Some(pid) => Some(pid),
        None => job_pid_file(&state, &job).await,
    };
    Ok(Json(JobOutput {
        stdout,
        stderr,
        pid,
        running: job.status == JOB_STATUS_RUNNING,
    }))
}

#[derive(Debug, Deserialize)]
struct UsageQuery {
    project_id: Option<String>,
    agent_id: Option<String>,
    role: Option<String>,
    #[serde(alias = "start")]
    from: Option<String>,
    #[serde(alias = "end")]
    to: Option<String>,
    group_by: Option<String>,
}

async fn usage_report(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
) -> Result<Json<usage::UsageReport>, ApiError> {
    let group_by = usage::UsageGroupBy::parse(query.group_by.as_deref()).map_err(ApiError::bad)?;
    let from = parse_usage_time(query.from.as_deref())?;
    let to = parse_usage_time(query.to.as_deref())?;
    let filter = usage::UsageFilter {
        project_id: query.project_id.clone(),
        agent_id: query.agent_id.clone(),
        role: query.role.clone(),
        from,
        to,
    };
    let rows = usage::load_jobs(&state.db, query.project_id.as_deref()).await?;
    let agent_rows = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
        .fetch_all(&state.db)
        .await?;
    let names: HashMap<String, String> = agent_rows
        .into_iter()
        .map(|agent| (agent.id, agent.name))
        .collect();
    Ok(Json(usage::summarize(&rows, &filter, group_by, &names)))
}

fn parse_usage_time(raw: Option<&str>) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|value| Some(value.with_timezone(&chrono::Utc)))
        .map_err(|_| ApiError::bad(format!("时间格式必须为 RFC3339: {raw}")))
}

#[derive(Debug, Deserialize)]
struct EventQuery {
    project_id: Option<String>,
    after: Option<i64>,
    limit: Option<i64>,
}

async fn list_events(
    State(state): State<AppState>,
    Query(query): Query<EventQuery>,
) -> Result<Json<Vec<EventRow>>, ApiError> {
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let rows = if let Some(project_id) = &query.project_id {
        sqlx::query_as::<_, EventRow>(
            "SELECT * FROM events WHERE project_id = ? AND id > ? ORDER BY id ASC LIMIT ?",
        )
        .bind(project_id)
        .bind(query.after.unwrap_or(0))
        .bind(limit)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, EventRow>("SELECT * FROM events WHERE id > ? ORDER BY id ASC LIMIT ?")
            .bind(query.after.unwrap_or(0))
            .bind(limit)
            .fetch_all(&state.db)
            .await?
    };
    Ok(Json(rows))
}

async fn stream_events(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.bus.subscribe();
    let events = BroadcastStream::new(receiver).filter_map(|message| match message {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
            Some(Ok(Event::default().data(data)))
        }
        Err(_) => None,
    });
    // The broadcast bus outlives every connection, so the response only ends
    // when shutdown is requested. Without this the graceful drain would wait
    // for an event stream that can never finish.
    let shutdown = WatchStream::new(state.shutdown_receiver())
        .filter(|stopping| *stopping)
        .map(|_| None::<Result<Event, Infallible>>);
    let stream = events
        .map(Some)
        .merge(shutdown)
        .take_while(Option::is_some)
        .map(|item| item.expect("event stream item passes the shutdown guard"));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn load_detail(state: &AppState, id: &str) -> Result<ProjectDetail, ApiError> {
    let project = require_project(state, id).await?;
    let tasks = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM tasks WHERE project_id = ? ORDER BY queue_order ASC, seq ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let messages = sqlx::query_as::<_, MessageRow>(
        "SELECT * FROM messages WHERE project_id = ? ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let mut events = sqlx::query_as::<_, EventRow>(
        "SELECT * FROM events WHERE project_id = ? ORDER BY id DESC LIMIT 500",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    events.reverse();
    let agent_rows =
        sqlx::query_as::<_, AgentRow>("SELECT * FROM agents ORDER BY created_at ASC, id ASC")
            .fetch_all(&state.db)
            .await?;
    let mut agents: Vec<AgentView> = Vec::new();
    for row in agent_rows {
        agents.push(agent_view(state, row).await?);
    }
    let agent_pool = crate::db::load_project_agent_pool(&state.db, id).await?;
    let agent_names: HashMap<String, String> = agents
        .iter()
        .map(|agent| (agent.id.clone(), agent.name.clone()))
        .collect();
    let usage_rows = usage::load_jobs(&state.db, Some(id)).await?;
    let project_usage = usage::project_usage(&usage_rows, id, &agent_names);
    let job_rows = sqlx::query_as::<_, JobRow>(
        "SELECT * FROM jobs WHERE project_id = ? ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let jobs = job_rows
        .into_iter()
        .map(|job| JobSummary {
            id: job.id,
            project_id: job.project_id,
            task_id: job.task_id,
            role: job.role,
            agent_id: job.agent_id.clone(),
            agent_name: job
                .agent_id
                .as_ref()
                .and_then(|agent_id| agent_names.get(agent_id).cloned()),
            status: job.status,
            attempt: job.attempt,
            failure_summary: crate::failure::summarize(job.error.as_deref()),
            error: job.error,
            exit_code: job.exit_code,
            failure_kind: job.failure_kind,
            input_tokens: job.input_tokens,
            output_tokens: job.output_tokens,
            cache_read_tokens: job.cache_read_tokens,
            cache_write_tokens: job.cache_write_tokens,
            reasoning_tokens: job.reasoning_tokens,
            cost: job.cost,
            duration_ms: job
                .duration_ms
                .or_else(|| job_duration(&job.started_at, &job.finished_at)),
            usage_known: job.usage_known != 0,
            started_at: job.started_at,
            finished_at: job.finished_at,
            created_at: job.created_at,
        })
        .collect();
    let merged_at = project.merged_at.clone();
    let merged_commit = project.merged_commit.clone();
    let merged_into_base = project_merged_into_base(&project).await;
    let proposal = project
        .proposal
        .as_deref()
        .and_then(|raw| serde_json::from_str::<AnalysisProposal>(raw).ok());
    Ok(ProjectDetail {
        project,
        merged_at: merged_at.clone(),
        merged_commit: merged_commit.clone(),
        merged_into_base,
        tasks: task_views(tasks),
        jobs,
        messages,
        events,
        agents,
        agent_pool,
        usage: project_usage,
        proposal,
    })
}

/// 目标分支是否已经包含项目分支提交。比较当前分支提交，覆盖 DOMO 记录的
/// 合入和用户在 DOMO 之外完成的合入；分支不可读时退回已记录的合入状态。
async fn project_merged_into_base(project: &ProjectRow) -> bool {
    if project.stage != STAGE_COMPLETED {
        return false;
    }
    let repo = PathBuf::from(&project.repo_path);
    let Ok(project_commit) = git::commit_of(&repo, &project.branch).await else {
        return project.merged_at.is_some();
    };
    let Ok(base_commit) = git::commit_of(&repo, &project.base_branch).await else {
        return project.merged_at.is_some();
    };
    git::is_ancestor(&repo, &project_commit, &base_commit)
        .await
        .unwrap_or(false)
}

fn job_duration(started_at: &Option<String>, finished_at: &Option<String>) -> Option<i64> {
    usage::elapsed_ms(started_at.as_deref(), finished_at.as_deref()?)
}

/// Queue positions rank ready, queued tasks by the stable queue order. Paused
/// and not-yet-ready tasks are excluded from the dispatch ordering.
fn task_views(rows: Vec<TaskRow>) -> Vec<TaskView> {
    let completed: HashSet<String> = rows
        .iter()
        .filter(|task| task.stage == TASK_STAGE_COMPLETED)
        .map(|task| task.key.clone())
        .collect();
    let mut ready: Vec<&TaskRow> = rows
        .iter()
        .filter(|task| {
            task.status.as_deref() == Some(STATUS_QUEUED)
                && task.stage != TASK_STAGE_COMPLETED
                && (task.stage != TASK_STAGE_PENDING
                    || deps_complete(&parse_list(&task.deps), &completed))
        })
        .collect();
    ready.sort_by_key(|task| (task.queue_order, task.seq));
    let positions: HashMap<String, i64> = ready
        .iter()
        .enumerate()
        .map(|(index, task)| (task.id.clone(), (index + 1) as i64))
        .collect();
    rows.into_iter()
        .map(|row| {
            let position = positions.get(&row.id).copied();
            let mut view = TaskView::from_row(row);
            view.queue_position = position;
            view.waiting_reason = task_waiting_reason(&view, &completed);
            view
        })
        .collect()
}

fn task_waiting_reason(task: &TaskView, completed: &HashSet<String>) -> Option<String> {
    match task.status.as_deref() {
        Some(STATUS_CANCELED) => return Some("任务已取消".into()),
        Some(STATUS_PAUSED) => return Some("任务已暂停".into()),
        Some(STATUS_PAUSING) => return Some("暂停中，等待在途作业结束".into()),
        Some(STATUS_BLOCKED) => {
            return Some(
                if task.human_gate.as_deref() == Some(HUMAN_GATE_MILESTONE) {
                    "等待人工验收".into()
                } else {
                    "等待人工裁决".into()
                },
            )
        }
        _ => {}
    }
    if task.stage == TASK_STAGE_COMPLETED {
        return None;
    }
    if task.stage == TASK_STAGE_PENDING {
        let missing: Vec<String> = task
            .deps
            .iter()
            .filter(|dep| !completed.contains(*dep))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Some(format!("等待前置任务：{}", missing.join("、")));
        }
    }
    if task.queue_position.is_some() {
        return task
            .waiting_reason
            .clone()
            .or_else(|| Some("等待派发".into()));
    }
    task.waiting_reason.clone()
}

async fn require_project(state: &AppState, id: &str) -> Result<ProjectRow, ApiError> {
    sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("项目不存在"))
}

fn validate_kind(kind: &str) -> Result<String, ApiError> {
    match kind {
        AGENT_MOCK | AGENT_OPENCODE | AGENT_CLI => Ok(kind.to_string()),
        _ => Err(ApiError::bad("Agent 类型必须是 mock、opencode 或 cli")),
    }
}

fn validate_role(role: &str) -> Result<String, ApiError> {
    match role {
        ROLE_PLANNER | ROLE_EXECUTOR | ROLE_VERIFIER => Ok(role.to_string()),
        _ => Err(ApiError::bad(format!("未知角色: {role}"))),
    }
}

fn validate_roles(roles: Vec<String>) -> Result<Vec<String>, ApiError> {
    let allowed: HashSet<&str> = [ROLE_PLANNER, ROLE_EXECUTOR, ROLE_VERIFIER]
        .into_iter()
        .collect();
    let mut result = Vec::new();
    for role in roles {
        if !allowed.contains(role.as_str()) {
            return Err(ApiError::bad(format!("未知角色: {role}")));
        }
        if !result.contains(&role) {
            result.push(role);
        }
    }
    if result.is_empty() {
        return Err(ApiError::bad("Agent 至少需要一个角色"));
    }
    Ok(result)
}
