use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use tokio::fs;

use crate::domain::{now, short_id, slug};
use crate::git;
use crate::models::*;
use crate::paths;
use crate::state::AppState;

use super::common::load_task;

/// Every Git job runs in a dedicated worktree root; a mismatch means the agent
/// would read or write another checkout, so the job is rejected before spawn.
/// 普通工作区直接在所选目录中运行，只校验目录存在。
pub(super) async fn ensure_worktree_root(cwd: &Path, is_git: bool) -> Result<()> {
    if !is_git {
        if fs::metadata(cwd).await.map(|meta| meta.is_dir())? {
            return Ok(());
        }
        return Err(anyhow!("工作目录不存在: {}", cwd.display()));
    }
    let expected = fs::canonicalize(cwd).await?;
    let root = fs::canonicalize(git::repo_root(cwd).await?).await?;
    if root != expected {
        return Err(anyhow!(
            "工作目录 {} 不是独立 Git 工作树根目录（实际根目录 {}）",
            expected.display(),
            root.display()
        ));
    }
    Ok(())
}

pub(super) async fn prepare_task_worktree(
    state: &AppState,
    project: &ProjectRow,
    task: Option<&TaskRow>,
) -> Result<PathBuf> {
    let task = task.ok_or_else(|| anyhow!("执行作业缺少任务"))?;
    if !project.is_git() {
        let path = PathBuf::from(&project.repo_path);
        fs::create_dir_all(&path).await?;
        return Ok(path);
    }
    if let Some(worktree) = &task.worktree {
        let path = PathBuf::from(worktree);
        if path.join(".git").exists() {
            return Ok(path);
        }
    }
    let workspace = PathBuf::from(&project.workspace);
    let repo = paths::project_repo(&workspace);
    let path = paths::task_worktree(&workspace, task.seq);
    let branch = format!(
        "domo-task/{}/{}-{}",
        short_id(&project.id),
        task.seq,
        slug(&task.key)
    );
    let base_branch = if let Some(parent_id) = &task.parent_id {
        let parent = load_task(&state.db, parent_id)
            .await?
            .ok_or_else(|| anyhow!("派生子任务的父任务不存在"))?;
        parent
            .branch
            .clone()
            .ok_or_else(|| anyhow!("父任务还没有执行分支，无法派生子任务"))?
    } else {
        project.branch.clone()
    };
    {
        let _guard = state.git_lock.lock().await;
        if path.exists() {
            let _ = git::remove_worktree(&repo, &path).await;
            if path.exists() {
                let _ = crate::platform::remove_dir_all(&path).await;
            }
        }
        // A crash can leave the worktree registered while its directory is
        // gone. Prune that metadata before adding the path again.
        git::prune_worktrees(&repo).await?;
        git::create_task_worktree(&repo, &base_branch, &path, &branch).await?;
    }
    sqlx::query(
        "UPDATE tasks SET worktree = ?, branch = ?, artifacts_released_at = NULL, updated_at = ? \
         WHERE id = ?",
    )
    .bind(path.to_string_lossy().to_string())
    .bind(&branch)
    .bind(now())
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    Ok(path)
}

pub(super) async fn prepare_verify_worktree(
    state: &AppState,
    project: &ProjectRow,
    task: Option<&TaskRow>,
    job_id: &str,
) -> Result<PathBuf> {
    let task = task.ok_or_else(|| anyhow!("验收作业缺少任务"))?;
    if !project.is_git() {
        // 普通工作区不提供隔离副本，验收者只读核验同一交付目录。
        let path = PathBuf::from(&project.repo_path);
        fs::create_dir_all(&path).await?;
        return Ok(path);
    }
    let branch = task
        .branch
        .clone()
        .ok_or_else(|| anyhow!("任务没有执行分支，无法验收"))?;
    let workspace = PathBuf::from(&project.workspace);
    let repo = paths::project_repo(&workspace);
    let path = paths::verify_worktree(&workspace, task.seq, job_id);
    {
        let _guard = state.git_lock.lock().await;
        if path.exists() {
            let _ = git::remove_worktree(&repo, &path).await;
            if path.exists() {
                let _ = crate::platform::remove_dir_all(&path).await;
            }
        }
        git::prune_worktrees(&repo).await?;
        git::create_verify_worktree(&repo, &branch, &path).await?;
    }
    Ok(path)
}
