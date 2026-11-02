//! 项目分支合入与失败回滚。

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use crate::domain::now;
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::paths;
use crate::state::AppState;

use super::detail::load_detail;
use super::helpers::require_project;
use super::ApiError;

/// Rebases the project branch onto its base branch and fast-forwards the base
/// branch. Only completed projects can be merged, and any failure leaves both
/// branch refs at their pre-merge commits.
pub(super) async fn merge_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.stage != STAGE_COMPLETED {
        return Err(ApiError::bad("项目尚未完成，只有完成阶段的项目可以合入"));
    }
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad(
            "项目已归档，项目分支在 refs/domo-archive 中，无法直接合入",
        ));
    }
    let _guard = state.git_lock.lock().await;
    let project = require_project(&state, &id).await?;
    if project.stage != STAGE_COMPLETED {
        return Err(ApiError::bad("项目尚未完成，只有完成阶段的项目可以合入"));
    }
    let repo = PathBuf::from(&project.repo_path);
    let worktree = paths::project_repo(&PathBuf::from(&project.workspace));
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

pub(super) async fn ensure_base_worktree_clean(
    path: &std::path::Path,
    branch: &str,
) -> Result<(), ApiError> {
    let status = git::status_porcelain(path).await.map_err(ApiError::from)?;
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

pub(super) async fn restore_project_branch(
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

pub(super) async fn restore_fast_forward_failure(
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

pub(super) async fn finish_project_merge(
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
    let cleanup_state = state.clone();
    let cleanup_project = project.clone();
    tokio::spawn(async move {
        if let Err(error) =
            crate::artifacts::cleanup_completed_tasks(&cleanup_state, &cleanup_project).await
        {
            tracing::warn!(
                project = %cleanup_project.id,
                error = %format!("{error:#}"),
                "清理已合入项目的任务工作区失败"
            );
        }
    });
    Ok(Json(json!({
        "status": status,
        "message": message,
        "merged_at": merged_at,
        "merged_commit": merged_commit,
        "project": load_detail(state, &project.id).await?,
    })))
}

pub(super) fn short_commit(commit: &str) -> String {
    commit.chars().take(7).collect()
}
