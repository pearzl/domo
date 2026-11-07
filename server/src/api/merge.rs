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
use crate::transitions::{ProjectAction, Transitions};

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
    let transition = Transitions::lock(&state).await;
    let result = merge_project_inner(&state, &id, &transition).await;
    if let Err(error) = &result {
        if error.message.starts_with("存在未恢复的合入操作") {
            return result;
        }
        let project = require_project(&state, &id).await?;
        let operation: Option<(String, String)> = sqlx::query_as(
            "SELECT base_before, project_before FROM merge_operations WHERE project_id = ? AND phase NOT IN ('completed', 'rolled_back')",
        ).bind(&id).fetch_optional(&state.db).await?;
        if let Some((base, branch)) = operation {
            let repo = PathBuf::from(&project.repo_path);
            let restored = git::commit_of(&repo, &project.base_branch)
                .await
                .ok()
                .as_deref()
                == Some(base.as_str())
                && git::commit_of(&repo, &project.branch).await.ok().as_deref()
                    == Some(branch.as_str());
            sqlx::query("UPDATE merge_operations SET phase = ?, error = ?, finished_at = ? WHERE project_id = ?")
                .bind(if restored { "rolled_back" } else { "recovery_required" })
                .bind(&error.message).bind(now()).bind(&id).execute(&state.db).await?;
            if !restored {
                return Err(ApiError::conflict(format!(
                    "合入恢复未完成，禁止再次合入；操作前引用已保存，请检查工作区与分支：{}",
                    error.message
                )));
            }
        }
        emit(
            &state.db,
            &state.bus,
            Some(&id),
            None,
            "project_merge_failed",
            &error.message,
        )
        .await;
    }
    result
}

async fn merge_project_inner(
    state: &AppState,
    id: &str,
    transition: &Transitions<'_>,
) -> Result<Json<Value>, ApiError> {
    let project = transition.project(id, ProjectAction::Merge).await?;
    if !project.is_git() {
        return Err(ApiError::bad("普通工作区没有交付分支，不支持合入"));
    }
    let _guard = state.git_lock.lock().await;
    let repo = PathBuf::from(&project.repo_path);
    let worktree = paths::project_repo(&PathBuf::from(&project.workspace));
    let base_branch = project.base_branch.clone();
    let project_branch = project.branch.clone();
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM merge_operations WHERE project_id = ? AND phase NOT IN ('completed', 'rolled_back')")
        .bind(id).fetch_one(&state.db).await?;
    if pending > 0 {
        return Err(ApiError::conflict(
            "存在未恢复的合入操作；操作前引用保存在 merge_operations，完成恢复前不能重试",
        ));
    }

    let base_before = git::commit_of(&repo, &base_branch)
        .await
        .map_err(|error| ApiError::bad(format!("无法读取目标分支 {base_branch}：{error:#}")))?;
    let project_before = git::commit_of(&repo, &project_branch)
        .await
        .map_err(|error| ApiError::bad(format!("无法读取项目分支 {project_branch}：{error:#}")))?;

    if git::is_ancestor(&repo, &project_before, &base_before).await? {
        return finish_project_merge(
            state,
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
    let project_status = git::status_porcelain(&worktree).await?;
    if !project_status.trim().is_empty() {
        return Err(ApiError::conflict(
            "项目工作区有未提交改动，拒绝 rebase，以免回滚覆盖文件现场",
        ));
    }
    sqlx::query("INSERT INTO merge_operations(project_id, base_before, project_before, phase, started_at) \
        VALUES(?, ?, ?, 'rebasing', ?)         ON CONFLICT(project_id) DO UPDATE SET base_before = excluded.base_before, \
        project_before = excluded.project_before, prepared_commit = NULL, phase = 'rebasing', \
        rebased = 0, agent_resolved = 0, conflicts = NULL, error = NULL, \
        started_at = excluded.started_at, finished_at = NULL")
        .bind(id).bind(&base_before).bind(&project_before).bind(now()).execute(&state.db).await?;

    if let Err(error) = git::rebase_onto(&worktree, &base_branch).await {
        restore_project_branch(&repo, &worktree, &project_branch, &project_before).await;
        return Err(ApiError::bad(format!(
            "合入失败：{error:#}；项目分支与目标分支均保持在操作前状态"
        )));
    }

    let project_after = git::commit_of(&repo, &project_branch).await?;
    let rebased = !git::is_ancestor(&repo, &base_before, &project_before).await?;
    sqlx::query(
        "UPDATE merge_operations SET prepared_commit = ?, phase = 'fast_forwarding', \
         rebased = ? WHERE project_id = ?",
    )
    .bind(&project_after)
    .bind(i64::from(rebased))
    .bind(id)
    .execute(&state.db)
    .await?;
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
            state,
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
        state,
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

/// Publishes a user-requested, isolated conflict resolution only if both source
/// refs still match its input. The project remains completed on every outcome.
pub(crate) async fn publish_assisted(
    state: &AppState,
    project: &ProjectRow,
    base_before: &str,
    project_before: &str,
    prepared: &str,
) -> Result<(), ApiError> {
    let _guard = state.git_lock.lock().await;
    let repo = PathBuf::from(&project.repo_path);
    let checkout = paths::project_repo(&PathBuf::from(&project.workspace));
    if git::commit_of(&repo, &project.base_branch).await? != base_before
        || git::commit_of(&repo, &project.branch).await? != project_before
    {
        return Err(ApiError::conflict(
            "合入输入分支已变化，请基于最新分支重新发起 Agent 处理",
        ));
    }
    if !git::status_porcelain(&checkout).await?.trim().is_empty() {
        return Err(ApiError::conflict(
            "项目工作区存在未提交文件，保留现场并拒绝合入",
        ));
    }
    if !git::is_ancestor(&repo, base_before, prepared).await?
        || git::count_merge_commits(&repo, base_before, prepared).await? != 0
    {
        return Err(ApiError::bad("Agent 结果未基于目标分支形成线性历史"));
    }
    let target = git::worktree_for_branch(&repo, &project.base_branch).await?;
    if let Some(path) = &target {
        ensure_base_worktree_clean(path, &project.base_branch).await?;
    }
    let rebased = !git::is_ancestor(&repo, base_before, project_before).await?;
    sqlx::query("INSERT INTO merge_operations(project_id, base_before, project_before, prepared_commit, phase, \
        rebased, agent_resolved, started_at) VALUES(?, ?, ?, ?, 'fast_forwarding', ?, 1, ?) \
        ON CONFLICT(project_id) DO UPDATE SET base_before = excluded.base_before, project_before = excluded.project_before, \
        prepared_commit = excluded.prepared_commit, phase = excluded.phase, rebased = excluded.rebased, \
        agent_resolved = 1, error = NULL, started_at = excluded.started_at, finished_at = NULL")
        .bind(&project.id).bind(base_before).bind(project_before).bind(prepared)
        .bind(i64::from(rebased)).bind(now()).execute(&state.db).await?;
    git::reset_hard_to(&checkout, prepared).await?;
    let result = if let Some(path) = &target {
        git::merge_ff_only(path, &project.branch).await
    } else {
        git::update_branch_ref(&repo, &project.base_branch, prepared, base_before).await
    };
    if let Err(error) = result {
        restore_project_branch(&repo, &checkout, &project.branch, project_before).await;
        let restored = git::commit_of(&repo, &project.branch).await? == project_before
            && git::commit_of(&repo, &project.base_branch).await? == base_before;
        sqlx::query("UPDATE merge_operations SET phase = ?, error = ? WHERE project_id = ?")
            .bind(if restored {
                "rolled_back"
            } else {
                "recovery_required"
            })
            .bind(format!("{error:#}"))
            .bind(&project.id)
            .execute(&state.db)
            .await?;
        return Err(error.into());
    }
    let _ = finish_project_merge(
        state,
        project,
        "merged",
        "Agent 冲突处理完成并线性合入".into(),
        prepared,
    )
    .await?;
    Ok(())
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
    let flags: (i64, i64, Option<String>) = sqlx::query_as(
        "SELECT rebased, agent_resolved, conflicts FROM merge_operations WHERE project_id = ?",
    )
    .bind(&project.id)
    .fetch_optional(&state.db)
    .await?
    .unwrap_or((0, 0, None));
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    if status == "up_to_date" {
        sqlx::query(
            "UPDATE projects SET merged_at = ?, merged_commit = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&merged_at)
        .bind(&merged_commit)
        .bind(&merged_at)
        .bind(&project.id)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "UPDATE projects SET merged_at = ?, merged_commit = ?, merge_rebased = ?, \
             merge_agent_resolved = ?, merge_conflicts = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&merged_at)
        .bind(&merged_commit)
        .bind(flags.0)
        .bind(flags.1)
        .bind(&flags.2)
        .bind(&merged_at)
        .bind(&project.id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE merge_operations SET phase = 'completed', finished_at = ? WHERE project_id = ?",
    )
    .bind(&merged_at)
    .bind(&project.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO events(project_id, kind, message, payload, created_at) VALUES(?, 'project_merge_settled', ?, ?, ?)")
        .bind(&project.id).bind(&message)
        .bind(json!({"version": 1, "status": status, "commit": commit, "base": project.base_branch}).to_string())
        .bind(&merged_at).execute(&mut *tx).await?;
    tx.commit().await?;
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

/// Reconciles operations interrupted between Git mutation and SQLite commit.
/// An unexpected branch tip or dirty checkout is never overwritten.
pub(crate) async fn recover_merges(state: &AppState) -> Result<(), ApiError> {
    let operations: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT project_id, base_before, project_before, prepared_commit FROM merge_operations \
         WHERE phase NOT IN ('completed', 'rolled_back')",
    )
    .fetch_all(&state.db)
    .await?;
    for (id, base_before, project_before, prepared) in operations {
        let project = require_project(state, &id).await?;
        let repo = PathBuf::from(&project.repo_path);
        let worktree = paths::project_repo(&PathBuf::from(&project.workspace));
        let base = git::commit_of(&repo, &project.base_branch).await?;
        let branch = git::commit_of(&repo, &project.branch).await?;
        if prepared.as_deref() == Some(base.as_str()) && branch == base {
            let _ = finish_project_merge(
                state,
                &project,
                "merged",
                "中断合入已核对完成".into(),
                &branch,
            )
            .await?;
            continue;
        }
        let expected = branch == project_before || prepared.as_deref() == Some(branch.as_str());
        if base == base_before && expected {
            git::abort_rebase(&worktree).await;
            if git::status_porcelain(&worktree).await?.trim().is_empty() {
                restore_project_branch(&repo, &worktree, &project.branch, &project_before).await;
                if git::commit_of(&repo, &project.branch).await? == project_before {
                    sqlx::query("UPDATE merge_operations SET phase = 'rolled_back', error = '服务中断，已恢复操作前引用', finished_at = ? WHERE project_id = ?")
                        .bind(now()).bind(&id).execute(&state.db).await?;
                    continue;
                }
            }
        }
        sqlx::query("UPDATE merge_operations SET phase = 'recovery_required', error = '引用或工作区与操作记录不符，需人工检查' WHERE project_id = ?")
            .bind(&id).execute(&state.db).await?;
        emit(&state.db, &state.bus, Some(&id), None, "merge_recovery_required",
            format!("合入未恢复：目标操作前 {base_before}；项目操作前 {project_before}；当前目标 {base}；当前项目 {branch}")).await;
    }
    Ok(())
}
