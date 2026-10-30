//! Git artifact lifecycle. Task and project worktrees are temporary; completed
//! work is preserved in `refs/domo-archive/heads/` before the visible branch and
//! working directory are released.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use sqlx::SqlitePool;

use crate::domain::now;
use crate::git;
use crate::models::*;
use crate::state::AppState;

/// Releases the worktree and branch of one task after its changes have been
/// merged. A task with unfinished derived tasks keeps its artifacts because the
/// children use its branch as their base. Dirty worktrees are left untouched.
pub async fn cleanup_task(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    force: bool,
) -> Result<bool> {
    if !force && has_unfinished_children(&state.db, &task.id).await? {
        return Ok(false);
    }

    let workspace = PathBuf::from(&project.workspace);
    if !managed_child(&state.cfg.workspace_root, &workspace) {
        return Err(anyhow!("项目工作区不在 DOMO 工作区根目录下"));
    }
    let repo = project_repo(project)?;
    let branch = task.branch.as_deref().filter(|value| !value.is_empty());
    let worktree = task.worktree.as_deref().map(PathBuf::from);
    let mut clean = true;

    let guard = state.git_lock.lock().await;
    if let Some(path) = worktree.as_deref() {
        if path.exists() {
            if !managed_child(&workspace, path) {
                tracing::warn!(path = %path.display(), "任务工作区不在项目工作区内，跳过清理");
                clean = false;
            } else if !git::status_porcelain(path).await?.trim().is_empty() {
                tracing::warn!(path = %path.display(), "任务工作区有未提交改动，跳过清理");
                clean = false;
            } else {
                git::remove_worktree(&repo, path).await?;
            }
        }
    }

    if clean {
        if let Some(branch) = branch {
            if git::has_branch(&repo, branch).await {
                if let Some(other) = git::worktree_for_branch(&repo, branch).await? {
                    if other.exists() {
                        tracing::warn!(
                            branch,
                            path = %other.display(),
                            "任务分支仍被其他工作树占用，跳过清理"
                        );
                        clean = false;
                    } else {
                        git::prune_worktrees(&repo).await?;
                    }
                }
                if clean {
                    let expected = git::commit_of(&repo, branch).await?;
                    git::archive_branch(&repo, branch, &expected).await?;
                }
            }
        }
    }
    drop(guard);

    if clean && worktree.is_some() {
        sqlx::query("UPDATE tasks SET worktree = NULL, updated_at = ? WHERE id = ?")
            .bind(now())
            .bind(&task.id)
            .execute(&state.db)
            .await?;
    }
    Ok(clean)
}

/// Releases every task artifact and the project worktree, branch, and workspace
/// of an archived project. Commits are preserved under `refs/domo-archive/`.
pub async fn cleanup_project(state: &AppState, project: &ProjectRow, force: bool) -> Result<bool> {
    let workspace = PathBuf::from(&project.workspace);
    if !managed_child(&state.cfg.workspace_root, &workspace) {
        return Err(anyhow!("项目工作区不在 DOMO 工作区根目录下"));
    }
    let repo = project_repo(project)?;
    let tasks =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? ORDER BY seq ASC")
            .bind(&project.id)
            .fetch_all(&state.db)
            .await?;

    let mut clean = true;
    for task in &tasks {
        if !cleanup_task(state, project, task, force).await? {
            clean = false;
        }
    }

    let project_worktree = workspace.join("repo");
    let guard = state.git_lock.lock().await;
    if project_worktree.exists() {
        if !managed_child(&workspace, &project_worktree) {
            tracing::warn!(
                path = %project_worktree.display(),
                "项目工作区不在项目目录内，跳过清理"
            );
            clean = false;
        } else if !git::status_porcelain(&project_worktree)
            .await?
            .trim()
            .is_empty()
        {
            tracing::warn!(
                path = %project_worktree.display(),
                "项目工作区有未提交改动，跳过清理"
            );
            clean = false;
        } else {
            git::remove_worktree(&repo, &project_worktree).await?;
        }
    }

    if clean && git::has_branch(&repo, &project.branch).await {
        if let Some(other) = git::worktree_for_branch(&repo, &project.branch).await? {
            if other.exists() {
                tracing::warn!(
                    branch = %project.branch,
                    path = %other.display(),
                    "项目分支仍被其他工作树占用，跳过清理"
                );
                clean = false;
            } else {
                git::prune_worktrees(&repo).await?;
            }
        }
        if clean {
            let expected = git::commit_of(&repo, &project.branch).await?;
            git::archive_branch(&repo, &project.branch, &expected).await?;
        }
    }
    drop(guard);

    if clean {
        if workspace.exists() {
            tokio::fs::remove_dir_all(&workspace).await?;
        }
        sqlx::query("UPDATE tasks SET worktree = NULL WHERE project_id = ?")
            .bind(&project.id)
            .execute(&state.db)
            .await?;
    }
    Ok(clean)
}

/// Releases artifacts left behind by completed tasks. Used after project merges
/// and during startup recovery.
pub async fn cleanup_completed_tasks(state: &AppState, project: &ProjectRow) -> Result<()> {
    let tasks = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM tasks WHERE project_id = ? AND stage = ? AND worktree IS NOT NULL",
    )
    .bind(&project.id)
    .bind(TASK_STAGE_COMPLETED)
    .fetch_all(&state.db)
    .await?;
    for task in &tasks {
        if let Err(error) = cleanup_task(state, project, task, false).await {
            tracing::warn!(
                task = %task.id,
                error = %format!("{error:#}"),
                "清理任务工作区失败"
            );
        }
    }
    Ok(())
}

/// Startup compensation for a crash between the database transition and the
/// filesystem cleanup.
pub async fn recover(state: &AppState) {
    let tasks = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM tasks WHERE stage = ? AND worktree IS NOT NULL",
    )
    .bind(TASK_STAGE_COMPLETED)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for task in &tasks {
        let project = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
            .bind(&task.project_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);
        let Some(project) = project else {
            continue;
        };
        if let Err(error) = cleanup_task(state, &project, task, false).await {
            tracing::warn!(
                task = %task.id,
                error = %format!("{error:#}"),
                "启动恢复清理任务工作区失败"
            );
        }
    }

    let archived = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE status = ?")
        .bind(STATUS_ARCHIVED)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    for project in &archived {
        if let Err(error) = cleanup_project(state, project, true).await {
            tracing::warn!(
                project = %project.id,
                error = %format!("{error:#}"),
                "启动恢复清理归档项目失败"
            );
        }
    }
}

fn project_repo(project: &ProjectRow) -> Result<PathBuf> {
    let repo = PathBuf::from(&project.repo_path);
    if !repo.exists() {
        return Err(anyhow!("项目主仓库不存在: {}", repo.display()));
    }
    Ok(repo)
}

async fn has_unfinished_children(pool: &SqlitePool, task_id: &str) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tasks WHERE parent_id = ? AND stage != ? \
         AND COALESCE(status, '') != ?",
    )
    .bind(task_id)
    .bind(TASK_STAGE_COMPLETED)
    .bind(STATUS_CANCELED)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

/// True when `path` is a descendant of `root`, not the root itself. The check is
/// lexical so a deleted directory can still be recognized before cleanup.
fn managed_child(root: &Path, path: &Path) -> bool {
    path != root && path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_child_rejects_root_and_siblings() {
        let root = Path::new("/home/domo/workspace/prj_1");
        assert!(managed_child(root, &root.join("repo")));
        assert!(managed_child(root, &root.join("tasks/tsk_1")));
        assert!(!managed_child(root, root));
        assert!(!managed_child(
            root,
            Path::new("/home/domo/workspace/prj_2")
        ));
    }
}
