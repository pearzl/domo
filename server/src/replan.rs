//! Project replanning. Unfinished tasks leave the active DAG while completed
//! tasks, canceled task rows, jobs, and audit events remain available.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use sqlx::SqlitePool;

use crate::agents::kill_job_dir;
use crate::domain::now;
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct ReplanSummary {
    pub canceled_tasks: usize,
    pub retained_tasks: usize,
    pub canceled_jobs: usize,
}

pub async fn request(
    state: &AppState,
    project_id: &str,
    reason: &str,
    source: &str,
) -> Result<Option<ReplanSummary>> {
    let _guard = state.schedule_lock.lock().await;
    let Some(project) = load_project(&state.db, project_id).await? else {
        return Ok(None);
    };
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Ok(None);
    }
    if !matches!(project.stage.as_str(), STAGE_EXECUTING | STAGE_SUMMARIZING) {
        return Ok(None);
    }

    let tasks =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? ORDER BY seq ASC")
            .bind(project_id)
            .fetch_all(&state.db)
            .await?;
    let unfinished: Vec<TaskRow> = tasks
        .iter()
        .filter(|task| {
            task.stage != TASK_STAGE_COMPLETED && task.status.as_deref() != Some(STATUS_CANCELED)
        })
        .cloned()
        .collect();
    let retained_tasks = tasks
        .iter()
        .filter(|task| task.stage == TASK_STAGE_COMPLETED)
        .count();
    let unfinished_ids: HashSet<String> = unfinished.iter().map(|task| task.id.clone()).collect();
    let worktrees: Vec<PathBuf> = unfinished
        .iter()
        .filter_map(|task| task.worktree.as_ref().map(PathBuf::from))
        .collect();

    let running_jobs = sqlx::query_as::<_, JobRow>(
        "SELECT * FROM jobs WHERE project_id = ? AND status = 'running'",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await?;
    let canceled_jobs: Vec<JobRow> = running_jobs
        .into_iter()
        .filter(|job| {
            job.task_id
                .as_ref()
                .map(|task_id| unfinished_ids.contains(task_id))
                .unwrap_or(true)
        })
        .collect();

    let timestamp = now();
    let mut tx = state.db.begin().await?;
    if !unfinished.is_empty() {
        sqlx::query(
            "UPDATE tasks SET status = ?, agent_id = NULL, worktree = NULL, updated_at = ? \
             WHERE project_id = ? AND stage != ? AND COALESCE(status, '') != ?",
        )
        .bind(STATUS_CANCELED)
        .bind(&timestamp)
        .bind(project_id)
        .bind(TASK_STAGE_COMPLETED)
        .bind(STATUS_CANCELED)
        .execute(&mut *tx)
        .await?;
    }
    for job in &canceled_jobs {
        sqlx::query(
            "UPDATE jobs SET status = ?, error = ?, finished_at = ? \
             WHERE id = ? AND status = 'running'",
        )
        .bind(JOB_STATUS_CANCELED)
        .bind("项目重规划，作业已取消")
        .bind(&timestamp)
        .bind(&job.id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = NULL, report = NULL, report_feedback = NULL, \
         completion_mode = ?, proposal = NULL, replan_reason = ?, replan_source = ?, \
         replan_requested_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_ANALYSIS)
    .bind(STATUS_RUNNING)
    .bind(COMPLETION_TASKS)
    .bind(reason)
    .bind(source)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(project_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let workspace = PathBuf::from(&project.workspace);
    for job in &canceled_jobs {
        kill_job_dir(&workspace.join("jobs").join(&job.id));
    }
    if !worktrees.is_empty() {
        let _git_guard = state.git_lock.lock().await;
        let repo = workspace.join("repo");
        for path in worktrees {
            let _ = git::remove_worktree(&repo, &path).await;
            if path.exists() {
                let _ = tokio::fs::remove_dir_all(&path).await;
            }
        }
    }

    emit(
        &state.db,
        &state.bus,
        Some(project_id),
        None,
        "replan_requested",
        format!("重规划请求（来源：{}）：{reason}", source_label(source)),
    )
    .await;
    emit(
        &state.db,
        &state.bus,
        Some(project_id),
        None,
        "plan_superseded",
        format!(
            "旧计划已作废：取消 {} 个未完成任务，保留 {} 个已完成任务",
            unfinished.len(),
            retained_tasks
        ),
    )
    .await;

    Ok(Some(ReplanSummary {
        canceled_tasks: unfinished.len(),
        retained_tasks,
        canceled_jobs: canceled_jobs.len(),
    }))
}

/// A replan returns the project to analysis without a new user message. The
/// analysis job that follows the latest replan request runs unconditionally;
/// later analysis jobs still wait for user input.
pub async fn pending(pool: &SqlitePool, project: &ProjectRow) -> Result<bool> {
    let Some(requested_at) = project.replan_requested_at.as_deref() else {
        return Ok(false);
    };
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE project_id = ? AND role = ? AND created_at >= ?",
    )
    .bind(&project.id)
    .bind(JOB_ROLE_ANALYSIS)
    .bind(requested_at)
    .fetch_one(pool)
    .await?;
    Ok(count == 0)
}

fn source_label(source: &str) -> &'static str {
    match source {
        REPLAN_SOURCE_EXECUTOR => "执行者",
        REPLAN_SOURCE_HUMAN => "人工",
        _ => "未知",
    }
}

async fn load_project(pool: &SqlitePool, id: &str) -> Result<Option<ProjectRow>> {
    Ok(
        sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}
