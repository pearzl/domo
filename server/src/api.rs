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
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::agents::kill_job_dir;
use crate::domain::{self, now, short_id};
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::state::AppState;

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
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/{id}", get(get_project))
        .route("/api/projects/{id}/confirm", post(confirm_goal))
        .route("/api/projects/{id}/pause", post(pause_project))
        .route("/api/projects/{id}/resume", post(resume_project))
        .route("/api/projects/{id}/archive", post(archive_project))
        .route("/api/projects/{id}/retry", post(retry_project))
        .route("/api/projects/{id}/report", post(submit_report))
        .route("/api/projects/{id}/tasks/{task_id}/retry", post(retry_task))
        .route("/api/jobs/{id}", get(get_job))
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
    Json(json!({"ok": true}))
}

async fn list_agents(State(state): State<AppState>) -> Result<Json<Vec<AgentView>>, ApiError> {
    let rows =
        sqlx::query_as::<_, AgentRow>("SELECT * FROM agents ORDER BY priority ASC, created_at ASC")
            .fetch_all(&state.db)
            .await?;
    Ok(Json(rows.into_iter().map(AgentView::from_row).collect()))
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
    let id = domain::new_id("agt");
    let timestamp = now();
    sqlx::query(
        "INSERT INTO agents (id, name, kind, model, roles, priority, max_concurrency, enabled, \
         command_json, model_args_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(&kind)
    .bind(request.model.unwrap_or_default().trim())
    .bind(serde_json::to_string(&roles).unwrap_or_else(|_| "[]".into()))
    .bind(request.priority.unwrap_or(0))
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
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&state.db)
    .await?;
    let row = crate::db::load_agent(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Agent 不存在"))?;
    Ok(Json(AgentView::from_row(row)))
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
    let priority = request.priority.unwrap_or(existing.priority);
    let max_concurrency = request
        .max_concurrency
        .unwrap_or(existing.max_concurrency)
        .max(1);
    let enabled = request
        .enabled
        .map(|value| if value { 1 } else { 0 })
        .unwrap_or(existing.enabled);
    sqlx::query(
        "UPDATE agents SET name = ?, kind = ?, model = ?, roles = ?, priority = ?, \
         max_concurrency = ?, enabled = ?, command_json = ?, model_args_json = ?, \
         cooldown_until = NULL, last_error = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(&name)
    .bind(&kind)
    .bind(model.trim())
    .bind(&roles)
    .bind(priority)
    .bind(max_concurrency)
    .bind(enabled)
    .bind(&command)
    .bind(&model_args)
    .bind(now())
    .bind(&id)
    .execute(&state.db)
    .await?;
    let row = crate::db::load_agent(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Agent 不存在"))?;
    Ok(Json(AgentView::from_row(row)))
}

async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    sqlx::query("DELETE FROM agents WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await?;
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
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE project_id = ?")
            .bind(&row.id)
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
    let goal = request.goal.trim();
    if name.is_empty() {
        return Err(ApiError::bad("项目名称不能为空"));
    }
    if goal.is_empty() {
        return Err(ApiError::bad("项目目标不能为空"));
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
         stage, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&project_id)
    .bind(name)
    .bind(goal)
    .bind(repo.to_string_lossy().to_string())
    .bind(&base)
    .bind(&branch)
    .bind(workspace.to_string_lossy().to_string())
    .bind(STAGE_GOAL_CONFIRM)
    .bind(STATUS_BLOCKED)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&state.db)
    .await;
    if let Err(error) = result {
        let _ = git::remove_worktree(&repo, &repo_worktree).await;
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        return Err(error.into());
    }
    sqlx::query(
        "INSERT INTO messages (id, project_id, role, content, created_at) VALUES (?, ?, 'user', ?, ?)",
    )
    .bind(domain::new_id("msg"))
    .bind(&project_id)
    .bind(goal)
    .bind(&timestamp)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        None,
        "project_created",
        format!("项目创建，等待确认目标（base 分支 {base}）"),
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

async fn confirm_goal(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ConfirmGoalRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.stage != STAGE_GOAL_CONFIRM {
        return Err(ApiError::bad("当前阶段不允许确认目标"));
    }
    if let Some(message) = request
        .message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sqlx::query(
            "INSERT INTO messages (id, project_id, role, content, created_at) VALUES (?, ?, 'user', ?, ?)",
        )
        .bind(domain::new_id("msg"))
        .bind(&id)
        .bind(message)
        .bind(now())
        .execute(&state.db)
        .await?;
    }
    {
        let _guard = state.schedule_lock.lock().await;
        sqlx::query(
            "UPDATE projects SET stage = ?, status = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(STAGE_PLANNING)
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
        "goal_confirmed",
        "目标已确认，开始规划",
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &id).await.map(Json)
}

async fn pause_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    {
        let _guard = state.schedule_lock.lock().await;
        sqlx::query("UPDATE projects SET status = ?, updated_at = ? WHERE id = ?")
            .bind(STATUS_PAUSING)
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
        "project_pausing",
        "项目暂停请求已提交，等待在途作业结束",
    )
    .await;
    state.notify_scheduler();
    Ok(Json(json!({"ok": true})))
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
    let waiting_human = project.stage == STAGE_GOAL_CONFIRM
        || (project.stage == STAGE_SUMMARIZING && project.report.is_some());
    {
        let _guard = state.schedule_lock.lock().await;
        sqlx::query(
            "UPDATE tasks SET status = ?, updated_at = ? \
             WHERE project_id = ? AND stage != ? AND status = ?",
        )
        .bind(STATUS_QUEUED)
        .bind(now())
        .bind(&id)
        .bind(TASK_STAGE_COMPLETED)
        .bind(STATUS_PAUSED)
        .execute(&state.db)
        .await?;
        sqlx::query("UPDATE projects SET status = ?, updated_at = ? WHERE id = ?")
            .bind(if waiting_human {
                STATUS_BLOCKED
            } else {
                STATUS_RUNNING
            })
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
    if project.stage != STAGE_SUMMARIZING || project.report.is_none() {
        return Err(ApiError::bad("当前没有等待验收的项目报告"));
    }
    let event_message: String;
    {
        let _guard = state.schedule_lock.lock().await;
        match request.action.as_str() {
            "accept" => {
                sqlx::query(
                    "UPDATE projects SET stage = ?, status = NULL, blocked_reason = NULL, \
                     report_feedback = NULL, updated_at = ? WHERE id = ?",
                )
                .bind(STAGE_COMPLETED)
                .bind(now())
                .bind(&id)
                .execute(&state.db)
                .await?;
                event_message = "项目报告已通过，项目完成".into();
            }
            "reject" => {
                let feedback = request
                    .feedback
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string();
                if feedback.is_empty() {
                    return Err(ApiError::bad("拒绝报告时必须填写意见"));
                }
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
                event_message = format!("项目报告被拒绝，将重新撰写：{feedback}");
            }
            _ => return Err(ApiError::bad("action 必须是 accept 或 reject")),
        }
    }
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        if request.action == "accept" {
            "project_completed"
        } else {
            "report_rejected"
        },
        event_message,
    )
    .await;
    state.notify_scheduler();
    Ok(Json(json!({"ok": true})))
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
            STAGE_GOAL_CONFIRM | STAGE_PLANNING => {
                sqlx::query(
                    "UPDATE projects SET stage = ?, status = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
                )
                .bind(STAGE_PLANNING)
                .bind(STATUS_RUNNING)
                .bind(now())
                .bind(&id)
                .execute(&state.db)
                .await?;
            }
            STAGE_EXECUTING => {
                sqlx::query(
                    "UPDATE tasks SET status = ?, attempts = 0, rejections = 0, agent_id = NULL, \
                     last_agent_id = NULL, last_error = NULL, updated_at = ? \
                     WHERE project_id = ? AND status = ?",
                )
                .bind(STATUS_QUEUED)
                .bind(now())
                .bind(&id)
                .bind(STATUS_BLOCKED)
                .execute(&state.db)
                .await?;
                sqlx::query(
                    "UPDATE projects SET status = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
                )
                .bind(STATUS_RUNNING)
                .bind(now())
                .bind(&id)
                .execute(&state.db)
                .await?;
            }
            STAGE_SUMMARIZING => {
                if project.report.is_some() {
                    return Err(ApiError::bad("报告正在等待人工验收"));
                }
                sqlx::query(
                    "UPDATE projects SET status = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
                )
                .bind(STATUS_RUNNING)
                .bind(now())
                .bind(&id)
                .execute(&state.db)
                .await?;
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
             last_agent_id = NULL, last_error = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(STATUS_QUEUED)
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
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
        pid: job.pid,
        started_at: job.started_at,
        finished_at: job.finished_at,
        created_at: job.created_at,
    }))
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
    let stream = BroadcastStream::new(receiver).filter_map(|message| match message {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
            Some(Ok(Event::default().data(data)))
        }
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn load_detail(state: &AppState, id: &str) -> Result<ProjectDetail, ApiError> {
    let project = require_project(state, id).await?;
    let tasks =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? ORDER BY seq ASC")
            .bind(id)
            .fetch_all(&state.db)
            .await?;
    let messages = sqlx::query_as::<_, MessageRow>(
        "SELECT * FROM messages WHERE project_id = ? ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let events = sqlx::query_as::<_, EventRow>(
        "SELECT * FROM events WHERE project_id = ? ORDER BY id ASC LIMIT 500",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let agent_rows = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents ORDER BY priority ASC")
        .fetch_all(&state.db)
        .await?;
    let agents: Vec<AgentView> = agent_rows.into_iter().map(AgentView::from_row).collect();
    let agent_names: HashMap<String, String> = agents
        .iter()
        .map(|agent| (agent.id.clone(), agent.name.clone()))
        .collect();
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
            error: job.error,
            exit_code: job.exit_code,
            started_at: job.started_at,
            finished_at: job.finished_at,
            created_at: job.created_at,
        })
        .collect();
    Ok(ProjectDetail {
        project,
        tasks: tasks.into_iter().map(TaskView::from_row).collect(),
        jobs,
        messages,
        events,
        agents,
    })
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
