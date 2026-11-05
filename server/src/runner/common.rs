use anyhow::Result;
use sqlx::SqlitePool;

use crate::domain::{self, now, ParsedResult};
use crate::events::emit;
use crate::models::*;
use crate::state::AppState;

/// A project-level failure result is a valid protocol message carrying a
/// provider or task reason. It must not be relabeled as a schema violation.
pub(super) fn result_is_failure(role: &str, raw: &str) -> bool {
    if !matches!(
        role,
        JOB_ROLE_ANALYSIS | JOB_ROLE_DIRECT | JOB_ROLE_ACCEPTANCE | JOB_ROLE_SUMMARY
    ) {
        return false;
    }
    domain::extract_json(raw)
        .and_then(|value| {
            value
                .get("reason")
                .and_then(|reason| reason.as_str())
                .map(|reason| !reason.trim().is_empty())
        })
        .unwrap_or(false)
}

/// Project analysis runs again without a new user message after a replan
/// request, a manual retry, or an agent failure. Otherwise the planner is
/// waiting for the user to answer its questions or confirm the proposal.
pub(crate) async fn analysis_needs_dispatch(
    pool: &SqlitePool,
    project: &ProjectRow,
) -> Result<bool> {
    if project.retry_role.as_deref() == Some(ROLE_PLANNER) && project.retry_agent_id.is_some() {
        return Ok(true);
    }
    if crate::replan::pending(pool, project).await? {
        return Ok(true);
    }
    let last_role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM messages WHERE project_id = ? ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(&project.id)
    .fetch_optional(pool)
    .await?;
    if last_role.as_deref() == Some("user") {
        return Ok(true);
    }
    let last_analysis: Option<String> = sqlx::query_scalar(
        "SELECT failure_kind FROM jobs WHERE project_id = ? AND task_id IS NULL AND role = ? \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(&project.id)
    .bind(JOB_ROLE_ANALYSIS)
    .fetch_optional(pool)
    .await?;
    // Agent failures are retried by policy. A task-level failure means the
    // request itself was rejected, so the planner stays waiting for the user.
    let retryable = [FAILURE_RATE_LIMIT, FAILURE_TRANSIENT, FAILURE_PERMANENT];
    Ok(last_analysis
        .as_deref()
        .is_some_and(|kind| retryable.contains(&kind)))
}

pub(super) async fn has_active_tasks(pool: &SqlitePool, project_id: &str) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage != ? \
         AND COALESCE(status, '') != ?",
    )
    .bind(project_id)
    .bind(TASK_STAGE_COMPLETED)
    .bind(STATUS_CANCELED)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

/// A project-phase result is stale when the project already carries active
/// tasks, for example after tasks were added by hand while the job was in
/// flight. The project returns to the executing stage and the stale result only
/// remains in the audit log.
pub(super) async fn apply_stale_result(
    state: &AppState,
    project: &ProjectRow,
    kind: &str,
    message: String,
) -> Result<()> {
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN \
         ('pausing', 'paused', 'archived') THEN status ELSE ? END, blocked_reason = NULL, \
         report = NULL, report_feedback = NULL, acceptance = NULL, acceptance_passed = 0, \
         acceptance_round = 0, acceptance_at = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_EXECUTING)
    .bind(STATUS_RUNNING)
    .bind(now())
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        kind,
        message,
    )
    .await;
    Ok(())
}

pub(crate) async fn insert_message(
    pool: &SqlitePool,
    project_id: &str,
    role: &str,
    content: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO messages (id, project_id, role, content, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(domain::new_id("msg"))
    .bind(project_id)
    .bind(role)
    .bind(content)
    .bind(now())
    .execute(pool)
    .await?;
    Ok(())
}

pub(super) fn merge_issues(existing: &str, issues: &[String]) -> String {
    let mut merged: Vec<String> = parse_json_list(existing);
    for issue in issues {
        if !merged.iter().any(|item| item == issue) {
            merged.push(issue.clone());
        }
    }
    while merged.len() > 20 {
        merged.remove(0);
    }
    serde_json::to_string(&merged).unwrap_or_else(|_| "[]".into())
}

pub(super) fn parse_json_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

pub(super) fn models_list_json(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".into())
}

pub(super) fn parsed_error(parsed: &ParsedResult) -> Option<String> {
    match parsed {
        ParsedResult::ExecutorFailed { reason } => Some(reason.clone()),
        _ => None,
    }
}

pub(super) fn role_label(role: &str) -> &'static str {
    match role {
        JOB_ROLE_ANALYSIS => "项目分析",
        JOB_ROLE_DIRECT => "直接完成",
        JOB_ROLE_EXECUTOR => "执行",
        JOB_ROLE_VERIFIER => "验收",
        JOB_ROLE_ACCEPTANCE => "项目验收",
        JOB_ROLE_SUMMARY => "总结",
        JOB_ROLE_REPLAN_REVIEW => "重规划审批",
        _ => "未知",
    }
}

pub(super) async fn load_job(pool: &SqlitePool, id: &str) -> Result<Option<JobRow>> {
    Ok(
        sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

pub(super) async fn load_project(pool: &SqlitePool, id: &str) -> Result<Option<ProjectRow>> {
    Ok(
        sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

pub(super) async fn load_task(pool: &SqlitePool, id: &str) -> Result<Option<TaskRow>> {
    Ok(
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}
