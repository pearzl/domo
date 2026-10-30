//! Git artifact lifecycle. Task and project worktrees are temporary; completed
//! work is preserved in `refs/domo-archive/heads/` before the visible branch and
//! working directory are released.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use sqlx::SqlitePool;

use crate::domain::now;
use crate::git;
use crate::models::*;
use crate::paths;
use crate::state::AppState;

pub const ARTIFACTS_RELEASING: &str = "releasing";

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
    let pending = release_pending(task.artifacts_released_at.as_deref());
    let mut clean = true;
    let mut released = false;

    let guard = state.git_lock.lock().await;

    let mut remove_worktree = false;
    if let Some(path) = worktree.as_deref() {
        if path.exists() {
            if !managed_child(&workspace, path) {
                tracing::warn!(path = %path.display(), "任务工作区不在项目工作区内，跳过清理");
                clean = false;
            } else if !git::status_porcelain(path).await?.trim().is_empty() {
                tracing::warn!(path = %path.display(), "任务工作区有未提交改动，跳过清理");
                clean = false;
            } else {
                remove_worktree = true;
            }
        }
    }

    let mut archive_branch = false;
    if clean {
        if let Some(branch_name) = branch {
            if git::has_branch(&repo, branch_name).await {
                let mut available = true;
                if let Some(other) = git::worktree_for_branch(&repo, branch_name).await? {
                    if other.exists() {
                        let expected = worktree
                            .as_deref()
                            .is_some_and(|path| remove_worktree && same_path(path, &other));
                        if !expected {
                            tracing::warn!(
                                branch = branch_name,
                                path = %other.display(),
                                "任务分支仍被其他工作树占用，跳过清理"
                            );
                            available = false;
                        }
                    } else {
                        git::prune_worktrees(&repo).await?;
                    }
                }
                if available {
                    archive_branch = true;
                } else {
                    clean = false;
                }
            }
        }
    }

    if clean && !pending && (remove_worktree || archive_branch) {
        mark_releasing(&state.db, &task.id).await?;
    }

    if clean {
        if remove_worktree {
            if let Some(path) = worktree.as_deref() {
                git::remove_worktree(&repo, path).await?;
                released = true;
            }
        }
        if archive_branch {
            if let Some(branch_name) = branch {
                let expected = git::commit_of(&repo, branch_name).await?;
                git::archive_branch(&repo, branch_name, &expected).await?;
                released = true;
            }
        }
    }
    drop(guard);

    if clean && (released || pending) {
        finalize_release(&state.db, &task.id).await?;
    }
    Ok(clean)
}

/// Releases every task artifact together with the project checkout and branch.
/// Job directories remain because they back the audit view of archived projects.
/// Commits are preserved under `refs/domo-archive/`.
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
        if !release_pending(task.artifacts_released_at.as_deref()) {
            continue;
        }
        if !cleanup_task(state, project, task, force).await? {
            clean = false;
        }
    }

    let project_worktree = paths::project_repo(&workspace);
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
        remove_project_layout(&workspace).await?;
        let timestamp = now();
        sqlx::query(
            "UPDATE tasks SET artifacts_released_at = ? \
             WHERE project_id = ? AND (worktree IS NOT NULL OR branch IS NOT NULL) \
             AND (artifacts_released_at IS NULL OR artifacts_released_at = ?)",
        )
        .bind(&timestamp)
        .bind(&project.id)
        .bind(ARTIFACTS_RELEASING)
        .execute(&state.db)
        .await?;
    }
    Ok(clean)
}

/// Releases artifacts left behind by completed tasks. Used after project merges
/// and during startup recovery.
pub async fn cleanup_completed_tasks(state: &AppState, project: &ProjectRow) -> Result<()> {
    let tasks = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM tasks WHERE project_id = ? AND stage = ? \
         AND (artifacts_released_at IS NULL OR artifacts_released_at = ?) \
         AND (worktree IS NOT NULL OR branch IS NOT NULL)",
    )
    .bind(&project.id)
    .bind(TASK_STAGE_COMPLETED)
    .bind(ARTIFACTS_RELEASING)
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
        "SELECT * FROM tasks WHERE stage = ? \
         AND (artifacts_released_at IS NULL OR artifacts_released_at = ?) \
         AND (worktree IS NOT NULL OR branch IS NOT NULL)",
    )
    .bind(TASK_STAGE_COMPLETED)
    .bind(ARTIFACTS_RELEASING)
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

/// A task without a timestamp, or with the in-flight marker, still needs its
/// artifacts released. A concrete timestamp means the release is complete.
fn release_pending(value: Option<&str>) -> bool {
    matches!(value, None | Some(ARTIFACTS_RELEASING))
}

async fn mark_releasing(pool: &SqlitePool, task_id: &str) -> Result<()> {
    sqlx::query("UPDATE tasks SET artifacts_released_at = ? WHERE id = ?")
        .bind(ARTIFACTS_RELEASING)
        .bind(task_id)
        .execute(pool)
        .await?;
    Ok(())
}

async fn finalize_release(pool: &SqlitePool, task_id: &str) -> Result<()> {
    let timestamp = now();
    sqlx::query("UPDATE tasks SET artifacts_released_at = ?, updated_at = ? WHERE id = ?")
        .bind(&timestamp)
        .bind(&timestamp)
        .bind(task_id)
        .execute(pool)
        .await?;
    Ok(())
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// Releases the checkout directories of an archived project. Job directories
/// stay in place because they hold the prompt, result, and transcript context
/// that backs later audit views.
async fn remove_project_layout(workspace: &Path) -> Result<()> {
    for name in ["repo", "tasks", "verify"] {
        let path = workspace.join(name);
        if path.exists() {
            tokio::fs::remove_dir_all(&path).await?;
        }
    }
    Ok(())
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

    #[test]
    fn release_pending_accepts_missing_and_inflight_markers() {
        assert!(release_pending(None));
        assert!(release_pending(Some(ARTIFACTS_RELEASING)));
        assert!(!release_pending(Some("2026-10-30T12:00:00Z")));
    }

    #[tokio::test]
    async fn project_layout_cleanup_keeps_job_directories() {
        let workspace =
            std::env::temp_dir().join(format!("domo-artifacts-{}", uuid::Uuid::new_v4()));
        for name in ["repo", "tasks/1", "verify/1-job", "jobs/job_1"] {
            std::fs::create_dir_all(workspace.join(name)).unwrap();
        }
        std::fs::write(workspace.join("jobs/job_1/stdout.log"), "output").unwrap();

        remove_project_layout(&workspace).await.unwrap();

        assert!(!workspace.join("repo").exists());
        assert!(!workspace.join("tasks").exists());
        assert!(!workspace.join("verify").exists());
        assert_eq!(
            std::fs::read_to_string(workspace.join("jobs/job_1/stdout.log")).unwrap(),
            "output"
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn cleanup_task_retains_the_path_and_finalizes_the_release_marker() {
        let root = std::env::temp_dir().join(format!("domo-artifacts-{}", uuid::Uuid::new_v4()));
        let repo = root.join("repo");
        let workspace_root = root.join("ws");
        let workspace = workspace_root.join("prj_test");
        let project_worktree = workspace.join("repo");
        let task_worktree = workspace.join("tasks/1");
        std::fs::create_dir_all(&repo).unwrap();
        git::run_git(&repo, &["init", "-b", "main"]).await.unwrap();
        std::fs::write(repo.join("README.md"), "base").unwrap();
        git::run_git(&repo, &["add", "."]).await.unwrap();
        git::run_git(
            &repo,
            &[
                "-c",
                "user.name=DOMO",
                "-c",
                "user.email=domo@localhost",
                "commit",
                "-m",
                "initial",
            ],
        )
        .await
        .unwrap();
        let branch = "domo-task/test/1-t1";
        git::create_project_worktree(&repo, "main", &project_worktree, "domo/p-test")
            .await
            .unwrap();
        git::create_task_worktree(&project_worktree, "domo/p-test", &task_worktree, branch)
            .await
            .unwrap();

        let db = crate::db::connect(&root.join("data/domo.db"))
            .await
            .unwrap();
        let now = crate::domain::now();
        sqlx::query(
            "INSERT INTO projects (id, name, goal, repo_path, base_branch, branch, workspace, \
             stage, status, confirm_mode, completion_mode, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("prj_test")
        .bind("test")
        .bind("goal")
        .bind(repo.to_string_lossy().to_string())
        .bind("main")
        .bind("domo/p-test")
        .bind(workspace.to_string_lossy().to_string())
        .bind("completed")
        .bind("running")
        .bind("auto")
        .bind("tasks")
        .bind(&now)
        .bind(&now)
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tasks (id, project_id, seq, queue_order, key, title, description, \
             acceptance, deps, stage, branch, worktree, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("tsk_test")
        .bind("prj_test")
        .bind(1)
        .bind(1)
        .bind("T1")
        .bind("task")
        .bind("description")
        .bind("[]")
        .bind("[]")
        .bind("completed")
        .bind(branch)
        .bind(task_worktree.to_string_lossy().to_string())
        .bind(&now)
        .bind(&now)
        .execute(&db)
        .await
        .unwrap();

        let cfg = crate::config::Config {
            data_root: root.join("data"),
            workspace_root: workspace_root.clone(),
            log_root: root.join("log"),
            web_dist: root.join("web"),
            bind: "127.0.0.1".to_string(),
            port: 0,
            port_explicit: false,
            job_timeout: std::time::Duration::ZERO,
            stall_timeout: std::time::Duration::ZERO,
        };
        let (notify, _receiver) = tokio::sync::mpsc::channel(1);
        let state = AppState::new(db, cfg, crate::events::EventBus::new(), notify);
        let project = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
            .bind("prj_test")
            .fetch_one(&state.db)
            .await
            .unwrap();
        let task = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
            .bind("tsk_test")
            .fetch_one(&state.db)
            .await
            .unwrap();

        assert!(cleanup_task(&state, &project, &task, false).await.unwrap());
        assert!(!task_worktree.exists());
        let (worktree, released_at, branch_ref): (Option<String>, Option<String>, Option<String>) =
            sqlx::query_as("SELECT worktree, artifacts_released_at, branch FROM tasks WHERE id = ?")
                .bind("tsk_test")
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(worktree, Some(task_worktree.to_string_lossy().to_string()));
        assert!(released_at.is_some(), "回收完成后应写入时间");
        assert_ne!(released_at.as_deref(), Some(ARTIFACTS_RELEASING));
        assert_eq!(branch_ref.as_deref(), Some(branch));
        assert!(!git::has_branch(&repo, branch).await);
        assert!(git::commit_of(&repo, &git::archive_ref(branch))
            .await
            .is_ok());

        sqlx::query("UPDATE tasks SET artifacts_released_at = ? WHERE id = ?")
            .bind(ARTIFACTS_RELEASING)
            .bind("tsk_test")
            .execute(&state.db)
            .await
            .unwrap();
        let task = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
            .bind("tsk_test")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert!(cleanup_task(&state, &project, &task, false).await.unwrap());
        let released_at: Option<String> =
            sqlx::query_scalar("SELECT artifacts_released_at FROM tasks WHERE id = ?")
                .bind("tsk_test")
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert!(released_at.is_some());
        assert_ne!(released_at.as_deref(), Some(ARTIFACTS_RELEASING));
        let _ = std::fs::remove_dir_all(&root);
    }
}
