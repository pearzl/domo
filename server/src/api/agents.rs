//! Agent 配置、默认团队与用量相关的 HTTP handler。
use std::collections::HashSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use crate::agents::{self};
use crate::domain::{self, now};
use crate::events::emit;
use crate::models::*;
use crate::state::AppState;
use crate::usage;

use super::helpers::{plan_role_reorder, role_label};
use super::ApiError;

pub(super) async fn list_agents(
    State(state): State<AppState>,
) -> Result<Json<Vec<AgentView>>, ApiError> {
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

pub(super) async fn agent_view(state: &AppState, row: AgentRow) -> Result<AgentView, ApiError> {
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

pub(super) async fn create_agent(
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
    require_command_if_needed(&kind, &command)?;
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
        "INSERT INTO agents (id, name, kind, model, roles, priority, max_concurrency, \
         allow_priority_fallback, enabled, command_json, model_args_json, availability_json, \
         usage_limit_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(&kind)
    .bind(request.model.unwrap_or_default().trim())
    .bind(serde_json::to_string(&roles).unwrap_or_else(|_| "[]".into()))
    .bind(request.max_concurrency.unwrap_or(1).max(1))
    .bind(i64::from(request.allow_priority_fallback.unwrap_or(false)))
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
    crate::db::ensure_default_team(&state.db).await?;
    if request.add_to_existing_pools {
        let summary = crate::db::add_agent_to_existing_pools(&state.db, &id).await?;
        tracing::info!(
            agent_id = %id,
            project_entries = summary.project_entries,
            task_entries = summary.task_entries,
            "新建 Agent 已同步到现有项目和任务 Agent 池"
        );
        state.notify_scheduler();
    }
    let row = crate::db::load_agent(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Agent 不存在"))?;
    Ok(Json(agent_view(&state, row).await?))
}

pub(super) async fn update_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateAgentRequest>,
) -> Result<Json<AgentView>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
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
    let command = request.command;
    let command_json = match &command {
        Some(value) => serde_json::to_string(value).unwrap_or_else(|_| "[]".into()),
        None => existing.command_json.clone().unwrap_or_else(|| "[]".into()),
    };
    let resolved_command: Vec<String> = serde_json::from_str(&command_json).unwrap_or_default();
    require_command_if_needed(&kind, &resolved_command)?;
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
    let allow_priority_fallback = request
        .allow_priority_fallback
        .map(i64::from)
        .unwrap_or(existing.allow_priority_fallback);
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
         max_concurrency = ?, allow_priority_fallback = ?, enabled = ?, command_json = ?, \
         model_args_json = ?, availability_json = ?, usage_limit_json = ?, \
         cooldown_until = NULL, last_error = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(&name)
    .bind(&kind)
    .bind(model.trim())
    .bind(&roles)
    .bind(max_concurrency)
    .bind(allow_priority_fallback)
    .bind(enabled)
    .bind(&command_json)
    .bind(&model_args)
    .bind(&availability_json)
    .bind(&usage_limit_json)
    .bind(now())
    .bind(&id)
    .execute(&state.db)
    .await?;
    crate::db::ensure_default_team(&state.db).await?;
    crate::delivery::wake_configuration(&state.db, None).await?;
    state.notify_scheduler();
    let row = crate::db::load_agent(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Agent 不存在"))?;
    Ok(Json(agent_view(&state, row).await?))
}

pub(super) async fn get_default_team(
    State(state): State<AppState>,
) -> Result<Json<ProjectAgentPool>, ApiError> {
    Ok(Json(crate::db::load_default_team_pool(&state.db).await?))
}

pub(super) async fn update_default_team_agent(
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
    crate::delivery::wake_configuration(&state.db, None).await?;
    state.notify_scheduler();
    Ok(Json(crate::db::load_default_team_pool(&state.db).await?))
}

/// 把角色内指定 Agent 移动到 0 基目标位置，并在事务内把该角色优先级重排为 0..n-1。
/// 其余角色的条目不参与本次重排。
pub(super) async fn move_default_team_agent_to(
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

pub(super) async fn reset_agent_usage(
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
    crate::delivery::wake_configuration(&state.db, None).await?;
    state.notify_scheduler();
    let row = crate::db::load_agent(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Agent 不存在"))?;
    Ok(Json(agent_view(&state, row).await?))
}

pub(super) async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    sqlx::query("DELETE FROM agents WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await?;
    crate::db::ensure_default_team(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) fn validate_kind(kind: &str) -> Result<String, ApiError> {
    let kinds = agents::kinds();
    if !kinds.contains(&kind) {
        return Err(ApiError::bad(format!(
            "Agent 类型必须是 {}",
            kind_options(kinds)
        )));
    }
    Ok(kind.to_string())
}

/// Registry kinds joined as prose, keeping the last option after "或".
pub(super) fn kind_options(kinds: &[&str]) -> String {
    match kinds.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} 或 {last}", rest.join("、")),
        _ => kinds.join("、"),
    }
}

/// Ask the adapter registry, so a kind that needs a command template declares
/// it in one place instead of being special-cased here.
pub(super) fn require_command_if_needed(kind: &str, command: &[String]) -> Result<(), ApiError> {
    if agents::requires_command_template(kind) && command.is_empty() {
        return Err(ApiError::bad("CLI Agent 必须配置命令模板"));
    }
    agents::validate_config(kind, command).map_err(|error| ApiError::bad(format!("{error:#}")))
}

pub(super) fn validate_role(role: &str) -> Result<String, ApiError> {
    match role {
        ROLE_PLANNER | ROLE_EXECUTOR | ROLE_VERIFIER => Ok(role.to_string()),
        _ => Err(ApiError::bad(format!("未知角色: {role}"))),
    }
}

pub(super) fn validate_roles(roles: Vec<String>) -> Result<Vec<String>, ApiError> {
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
