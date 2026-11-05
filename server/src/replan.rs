//! Project replanning keeps the confirmed plan intact while old jobs drain and a
//! draft is analyzed. Draft generations guard every transition into a new plan.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::agents::kill_job_dir;
use crate::domain::{self, now};
use crate::events::emit;
use crate::models::*;
use crate::paths;
use crate::state::AppState;

#[derive(Debug, thiserror::Error)]
#[error("重规划草稿已失效，请查看最新方案")]
pub struct StaleReplan;

#[derive(Debug, Clone)]
pub struct ReplanSummary {
    pub canceled_tasks: usize,
    pub retained_tasks: usize,
    pub canceled_jobs: usize,
}

pub(crate) struct AppliedReplan {
    summary: ReplanSummary,
}

#[derive(sqlx::FromRow)]
struct CleanupRequest {
    project_id: String,
    generation: i64,
    requested_at: String,
}

pub(crate) struct TaskPartition {
    pub canceled: Vec<TaskRow>,
    pub retained: Vec<TaskRow>,
}

pub async fn request(
    state: &AppState,
    project_id: &str,
    reason: &str,
    source: &str,
) -> Result<Option<ReplanSummary>> {
    request_inner(state, project_id, reason, source, None, None).await
}

pub async fn change_goal(
    state: &AppState,
    project_id: &str,
    goal: &str,
) -> Result<Option<ReplanSummary>> {
    let reason = format!("用户主动修改项目目标为：{goal}");
    request_inner(
        state,
        project_id,
        &reason,
        REPLAN_SOURCE_HUMAN,
        Some(goal),
        None,
    )
    .await
}

pub async fn request_report(
    state: &AppState,
    expected: &ProjectRow,
    reason: &str,
) -> Result<Option<ReplanSummary>> {
    request_inner(
        state,
        &expected.id,
        reason,
        REPLAN_SOURCE_HUMAN,
        None,
        Some(expected),
    )
    .await
}

async fn request_inner(
    state: &AppState,
    project_id: &str,
    reason: &str,
    source: &str,
    goal: Option<&str>,
    expected: Option<&ProjectRow>,
) -> Result<Option<ReplanSummary>> {
    let _guard = state.schedule_lock.lock().await;
    request_locked(state, project_id, reason, source, goal, expected).await
}

/// Caller holds the scheduling lock; approvals use the same guarded transition
/// as human and acceptance requests.
pub(crate) async fn request_locked(
    state: &AppState,
    project_id: &str,
    reason: &str,
    source: &str,
    goal: Option<&str>,
    expected: Option<&ProjectRow>,
) -> Result<Option<ReplanSummary>> {
    let Some(project) = load_project(&state.db, project_id).await? else {
        return Ok(None);
    };
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Ok(None);
    }
    let limits = crate::delivery::limits(&state.db, project_id).await?;
    if limits
        .replan_limit
        .is_some_and(|limit| project.replan_generation >= limit)
    {
        let reason = format!(
            "项目重规划次数达到上限，累计 {} 次；请提高限制后重新发起重规划",
            project.replan_generation
        );
        sqlx::query("UPDATE projects SET status = CASE WHEN status IN ('paused', 'pausing') THEN status ELSE 'blocked' END, \
            blocked_reason = ?, updated_at = ? WHERE id = ?")
            .bind(&reason).bind(now()).bind(project_id).execute(&state.db).await?;
        emit(
            &state.db,
            &state.bus,
            Some(project_id),
            None,
            "project_limit_reached",
            &reason,
        )
        .await;
        return Ok(None);
    }
    if expected.is_some_and(|expected| {
        project.replan_generation != expected.replan_generation
            || project.stage != STAGE_SUMMARIZING
            || project.report != expected.report
    }) {
        return Ok(None);
    }
    if goal.is_some() {
        if !matches!(
            project.stage.as_str(),
            STAGE_EXECUTING | STAGE_SUMMARIZING | STAGE_ANALYSIS
        ) || project.goal_locked_at.is_none()
        {
            return Ok(None);
        }
    } else if !matches!(project.stage.as_str(), STAGE_EXECUTING | STAGE_SUMMARIZING) {
        return Ok(None);
    }

    let tasks = load_tasks(&state.db, project_id).await?;
    let partition = partition_tasks(tasks);
    let canceled_ids: HashSet<String> = partition
        .canceled
        .iter()
        .map(|task| task.id.clone())
        .collect();
    let running_jobs = sqlx::query_as::<_, JobRow>(
        "SELECT * FROM jobs WHERE project_id = ? AND status = 'running'",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await?;
    let canceled_jobs = running_jobs
        .iter()
        .filter(|job| job_will_be_canceled(job, &canceled_ids))
        .count();

    let timestamp = now();
    sqlx::query(
        "UPDATE projects SET goal = COALESCE(?, goal), stage = ?, \
         status = CASE WHEN status IN ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
         blocked_reason = NULL, replan_reason = ?, replan_source = ?, \
         replan_requested_at = ?, replan_active = 1, replan_proposal = NULL, \
         replan_generation = replan_generation + 1, replan_previous_stage = ?, \
         replan_previous_goal = CASE WHEN replan_active = 0 THEN ? ELSE replan_previous_goal END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(goal)
    .bind(STAGE_ANALYSIS)
    .bind(STATUS_RUNNING)
    .bind(reason)
    .bind(source)
    .bind(&timestamp)
    .bind(
        project
            .replan_previous_stage
            .as_deref()
            .unwrap_or(&project.stage),
    )
    .bind(&project.goal)
    .bind(&timestamp)
    .bind(project_id)
    .execute(&state.db)
    .await?;

    emit(
        &state.db,
        &state.bus,
        Some(project_id),
        None,
        "replan_requested",
        format!(
            "重规划请求（来源：{}）：{reason}；等待在途作业结束，旧计划在方案确认前保持可恢复",
            source_label(source)
        ),
    )
    .await;
    if let Some(goal) = goal {
        emit(
            &state.db,
            &state.bus,
            Some(project_id),
            None,
            "goal_updated",
            format!("用户主动提交新项目目标，重规划只能围绕该目标生成：{goal}"),
        )
        .await;
    }

    Ok(Some(ReplanSummary {
        canceled_tasks: partition.canceled.len(),
        retained_tasks: partition.retained.len(),
        canceled_jobs,
    }))
}

/// Stores a draft only while the requesting generation still owns analysis.
pub async fn store_draft(state: &AppState, project: &ProjectRow, raw: &str) -> Result<bool> {
    let _guard = state.schedule_lock.lock().await;
    let Some(current) = load_project(&state.db, &project.id).await? else {
        return Ok(false);
    };
    if !same_generation(&current, project) || current.stage != STAGE_ANALYSIS {
        return Ok(false);
    }
    if running_non_analysis_jobs(&state.db, &project.id).await? > 0 {
        return Ok(false);
    }
    let updated = sqlx::query(
        "UPDATE projects SET replan_proposal = ?, updated_at = ? \
         WHERE id = ? AND replan_active = 1 AND replan_generation = ? AND stage = ?",
    )
    .bind(raw)
    .bind(now())
    .bind(&project.id)
    .bind(project.replan_generation)
    .bind(STAGE_ANALYSIS)
    .execute(&state.db)
    .await?;
    Ok(updated.rows_affected() == 1)
}

pub async fn analysis_is_current(state: &AppState, project: &ProjectRow) -> Result<bool> {
    if project.goal_locked_at.is_none() {
        return Ok(true);
    }
    let Some(current) = load_project(&state.db, &project.id).await? else {
        return Ok(false);
    };
    let same_identity = current.goal_locked_at.is_some()
        && current.goal == project.goal
        && current.replan_generation == project.replan_generation;
    let same_replan = project.replan_active == 0 || same_generation(&current, project);
    Ok(same_identity && same_replan && current.stage == STAGE_ANALYSIS)
}

/// Rejects a draft that attempts to replace a confirmed goal through replanning.
pub async fn reject_goal_change(state: &AppState, project: &ProjectRow) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    if !analysis_is_current(state, project).await? {
        return Ok(());
    }
    let reason = "重规划不能修改已确认目标；草稿已失效，旧目标与旧计划保留";
    sqlx::query(
        "UPDATE projects SET replan_proposal = NULL, status = CASE WHEN status IN \
         ('pausing', 'paused', 'archived') THEN status ELSE ? END, blocked_reason = ?, \
         updated_at = ? WHERE id = ?",
    )
    .bind(STATUS_BLOCKED)
    .bind(reason)
    .bind(now())
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "plan_invalid",
        reason,
    )
    .await;
    Ok(())
}

/// Computes the user-visible effects against the plan that is still confirmed.
pub async fn impact(
    state: &AppState,
    project: &ProjectRow,
    kind: &str,
    acceptance: &[String],
    verification: &[String],
    tasks: &[domain::NormalizedTask],
) -> Result<ReplanImpact> {
    let current = load_tasks(&state.db, &project.id).await?;
    let active: Vec<TaskRow> = current
        .iter()
        .filter(|task| task.status.as_deref() != Some(STATUS_CANCELED))
        .cloned()
        .collect();
    let current_by_key: HashMap<String, &TaskRow> =
        active.iter().map(|task| (task.key.clone(), task)).collect();
    let partition = partition_tasks(current);
    let canceled_task_keys = partition
        .canceled
        .iter()
        .map(|task| task.key.clone())
        .collect::<Vec<_>>();
    let retained_task_keys = partition
        .retained
        .iter()
        .map(|task| task.key.clone())
        .collect::<Vec<_>>();
    let added_task_keys = tasks
        .iter()
        .filter(|task| !current_by_key.contains_key(&task.key))
        .map(|task| task.key.clone())
        .collect::<Vec<_>>();

    let previous = project
        .proposal
        .as_deref()
        .and_then(|raw| serde_json::from_str::<AnalysisProposal>(raw).ok());
    let previous_acceptance = previous.as_ref().map_or(&[][..], |proposal| {
        if proposal.kind == "direct" {
            &proposal.verification
        } else {
            &proposal.acceptance
        }
    });
    let next_acceptance = if kind == "direct" {
        verification
    } else {
        acceptance
    };
    let task_acceptance_changed = tasks.iter().any(|task| {
        current_by_key
            .get(&task.key)
            .is_some_and(|current| parse_string_list(&current.acceptance) != task.acceptance)
    });
    let acceptance_changed = previous_acceptance != next_acceptance || task_acceptance_changed;
    let high_impact =
        !canceled_task_keys.is_empty() || !added_task_keys.is_empty() || acceptance_changed;

    Ok(ReplanImpact {
        high_impact,
        canceled_task_keys,
        retained_task_keys,
        added_task_keys,
        acceptance_changed,
    })
}

pub(crate) async fn validate_apply_tx(
    tx: &mut Transaction<'_, Sqlite>,
    project: &ProjectRow,
    proposal_json: &str,
) -> Result<()> {
    let current = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
        .bind(&project.id)
        .fetch_one(&mut **tx)
        .await?;
    if !same_generation(&current, project)
        || current.stage != STAGE_ANALYSIS
        || current.replan_proposal.as_deref() != Some(proposal_json)
    {
        return Err(StaleReplan.into());
    }
    let proposal: AnalysisProposal = serde_json::from_str(proposal_json)?;
    if current.goal != project.goal
        || proposal.goal.as_deref().map(str::trim) != Some(current.goal.trim())
    {
        return Err(StaleReplan.into());
    }
    if running_non_analysis_jobs_tx(tx, &project.id).await? > 0 {
        return Err(StaleReplan.into());
    }
    Ok(())
}

pub(crate) async fn enqueue_cleanup_tx(
    tx: &mut Transaction<'_, Sqlite>,
    project_id: &str,
    generation: i64,
    timestamp: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO replan_cleanup (project_id, generation, requested_at, status, created_at) \
         VALUES (?, ?, ?, 'pending', ?) ON CONFLICT(project_id, generation) DO UPDATE SET \
         status = 'pending', requested_at = MIN(replan_cleanup.requested_at, excluded.requested_at), \
         created_at = excluded.created_at, completed_at = NULL",
    )
    .bind(project_id)
    .bind(generation)
    .bind(timestamp)
    .bind(timestamp)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Cancels complete task families inside the caller's apply transaction.
pub(crate) async fn supersede_tx(
    tx: &mut Transaction<'_, Sqlite>,
    project_id: &str,
    timestamp: &str,
) -> Result<AppliedReplan> {
    let tasks =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? ORDER BY seq ASC")
            .bind(project_id)
            .fetch_all(&mut **tx)
            .await?;
    let partition = partition_tasks(tasks);
    let canceled_ids: HashSet<String> = partition
        .canceled
        .iter()
        .map(|task| task.id.clone())
        .collect();
    let running_jobs = sqlx::query_as::<_, JobRow>(
        "SELECT * FROM jobs WHERE project_id = ? AND status = 'running'",
    )
    .bind(project_id)
    .fetch_all(&mut **tx)
    .await?;
    let canceled_jobs: Vec<JobRow> = running_jobs
        .into_iter()
        .filter(|job| job_will_be_canceled(job, &canceled_ids))
        .collect();

    for task in &partition.canceled {
        sqlx::query(
            "UPDATE tasks SET status = ?, agent_id = NULL, updated_at = ? \
             WHERE id = ? AND COALESCE(status, '') != ?",
        )
        .bind(STATUS_CANCELED)
        .bind(timestamp)
        .bind(&task.id)
        .bind(STATUS_CANCELED)
        .execute(&mut **tx)
        .await?;
    }
    for job in &canceled_jobs {
        sqlx::query(
            "UPDATE jobs SET status = ?, error = ?, finished_at = ? \
             WHERE id = ? AND status = 'running'",
        )
        .bind(JOB_STATUS_CANCELED)
        .bind("重规划方案已确认，旧计划作业已取消")
        .bind(timestamp)
        .bind(&job.id)
        .execute(&mut **tx)
        .await?;
    }

    Ok(AppliedReplan {
        summary: ReplanSummary {
            canceled_tasks: partition.canceled.len(),
            retained_tasks: partition.retained.len(),
            canceled_jobs: canceled_jobs.len(),
        },
    })
}

/// Runs crash-safe compensation after the plan switch commits.
pub(crate) async fn finish_apply(state: &AppState, project: &ProjectRow, applied: AppliedReplan) {
    if std::env::var("DOMO_MOCK_REPLAN_CRASH_AFTER_APPLY").is_ok_and(|value| value == "1") {
        std::process::exit(70);
    }
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "plan_superseded",
        format!(
            "旧计划已作废：取消 {} 个旧任务，保留 {} 个完整交付任务链",
            applied.summary.canceled_tasks, applied.summary.retained_tasks
        ),
    )
    .await;
    cleanup_current(state, &project.id, project.replan_generation).await;
}

pub async fn recover_cleanup(state: &AppState) {
    let requests = sqlx::query_as::<_, CleanupRequest>(
        "SELECT project_id, generation, requested_at FROM replan_cleanup WHERE status = 'pending' \
         ORDER BY created_at ASC",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for request in requests {
        cleanup_current(state, &request.project_id, request.generation).await;
    }
}

async fn cleanup_current(state: &AppState, project_id: &str, generation: i64) {
    let Some(request) = sqlx::query_as::<_, CleanupRequest>(
        "SELECT project_id, generation, requested_at FROM replan_cleanup \
         WHERE project_id = ? AND generation = ? AND status = 'pending'",
    )
    .bind(project_id)
    .bind(generation)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten() else {
        return;
    };
    let Some(project) = load_project(&state.db, project_id).await.ok().flatten() else {
        return;
    };
    let requested_at = request.requested_at.as_str();
    let workspace = PathBuf::from(&project.workspace);
    let jobs = sqlx::query_as::<_, JobRow>(
        "SELECT * FROM jobs WHERE project_id = ? AND status = ? AND finished_at >= ?",
    )
    .bind(project_id)
    .bind(JOB_STATUS_CANCELED)
    .bind(requested_at)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for job in jobs {
        kill_job_dir(&paths::job_dir(&workspace, &job.id));
    }
    let tasks = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM tasks WHERE project_id = ? AND status = ? AND updated_at >= ?",
    )
    .bind(project_id)
    .bind(STATUS_CANCELED)
    .bind(requested_at)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut complete = true;
    for task in tasks {
        match crate::artifacts::cleanup_task(state, &project, &task, true).await {
            Ok(true) => {}
            Ok(false) => complete = false,
            Err(error) => {
                complete = false;
                tracing::warn!(
                    task = %task.id,
                    error = %format!("{error:#}"),
                    "清理重规划任务工作区失败"
                );
            }
        }
    }
    if complete {
        let _ = sqlx::query(
            "UPDATE replan_cleanup SET status = 'done', completed_at = ? \
             WHERE project_id = ? AND generation = ? AND status = 'pending'",
        )
        .bind(now())
        .bind(project_id)
        .bind(generation)
        .execute(&state.db)
        .await;
    }
}

pub(crate) async fn apply_direct(
    state: &AppState,
    project: &ProjectRow,
    proposal_json: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    let timestamp = now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    validate_apply_tx(&mut tx, project, proposal_json).await?;
    let applied = supersede_tx(&mut tx, &project.id, &timestamp).await?;
    enqueue_cleanup_tx(&mut tx, &project.id, project.replan_generation, &timestamp).await?;
    // The acceptance round is cumulative across plans so automatic replans cannot bypass its cap.
    sqlx::query(
        "UPDATE projects SET proposal = ?, confirmed_at = ?, stage = ?, \
         status = CASE WHEN status IN ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
         completion_mode = ?, blocked_reason = NULL, report = NULL, report_feedback = NULL, \
         acceptance = NULL, acceptance_passed = 0, acceptance_at = NULL, \
         replan_active = 0, replan_proposal = NULL, replan_previous_stage = NULL, \
         replan_previous_goal = NULL, goal_locked_at = COALESCE(goal_locked_at, ?), \
         updated_at = ? WHERE id = ?",
    )
    .bind(proposal_json)
    .bind(&timestamp)
    .bind(STAGE_EXECUTING)
    .bind(STATUS_RUNNING)
    .bind(COMPLETION_DIRECT)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(&project.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    finish_apply(state, project, applied).await;
    Ok(())
}

/// Abandons the draft and restores dispatch of the plan that remains confirmed.
pub async fn cancel(state: &AppState, project_id: &str) -> Result<Option<ProjectRow>> {
    let _guard = state.schedule_lock.lock().await;
    let Some(project) = load_project(&state.db, project_id).await? else {
        return Ok(None);
    };
    if project.replan_active == 0 || project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Ok(None);
    }
    let running_analysis: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE project_id = ? AND role = ? AND status = 'running'",
    )
    .bind(project_id)
    .bind(JOB_ROLE_ANALYSIS)
    .fetch_one(&state.db)
    .await?;
    if running_analysis > 0 {
        return Ok(None);
    }
    let stage = if project.report.is_some() {
        STAGE_SUMMARIZING
    } else {
        project
            .replan_previous_stage
            .as_deref()
            .unwrap_or(STAGE_EXECUTING)
    };
    let waiting_report = stage == STAGE_SUMMARIZING && project.report.is_some();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query(
        "UPDATE projects SET goal = COALESCE(replan_previous_goal, goal), stage = ?, \
         status = CASE WHEN status IN ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
         blocked_reason = NULL, replan_active = 0, replan_proposal = NULL, \
         replan_previous_stage = NULL, replan_previous_goal = NULL, updated_at = ? \
         WHERE id = ? AND replan_active = 1",
    )
    .bind(stage)
    .bind(if waiting_report {
        STATUS_BLOCKED
    } else {
        STATUS_RUNNING
    })
    .bind(now())
    .bind(project_id)
    .execute(&mut *tx)
    .await?;
    let guidance =
        "用户已放弃替代方案，请沿用原任务，核对请求中提出的问题；不得把未应用的方案当作有效输入。";
    sqlx::query("UPDATE tasks SET status = CASE WHEN status IN ('paused', 'pausing') THEN status ELSE 'queued' END, guidance = guidance || char(10) || ?, waiting_reason = NULL, last_error = NULL, updated_at = ? WHERE id IN (SELECT task_id FROM replan_requests WHERE project_id = ? AND reviewer_task_id IS NULL AND state IN ('planned', 'ready') AND ? LIKE '任务请求 ' || id || '：%')")
        .bind(guidance).bind(now()).bind(project_id).bind(&project.replan_reason).execute(&mut *tx).await?;
    sqlx::query("UPDATE replan_requests SET state = 'resolved', guidance = ?, updated_at = ? WHERE project_id = ? AND reviewer_task_id IS NULL AND state IN ('planned', 'ready') AND ? LIKE '任务请求 ' || id || '：%'")
        .bind(guidance).bind(now()).bind(project_id).bind(&project.replan_reason).execute(&mut *tx).await?;
    tx.commit().await?;
    emit(
        &state.db,
        &state.bus,
        Some(project_id),
        None,
        "replan_aborted",
        format!(
            "已放弃重规划，恢复旧目标、旧计划与任务派发：{}",
            project
                .replan_previous_goal
                .as_deref()
                .unwrap_or(&project.goal)
        ),
    )
    .await;
    load_project(&state.db, project_id).await
}

pub(crate) fn partition_tasks(tasks: Vec<TaskRow>) -> TaskPartition {
    let active: Vec<TaskRow> = tasks
        .into_iter()
        .filter(|task| task.status.as_deref() != Some(STATUS_CANCELED))
        .collect();
    let canceled: HashSet<String> = active
        .iter()
        .filter(|task| task.stage != TASK_STAGE_COMPLETED)
        .map(|task| task.id.clone())
        .collect();
    // 已完成子任务已经独立验收并交付，不能随未完成父任务一起作废。
    let (canceled_tasks, retained): (Vec<TaskRow>, Vec<TaskRow>) = active
        .into_iter()
        .partition(|task| canceled.contains(&task.id));
    TaskPartition {
        canceled: canceled_tasks,
        retained: retained
            .into_iter()
            .filter(|task| task.stage == TASK_STAGE_COMPLETED)
            .collect(),
    }
}

fn same_generation(current: &ProjectRow, expected: &ProjectRow) -> bool {
    current.replan_active == 1 && current.replan_generation == expected.replan_generation
}

fn job_will_be_canceled(job: &JobRow, canceled_ids: &HashSet<String>) -> bool {
    job.task_id
        .as_ref()
        .map(|task_id| canceled_ids.contains(task_id))
        .unwrap_or(job.role != JOB_ROLE_ANALYSIS)
}

pub async fn running_non_analysis_jobs(pool: &SqlitePool, project_id: &str) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE project_id = ? AND status = 'running' AND role != ?",
    )
    .bind(project_id)
    .bind(JOB_ROLE_ANALYSIS)
    .fetch_one(pool)
    .await?)
}

async fn running_non_analysis_jobs_tx(
    tx: &mut Transaction<'_, Sqlite>,
    project_id: &str,
) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE project_id = ? AND status = 'running' AND role != ?",
    )
    .bind(project_id)
    .bind(JOB_ROLE_ANALYSIS)
    .fetch_one(&mut **tx)
    .await?)
}

/// Interrupted replan analysis is eligible for dispatch after restart or forced pause.
pub async fn pending(pool: &SqlitePool, project: &ProjectRow) -> Result<bool> {
    if project.replan_active == 0 {
        return Ok(false);
    }
    let latest = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT status, error FROM jobs WHERE project_id = ? AND role = ? AND replan_generation = ? \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(&project.id)
    .bind(JOB_ROLE_ANALYSIS)
    .bind(project.replan_generation)
    .fetch_optional(pool)
    .await?;
    Ok(match latest {
        None => true,
        Some((status, error))
            if matches!(status.as_str(), JOB_STATUS_FAILED | JOB_STATUS_CANCELED) =>
        {
            error.is_some_and(|error| error.contains("服务重启") || error.contains("强制暂停"))
        }
        _ => false,
    })
}

fn source_label(source: &str) -> &'static str {
    match source {
        REPLAN_SOURCE_EXECUTOR => "执行者",
        REPLAN_SOURCE_ACCEPTANCE => "项目验收",
        REPLAN_SOURCE_HUMAN => "人工",
        _ => "未知",
    }
}

async fn load_tasks(pool: &SqlitePool, project_id: &str) -> Result<Vec<TaskRow>> {
    Ok(
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? ORDER BY seq ASC")
            .bind(project_id)
            .fetch_all(pool)
            .await?,
    )
}

pub async fn load_project(pool: &SqlitePool, id: &str) -> Result<Option<ProjectRow>> {
    Ok(
        sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}
