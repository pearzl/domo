//! HTTP API and server-sent events.

mod agents;
mod archive;
mod detail;
mod helpers;
mod jobs;
mod merge;
mod milestones;
mod projects;
mod reports;
mod tasks;
#[cfg(test)]
mod tests;
mod verification;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, patch, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::models::ErrorBody;
use crate::state::AppState;
use crate::version;

use crate::agents::AgentError;
use crate::artifacts::ArtifactsError;
use crate::db::DbError;
use crate::git::GitError;
use crate::opencode::OpencodeError;
use crate::platform::PlatformError;
use crate::usage::UsageError;

use self::agents::{
    create_agent, delete_agent, get_default_team, list_agents, reset_agent_usage, update_agent,
    update_default_team_agent,
};
use self::archive::{archive_preview, archive_project, project_archive_refs};
use self::jobs::{
    get_event, get_job, get_job_activity, get_job_output, list_events, retry_job, stream_events,
    usage_report,
};
use self::merge::merge_project;
use self::milestones::{
    accept_milestone, reject_milestone, save_task_guidance, set_task_milestone,
};
use self::projects::{
    create_project, get_project, get_project_agents, get_task_agent_pool, list_projects,
    replan_project, set_task_agents, update_project_agent, update_task_agent,
};
use self::reports::submit_report;
use self::tasks::{
    confirm_analysis, create_task, pause_project, pause_task, post_analysis_message,
    reorder_task_queue, resume_project, resume_task, update_analysis_policy,
};
use self::verification::{retry_task, submit_arbitration, task_arbitration, task_verifications};

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
    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
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

impl From<DbError> for ApiError {
    fn from(error: DbError) -> Self {
        Self::internal(error.into())
    }
}

impl From<UsageError> for ApiError {
    fn from(error: UsageError) -> Self {
        Self::internal(error.into())
    }
}

impl From<GitError> for ApiError {
    fn from(error: GitError) -> Self {
        Self::internal(error.into())
    }
}

impl From<PlatformError> for ApiError {
    fn from(error: PlatformError) -> Self {
        Self::internal(error.into())
    }
}

impl From<OpencodeError> for ApiError {
    fn from(error: OpencodeError) -> Self {
        Self::internal(error.into())
    }
}

impl From<AgentError> for ApiError {
    fn from(error: AgentError) -> Self {
        Self::internal(error.into())
    }
}

impl From<ArtifactsError> for ApiError {
    fn from(error: ArtifactsError) -> Self {
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
        .route("/api/team/default", get(get_default_team))
        .route(
            "/api/team/default/{role}/{agent_id}",
            patch(update_default_team_agent),
        )
        .route("/api/usage", get(usage_report))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/{id}", get(get_project))
        .route("/api/projects/{id}/agents", get(get_project_agents))
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
        .route(
            "/api/projects/{id}/archive",
            get(archive_preview).post(archive_project),
        )
        .route("/api/projects/{id}/archive-refs", get(project_archive_refs))
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
            get(get_task_agent_pool).patch(set_task_agents),
        )
        .route(
            "/api/projects/{id}/tasks/{task_id}/agents/{role}/{agent_id}",
            patch(update_task_agent),
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
        .route(
            "/api/projects/{id}/tasks/{task_id}/verifications",
            get(task_verifications),
        )
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/jobs/{id}/retry", post(retry_job))
        .route("/api/jobs/{id}/output", get(get_job_output))
        .route("/api/jobs/{id}/activity", get(get_job_activity))
        .route("/api/events", get(list_events))
        .route("/api/events/stream", get(stream_events))
        .route("/api/events/{id}", get(get_event))
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
