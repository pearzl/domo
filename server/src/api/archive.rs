//! 项目归档与归档引用查询。

use std::path::PathBuf;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agents::kill_job_dir;
use crate::domain::now;
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::paths;
use crate::state::AppState;

use super::helpers::require_project;
use super::ApiError;

#[derive(Debug, Default, Deserialize)]
pub(super) struct ArchiveQuery {
    force: Option<bool>,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct DirtyWorktree {
    task_id: Option<String>,
    key: Option<String>,
    title: Option<String>,
    label: String,
    path: String,
}

/// Worktrees that would survive archiving because they still hold uncommitted
/// changes. The archive endpoint asks for confirmation before accepting them.
pub(super) async fn dirty_worktrees(
    state: &AppState,
    project: &ProjectRow,
) -> Result<Vec<DirtyWorktree>, ApiError> {
    if !project.is_git() {
        return Ok(Vec::new());
    }
    let _guard = state.git_lock.lock().await;
    let mut dirty = Vec::new();
    let project_worktree = paths::project_repo(&PathBuf::from(&project.workspace));
    if project_worktree.exists() {
        let project_dirty = match git::status_porcelain(&project_worktree).await {
            Ok(output) => !output.trim().is_empty(),
            Err(error) => {
                tracing::warn!(
                    path = %project_worktree.display(),
                    error = %format!("{error:#}"),
                    "归档检查无法读取项目工作区状态"
                );
                true
            }
        };
        if project_dirty {
            dirty.push(DirtyWorktree {
                task_id: None,
                key: None,
                title: None,
                label: "项目工作区".to_string(),
                path: project_worktree.to_string_lossy().to_string(),
            });
        }
    }
    let tasks =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? ORDER BY seq ASC")
            .bind(&project.id)
            .fetch_all(&state.db)
            .await?;
    for task in tasks {
        let Some(path) = task.worktree else {
            continue;
        };
        let path = PathBuf::from(path);
        if !path.exists() {
            continue;
        }
        let dirty_worktree = match git::status_porcelain(&path).await {
            Ok(output) => !output.trim().is_empty(),
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %format!("{error:#}"),
                    "归档检查无法读取任务工作区状态"
                );
                true
            }
        };
        if dirty_worktree {
            let label = format!("{} {}", task.key, task.title);
            dirty.push(DirtyWorktree {
                task_id: Some(task.id),
                key: Some(task.key),
                title: Some(task.title),
                label,
                path: path.to_string_lossy().to_string(),
            });
        }
    }
    let jobs: Vec<String> =
        sqlx::query_scalar("SELECT id FROM jobs WHERE project_id = ? AND role = 'merge_conflict'")
            .bind(&project.id)
            .fetch_all(&state.db)
            .await?;
    for job in jobs {
        let path = paths::job_dir(&PathBuf::from(&project.workspace), &job).join("resolution");
        if path.exists()
            && git::status_porcelain(&path)
                .await
                .map(|s| !s.trim().is_empty())
                .unwrap_or(true)
        {
            dirty.push(DirtyWorktree {
                task_id: None,
                key: None,
                title: None,
                label: format!("合入冲突处理现场 {job}"),
                path: path.to_string_lossy().into_owned(),
            });
        }
    }
    Ok(dirty)
}

pub(super) async fn archive_preview(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    let dirty = dirty_worktrees(&state, &project).await?;
    Ok(Json(json!({ "dirty": dirty })))
}

pub(super) async fn archive_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ArchiveQuery>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    if !query.force.unwrap_or(false) {
        let dirty = dirty_worktrees(&state, &project).await?;
        if !dirty.is_empty() {
            let labels = dirty
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>()
                .join("、");
            return Err(ApiError::conflict(format!(
                "以下工作区有未提交改动：{labels}。归档会保留这些工作区，项目工作区可能无法完全清理。确认后请携带 force=true 重新归档。"
            )));
        }
    }
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
            let dir = paths::job_dir(&PathBuf::from(&project.workspace), &job.id);
            kill_job_dir(&dir);
        }
    }
    if let Err(error) = crate::artifacts::cleanup_project(&state, &project, true).await {
        tracing::warn!(
            project = %id,
            error = %format!("{error:#}"),
            "清理归档项目工作区失败"
        );
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

pub(super) async fn project_archive_refs(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    if !project.is_git() {
        return Ok(Json(json!({
            "project": Value::Null,
            "tasks": [],
            "backup_command": Value::Null,
        })));
    }
    let repo = PathBuf::from(&project.repo_path);
    let project_reference = git::archive_ref(&project.branch);
    let project_recovery = format!("git branch {} {}", project.branch, project_reference);
    let project_log = format!("git log --oneline {project_reference}");
    let project_view = match git::commit_of(&repo, &project_reference).await {
        Ok(tip) => json!({
            "branch": project.branch,
            "reference": project_reference,
            "tip": tip,
            "recovery": project_recovery,
            "log_command": project_log,
        }),
        Err(_) => Value::Null,
    };
    let tasks = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM tasks WHERE project_id = ? AND branch IS NOT NULL ORDER BY seq ASC",
    )
    .bind(&project.id)
    .fetch_all(&state.db)
    .await?;
    let mut archived_tasks = Vec::new();
    for task in tasks {
        let Some(branch) = task.branch else {
            continue;
        };
        let reference = git::archive_ref(&branch);
        let Ok(tip) = git::commit_of(&repo, &reference).await else {
            continue;
        };
        let recovery = format!("git branch {branch} {reference}");
        let log_command = format!("git log --oneline {reference}");
        archived_tasks.push(json!({
            "task_id": task.id,
            "key": task.key,
            "title": task.title,
            "released_at": task.artifacts_released_at,
            "branch": branch,
            "reference": reference,
            "tip": tip,
            "recovery": recovery,
            "log_command": log_command,
        }));
    }
    Ok(Json(json!({
        "project": project_view,
        "tasks": archived_tasks,
        "backup_command": "git bundle create domo-archive.bundle --all",
    })))
}
