//! Single scheduler loop. It reconciles persisted state on every tick, so a
//! restart or a completed job is just another reason to run reconciliation.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use tokio::sync::mpsc;

use crate::agents::kill_job_dir;
use crate::domain::{self, deps_complete, now, short_id};
use crate::events::{emit, emit_job};
use crate::models::*;
use crate::paths;
use crate::runner;
use crate::state::AppState;
use crate::usage;

pub async fn run(state: AppState, mut rx: mpsc::Receiver<()>) {
    if let Err(error) = recover(&state).await.context("恢复启动状态失败") {
        tracing::error!(error = %error, "启动恢复失败");
        // 未完成资源核对时禁止开始派发，保留现场供人工处理。
        return;
    }
    let mut shutdown = state.shutdown_receiver();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = shutdown.wait_for(|stopping| *stopping) => break,
            message = rx.recv() => {
                if message.is_none() {
                    break;
                }
            }
        }
        if state.is_shutting_down() {
            break;
        }
        let jobs = {
            let _guard = state.schedule_lock.lock().await;
            match reconcile(&state).await.context("调度轮次失败") {
                Ok(jobs) => jobs,
                Err(error) => {
                    tracing::error!(error = %error, "调度失败");
                    Vec::new()
                }
            }
        };
        if state.is_shutting_down() {
            break;
        }
        for job_id in jobs {
            let state = state.clone();
            tokio::spawn(async move {
                runner::execute_job(state, job_id).await;
            });
        }
    }
}

async fn recover(state: &AppState) -> Result<()> {
    cleanup_orphans(state).await;
    crate::resources::recover(state).await?;
    sqlx::query("UPDATE merge_requests SET state = 'failed', error = '服务中断，保留处理工作区，请显式重新发起' WHERE state = 'running'")
        .execute(&state.db).await?;
    crate::api::recover_merges(state)
        .await
        .map_err(|error| anyhow::anyhow!("合入恢复失败：{error:?}"))?;
    let jobs = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE status = 'running'")
        .fetch_all(&state.db)
        .await?;
    for job in jobs {
        if let Ok(Some(workspace)) =
            sqlx::query_scalar::<_, String>("SELECT workspace FROM projects WHERE id = ?")
                .bind(&job.project_id)
                .fetch_optional(&state.db)
                .await
        {
            let job_dir = paths::job_dir(&PathBuf::from(workspace), &job.id);
            if crate::agents::kill_job_dir(&job_dir) {
                tracing::info!(job_id = %job.id, "服务重启后终止旧作业进程");
            }
        }
        let finished_at = now();
        let duration_ms = job
            .started_at
            .as_deref()
            .and_then(|started| crate::usage::elapsed_ms(Some(started), &finished_at));
        sqlx::query(
            "UPDATE jobs SET status = ?, error = ?, duration_ms = COALESCE(duration_ms, ?), \
             finished_at = ? WHERE id = ?",
        )
        .bind(JOB_STATUS_FAILED)
        .bind("服务重启导致作业中断")
        .bind(duration_ms)
        .bind(&finished_at)
        .bind(&job.id)
        .execute(&state.db)
        .await?;
        if let Some(task_id) = &job.task_id {
            if let Some(task) = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
                .bind(task_id)
                .fetch_optional(&state.db)
                .await?
            {
                if task.status.as_deref() == Some(STATUS_RUNNING) {
                    let attempts = task.attempts + 1;
                    let blocked = attempts >= 3;
                    sqlx::query(
                        "UPDATE tasks SET attempts = ?, status = ?, last_error = ?, \
                         last_agent_id = COALESCE(?, last_agent_id), updated_at = ? WHERE id = ?",
                    )
                    .bind(attempts)
                    .bind(if blocked {
                        STATUS_BLOCKED
                    } else {
                        STATUS_QUEUED
                    })
                    .bind("服务重启导致作业中断")
                    .bind(&job.agent_id)
                    .bind(now())
                    .bind(&task.id)
                    .execute(&state.db)
                    .await?;
                }
            }
        } else if job.role == JOB_ROLE_ANALYSIS {
            sqlx::query(
                "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
                 THEN status ELSE ? END, blocked_reason = NULL, updated_at = ? \
                 WHERE id = ? AND stage = ?",
            )
            .bind(STAGE_ANALYSIS)
            .bind(STATUS_RUNNING)
            .bind(now())
            .bind(&job.project_id)
            .bind(STAGE_ANALYSIS)
            .execute(&state.db)
            .await?;
        } else if job.role == JOB_ROLE_DIRECT {
            sqlx::query(
                "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
                 THEN status ELSE ? END, blocked_reason = NULL, updated_at = ? \
                 WHERE id = ? AND stage = ?",
            )
            .bind(STAGE_EXECUTING)
            .bind(STATUS_RUNNING)
            .bind(now())
            .bind(&job.project_id)
            .bind(STAGE_EXECUTING)
            .execute(&state.db)
            .await?;
        } else if job.role == JOB_ROLE_ACCEPTANCE {
            sqlx::query(
                "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
                 THEN status ELSE ? END, blocked_reason = NULL, acceptance = NULL, \
                 acceptance_passed = 0, acceptance_at = NULL, updated_at = ? \
                 WHERE id = ? AND stage = ? AND acceptance_passed = 0",
            )
            .bind(STAGE_SUMMARIZING)
            .bind(STATUS_RUNNING)
            .bind(now())
            .bind(&job.project_id)
            .bind(STAGE_SUMMARIZING)
            .execute(&state.db)
            .await?;
        } else if job.role == JOB_ROLE_SUMMARY {
            sqlx::query(
                "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
                 THEN status ELSE ? END, blocked_reason = NULL, updated_at = ? \
                 WHERE id = ? AND stage = ? AND report IS NULL",
            )
            .bind(STAGE_SUMMARIZING)
            .bind(STATUS_RUNNING)
            .bind(now())
            .bind(&job.project_id)
            .bind(STAGE_SUMMARIZING)
            .execute(&state.db)
            .await?;
        }
        emit_job(
            &state.db,
            &state.bus,
            Some(&job.project_id),
            job.task_id.as_deref(),
            Some(&job.id),
            "job_recovered",
            format!("作业 {} 因服务重启中断，将重新调度", short_id(&job.id)),
        )
        .await;
    }

    let projects = sqlx::query_as::<_, ProjectRow>(
        "SELECT * FROM projects WHERE status IN ('running', 'pausing', 'blocked')",
    )
    .fetch_all(&state.db)
    .await?;
    for project in projects {
        if project.status.as_deref() == Some(STATUS_BLOCKED)
            && project
                .blocked_reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty())
            && project.completion_mode == COMPLETION_DIRECT
        {
            // 协议失败与人工决策不能通过重启解除。
            continue;
        }
        if project.stage == STAGE_SUMMARIZING && project.report.is_some() {
            if matches!(
                project.status.as_deref(),
                Some(STATUS_PAUSING) | Some(STATUS_PAUSED) | Some(STATUS_ARCHIVED)
            ) {
                continue;
            }
            if project.status.as_deref() != Some(STATUS_BLOCKED) {
                sqlx::query("UPDATE projects SET status = ?, updated_at = ? WHERE id = ?")
                    .bind(STATUS_BLOCKED)
                    .bind(now())
                    .bind(&project.id)
                    .execute(&state.db)
                    .await?;
            }
            continue;
        }
        if project.stage == STAGE_EXECUTING || project.stage == STAGE_SUMMARIZING {
            let human_gate: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage != ? AND status = ? \
                 AND human_gate IS NOT NULL",
            )
            .bind(&project.id)
            .bind(TASK_STAGE_COMPLETED)
            .bind(STATUS_BLOCKED)
            .fetch_one(&state.db)
            .await?;
            if human_gate > 0 && !has_runnable_tasks(&state.db, &project.id).await? {
                if project.status.as_deref() != Some(STATUS_BLOCKED) {
                    sqlx::query(
                        "UPDATE projects SET status = CASE WHEN status IN \
                         ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
                         updated_at = ? WHERE id = ?",
                    )
                    .bind(STATUS_BLOCKED)
                    .bind(now())
                    .bind(&project.id)
                    .execute(&state.db)
                    .await?;
                }
                continue;
            }
            let blocked: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage != ? AND status = ? \
                 AND COALESCE(status, '') != ?",
            )
            .bind(&project.id)
            .bind(TASK_STAGE_COMPLETED)
            .bind(STATUS_BLOCKED)
            .bind(STATUS_CANCELED)
            .fetch_one(&state.db)
            .await?;
            let incomplete: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage != ? \
                 AND COALESCE(status, '') != ?",
            )
            .bind(&project.id)
            .bind(TASK_STAGE_COMPLETED)
            .bind(STATUS_CANCELED)
            .fetch_one(&state.db)
            .await?;
            if blocked > 0 && blocked == incomplete {
                sqlx::query(
                    "UPDATE projects SET status = CASE WHEN status IN \
                     ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
                     updated_at = ? WHERE id = ? AND stage = ?",
                )
                .bind(STATUS_BLOCKED)
                .bind(now())
                .bind(&project.id)
                .bind(&project.stage)
                .execute(&state.db)
                .await?;
            } else {
                sqlx::query(
                    "UPDATE projects SET status = ?, updated_at = ? WHERE id = ? AND status = ?",
                )
                .bind(STATUS_RUNNING)
                .bind(now())
                .bind(&project.id)
                .bind(STATUS_BLOCKED)
                .execute(&state.db)
                .await?;
            }
        }
    }
    crate::artifacts::recover(state).await;
    crate::replan::recover_cleanup(state).await;
    Ok(())
}

async fn cleanup_orphans(state: &AppState) {
    let running = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE status = 'running'")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    for job in running {
        let Some(project) = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
            .bind(&job.project_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None)
        else {
            continue;
        };
        kill_job_dir(&paths::job_dir(&PathBuf::from(&project.workspace), &job.id));
    }
}

async fn reconcile(state: &AppState) -> Result<Vec<String>> {
    crate::replan_review::reconcile_locked(state).await?;
    crate::queue::reconcile(&state.db).await?;
    let mut merge_spawns = Vec::new();
    let requests: Vec<(String, String)> = sqlx::query_as("SELECT project_id, agent_id FROM merge_requests WHERE state = 'waiting' ORDER BY requested_at")
        .fetch_all(&state.db).await?;
    for (id, agent_id) in requests {
        let project = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;
        if project.status.as_deref() == Some(STATUS_ARCHIVED) {
            continue;
        }
        if let Some(reason) = crate::delivery::gate(&state.db, &id, None).await? {
            sqlx::query("UPDATE merge_requests SET error = ? WHERE project_id = ?")
                .bind(reason)
                .bind(&id)
                .execute(&state.db)
                .await?;
            continue;
        }
        let Some(agent) = crate::db::load_agent(&state.db, &agent_id).await? else {
            continue;
        };
        if !crate::queue::candidate(&state.db, &project, None, "merge_conflict", &agent, true)
            .await?
            || running_jobs(state, &agent.id).await? >= agent.max_concurrency.max(1)
        {
            crate::queue::waiting(
                &state.db,
                &project,
                None,
                "merge_conflict",
                "由人工指定，等待 Agent 全局队列或并发释放",
            )
            .await?;
            continue;
        }
        let job = domain::new_id("job");
        sqlx::query("INSERT INTO jobs(id, project_id, role, agent_id, status, prompt, created_at) VALUES(?, ?, 'merge_conflict', ?, 'running', '', ?)")
            .bind(&job).bind(&id).bind(&agent.id).bind(now()).execute(&state.db).await?;
        sqlx::query("UPDATE merge_requests SET state = 'running', job_id = ?, error = NULL WHERE project_id = ?")
            .bind(&job).bind(&id).execute(&state.db).await?;
        crate::queue::dispatched(&state.db, &project, None, "merge_conflict", &job).await?;
        merge_spawns.push(job);
    }
    let projects = sqlx::query_as::<_, ProjectRow>(
        "SELECT * FROM projects WHERE status IN ('running', 'pausing') ORDER BY created_at ASC",
    )
    .fetch_all(&state.db)
    .await?;
    let mut spawns = merge_spawns;
    for project in projects {
        crate::db::ensure_project_agents(&state.db, &project.id).await?;
        if project.status.as_deref() == Some(STATUS_PAUSING) {
            let active = active_project_jobs(&state.db, &project.id).await?;
            if active == 0 {
                let _ = sqlx::query(
                    "UPDATE projects SET status = ?, updated_at = ? WHERE id = ? AND status = ?",
                )
                .bind(STATUS_PAUSED)
                .bind(now())
                .bind(&project.id)
                .bind(STATUS_PAUSING)
                .execute(&state.db)
                .await;
                emit(
                    &state.db,
                    &state.bus,
                    Some(&project.id),
                    None,
                    "project_paused",
                    "项目已暂停",
                )
                .await;
            }
            continue;
        }
        if let Some(reason) = crate::delivery::gate(&state.db, &project.id, None).await? {
            note_waiting(state, &project, &reason, false).await?;
            continue;
        }
        if crate::replan_review::advance_plans_locked(state, &project).await? {
            continue;
        }
        match project.stage.as_str() {
            STAGE_ANALYSIS => {
                if project.replan_active != 0
                    && crate::replan::running_non_analysis_jobs(&state.db, &project.id).await? > 0
                {
                    continue;
                }
                if has_project_job(&state.db, &project.id, JOB_ROLE_ANALYSIS).await? {
                    continue;
                }
                if !crate::runner::analysis_needs_dispatch(&state.db, &project).await? {
                    let last_role = latest_message_role(&state.db, &project.id).await?;
                    sqlx::query(
                        "UPDATE projects SET status = ?, blocked_reason = COALESCE(blocked_reason, ?), \
                         updated_at = ? WHERE id = ? AND status = ?",
                    )
                    .bind(STATUS_BLOCKED)
                    .bind(if last_role.is_some() {
                        "等待用户答复或确认方案"
                    } else {
                        "等待用户输入项目目标"
                    })
                    .bind(now())
                    .bind(&project.id)
                    .bind(STATUS_RUNNING)
                    .execute(&state.db)
                    .await?;
                    continue;
                }
                let choice = choose_agent(state, &project, ROLE_PLANNER, None).await?;
                let Some(agent) = choice.agent else {
                    note_waiting(
                        state,
                        &project,
                        choice.reason.as_deref().unwrap_or("等待可用的规划 Agent"),
                        choice.recoverable,
                    )
                    .await?;
                    continue;
                };
                if let Some(job_id) =
                    spawn_project_job(state, &project, &agent, JOB_ROLE_ANALYSIS, &choice.yielded)
                        .await?
                {
                    spawns.push(job_id);
                }
            }
            STAGE_EXECUTING => {
                for request in crate::replan_review::pending(&state.db, &project.id).await? {
                    if let Some(job_id) = spawn_replan_review(state, &project, &request).await? {
                        spawns.push(job_id);
                    }
                }
                if project.completion_mode == COMPLETION_DIRECT {
                    if has_project_job(&state.db, &project.id, JOB_ROLE_DIRECT).await? {
                        continue;
                    }
                    let choice = choose_agent(state, &project, ROLE_PLANNER, None).await?;
                    let Some(agent) = choice.agent else {
                        note_waiting(
                            state,
                            &project,
                            choice
                                .reason
                                .as_deref()
                                .unwrap_or("等待可用的直接完成 Agent"),
                            choice.recoverable,
                        )
                        .await?;
                        continue;
                    };
                    if let Some(job_id) =
                        spawn_project_job(state, &project, &agent, JOB_ROLE_DIRECT, &choice.yielded)
                            .await?
                    {
                        spawns.push(job_id);
                    }
                    continue;
                }
                let tasks = sqlx::query_as::<_, TaskRow>(
                    "SELECT * FROM tasks WHERE project_id = ? ORDER BY queue_order ASC, seq ASC",
                )
                .bind(&project.id)
                .fetch_all(&state.db)
                .await?
                .into_iter()
                .filter(|task| task.status.as_deref() != Some(STATUS_CANCELED))
                .collect::<Vec<_>>();
                for task in &tasks {
                    if task.status.as_deref() == Some(STATUS_PAUSING)
                        && !task_has_running_job(&state.db, &task.id).await?
                    {
                        sqlx::query(
                            "UPDATE tasks SET status = ?, updated_at = ? \
                             WHERE id = ? AND status = ?",
                        )
                        .bind(STATUS_PAUSED)
                        .bind(now())
                        .bind(&task.id)
                        .bind(STATUS_PAUSING)
                        .execute(&state.db)
                        .await?;
                        emit(
                            &state.db,
                            &state.bus,
                            Some(&project.id),
                            Some(&task.id),
                            "task_paused",
                            format!("任务 {} 已在途作业结束，进入已暂停", task.key),
                        )
                        .await;
                    }
                }
                let completed: HashSet<String> = tasks
                    .iter()
                    .filter(|task| task.stage == TASK_STAGE_COMPLETED)
                    .map(|task| task.key.clone())
                    .collect();
                if !tasks.is_empty() && completed.len() == tasks.len() {
                    sqlx::query(
                        "UPDATE projects SET stage = ?, status = CASE WHEN status IN \
                         ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
                         blocked_reason = NULL, acceptance = NULL, acceptance_passed = 0, \
                         acceptance_at = NULL, updated_at = ? WHERE id = ?",
                    )
                    .bind(STAGE_SUMMARIZING)
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
                        "summarizing",
                        "全部任务完成，开始项目验收",
                    )
                    .await;
                    let choice = choose_agent(state, &project, ROLE_PLANNER, None).await?;
                    if let Some(agent) = choice.agent {
                        if let Some(job_id) = spawn_project_job(
                            state,
                            &project,
                            &agent,
                            JOB_ROLE_ACCEPTANCE,
                            &choice.yielded,
                        )
                        .await?
                        {
                            spawns.push(job_id);
                        }
                    }
                    continue;
                }
                for task in &tasks {
                    if crate::replan_review::active(&state.db, &task.id)
                        .await?
                        .is_some()
                        || crate::replan_review::holds_task(&state.db, task).await?
                    {
                        continue;
                    }
                    if task.status.as_deref() == Some(STATUS_BLOCKED)
                        || task.stage == TASK_STAGE_COMPLETED
                    {
                        continue;
                    }
                    if task.status.as_deref() != Some(STATUS_QUEUED)
                        && !(task.stage == TASK_STAGE_PENDING && task.status.is_none())
                    {
                        continue;
                    }
                    let deps = parse_list(&task.deps);
                    let ready =
                        task.stage != TASK_STAGE_PENDING || deps_complete(&deps, &completed);
                    if !ready {
                        sqlx::query(
                            "UPDATE tasks SET status = NULL WHERE id = ? AND status = 'queued'",
                        )
                        .bind(&task.id)
                        .execute(&state.db)
                        .await?;
                        let missing = deps
                            .iter()
                            .filter(|dep| !completed.contains(*dep))
                            .cloned()
                            .collect::<Vec<_>>();
                        set_task_waiting(
                            state,
                            &task.id,
                            &format!("等待前置任务：{}", missing.join("、")),
                        )
                        .await?;
                        continue;
                    }
                    if task.stage == TASK_STAGE_PENDING {
                        sqlx::query("UPDATE tasks SET stage = 'executing', status = 'queued', updated_at = ? \
                            WHERE id = ? AND stage = 'pending' AND (status IS NULL OR status = 'queued')")
                            .bind(now()).bind(&task.id).execute(&state.db).await?;
                    }
                    let role = if task.stage == TASK_STAGE_VERIFYING {
                        ROLE_VERIFIER
                    } else {
                        ROLE_EXECUTOR
                    };
                    if role == ROLE_EXECUTOR {
                        if let Some(reason) =
                            crate::delivery::gate(&state.db, &project.id, Some(&task.id)).await?
                        {
                            let reason = format!(
                                "{reason}{}",
                                task.last_error
                                    .as_ref()
                                    .map(|error| format!("；上次失败：{error}"))
                                    .unwrap_or_default()
                            );
                            sqlx::query("UPDATE tasks SET status = 'blocked', last_error = ?, updated_at = ? WHERE id = ?")
                                .bind(&reason).bind(now()).bind(&task.id).execute(&state.db).await?;
                            emit(
                                &state.db,
                                &state.bus,
                                Some(&project.id),
                                Some(&task.id),
                                "task_blocked",
                                &reason,
                            )
                            .await;
                            continue;
                        }
                    }
                    if has_task_job(&state.db, &task.id, role).await? {
                        continue;
                    }
                    if let Some(job_id) =
                        orphan_task_process(&state.db, &project.workspace, &task.id).await?
                    {
                        set_task_waiting(
                            state,
                            &task.id,
                            &format!("等待上一个 Agent 进程退出（{}）", short_id(&job_id)),
                        )
                        .await?;
                        continue;
                    }
                    // 普通工作区没有隔离副本，同一项目同一时刻只推进一个任务。
                    if !project.is_git() && active_project_jobs(&state.db, &project.id).await? > 0 {
                        set_task_waiting(state, &task.id, "普通工作区按顺序推进任务").await?;
                        continue;
                    }
                    let choice = choose_agent(state, &project, role, Some(task)).await?;
                    let Some(agent) = choice.agent else {
                        if task.status.as_deref() != Some(STATUS_QUEUED) {
                            sqlx::query("UPDATE tasks SET status = ?, updated_at = ? WHERE id = ?")
                                .bind(STATUS_QUEUED)
                                .bind(now())
                                .bind(&task.id)
                                .execute(&state.db)
                                .await?;
                        }
                        let reason = choice
                            .reason
                            .clone()
                            .unwrap_or_else(|| format!("等待可用的{} Agent", role_label(role)));
                        set_task_waiting(state, &task.id, &reason).await?;
                        crate::queue::waiting(&state.db, &project, Some(task), role, &reason)
                            .await?;
                        note_waiting(state, &project, &reason, true).await?;
                        if !choice.recoverable {
                            sqlx::query("UPDATE tasks SET status = 'blocked', last_failure_kind = 'configuration', last_error = ?, updated_at = ? WHERE id = ?")
                                .bind(&reason).bind(now()).bind(&task.id).execute(&state.db).await?;
                        }
                        continue;
                    };
                    if let Some(job_id) =
                        spawn_task_job(state, &project, task, &agent, role, &choice.yielded).await?
                    {
                        spawns.push(job_id);
                    }
                }
                recompute_blocked(state, &project.id).await?;
            }
            STAGE_SUMMARIZING => {
                let (role, waiting) = if project.acceptance_passed == 0 {
                    if project.acceptance.is_some() {
                        let reason = "项目验收未通过，等待重规划或人工裁决";
                        sqlx::query(
                            "UPDATE projects SET status = CASE WHEN status IN \
                             ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
                             blocked_reason = COALESCE(blocked_reason, ?), updated_at = ? WHERE id = ?",
                        )
                        .bind(STATUS_BLOCKED)
                        .bind(reason)
                        .bind(now())
                        .bind(&project.id)
                        .execute(&state.db)
                        .await?;
                        continue;
                    }
                    (JOB_ROLE_ACCEPTANCE, "等待可用的项目验收 Agent")
                } else {
                    (JOB_ROLE_SUMMARY, "等待可用的总结 Agent")
                };
                if has_project_job(&state.db, &project.id, role).await? {
                    continue;
                }
                let choice = choose_agent(state, &project, ROLE_PLANNER, None).await?;
                let Some(agent) = choice.agent else {
                    note_waiting(
                        state,
                        &project,
                        choice.reason.as_deref().unwrap_or(waiting),
                        choice.recoverable,
                    )
                    .await?;
                    continue;
                };
                if let Some(job_id) =
                    spawn_project_job(state, &project, &agent, role, &choice.yielded).await?
                {
                    spawns.push(job_id);
                }
            }
            _ => {}
        }
    }
    Ok(spawns)
}

async fn recompute_blocked(state: &AppState, project_id: &str) -> Result<()> {
    let reviews: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM replan_requests WHERE project_id = ? AND state IN ('pending', 'running', 'planned')")
        .bind(project_id).fetch_one(&state.db).await?;
    if reviews > 0 {
        return Ok(());
    }
    let active: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE project_id = ? AND status = 'running'")
            .bind(project_id)
            .fetch_one(&state.db)
            .await?;
    if active > 0 {
        return Ok(());
    }
    let all_tasks =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? ORDER BY seq ASC")
            .bind(project_id)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .filter(|task| task.status.as_deref() != Some(STATUS_CANCELED))
            .collect::<Vec<_>>();
    let tasks: Vec<&TaskRow> = all_tasks
        .iter()
        .filter(|task| task.stage != TASK_STAGE_COMPLETED)
        .collect();
    if tasks.is_empty() {
        return Ok(());
    }
    let pending_tasks: Vec<&TaskRow> = tasks
        .iter()
        .copied()
        .filter(|task| {
            !matches!(
                task.status.as_deref(),
                Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
            )
        })
        .collect();
    if pending_tasks.is_empty() {
        // User-paused tasks are intentional stops; they must not turn the
        // project into a blocked state that needs manual arbitration.
        sqlx::query(
            "UPDATE projects SET status = CASE WHEN status = ? THEN ? ELSE status END, \
             blocked_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(STATUS_BLOCKED)
        .bind(STATUS_RUNNING)
        .bind(now())
        .bind(project_id)
        .execute(&state.db)
        .await?;
        return Ok(());
    }
    let completed: HashSet<String> = all_tasks
        .iter()
        .filter(|task| task.stage == TASK_STAGE_COMPLETED)
        .map(|task| task.key.clone())
        .collect();
    let runnable = pending_tasks.iter().any(|task| {
        if task.status.as_deref() == Some(STATUS_BLOCKED) {
            return false;
        }
        if task.stage == TASK_STAGE_PENDING {
            return deps_complete(&parse_list(&task.deps), &completed);
        }
        matches!(
            task.stage.as_str(),
            TASK_STAGE_EXECUTING | TASK_STAGE_VERIFYING
        )
    });
    if runnable {
        return Ok(());
    }
    let reason = pending_tasks
        .iter()
        .find_map(|task| task.last_error.clone())
        .unwrap_or_else(|| "没有可自动推进的任务".into());
    sqlx::query("UPDATE projects SET status = ?, blocked_reason = ?, updated_at = ? WHERE id = ?")
        .bind(STATUS_BLOCKED)
        .bind(&reason)
        .bind(now())
        .bind(project_id)
        .execute(&state.db)
        .await?;
    emit(
        &state.db,
        &state.bus,
        Some(project_id),
        None,
        "project_blocked",
        format!("项目阻塞：{reason}"),
    )
    .await;
    Ok(())
}

async fn has_runnable_tasks(pool: &SqlitePool, project_id: &str) -> Result<bool> {
    let tasks = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ?")
        .bind(project_id)
        .fetch_all(pool)
        .await?;
    let completed = tasks
        .iter()
        .filter(|task| task.stage == TASK_STAGE_COMPLETED)
        .map(|task| task.key.clone())
        .collect();
    Ok(tasks.iter().any(|task| {
        matches!(task.status.as_deref(), Some(STATUS_RUNNING | STATUS_QUEUED))
            && task.stage != TASK_STAGE_COMPLETED
            && (task.stage != TASK_STAGE_PENDING
                || deps_complete(&parse_list(&task.deps), &completed))
    }))
}

async fn choose_agent(
    state: &AppState,
    project: &ProjectRow,
    role: &str,
    task: Option<&TaskRow>,
) -> Result<AgentChoice> {
    choose_agent_for(state, project, role, task, role, task).await
}

/// Pool ownership and queue ownership differ for control jobs: a child owns
/// the request, but its parent owns the executor pool used for the decision.
async fn choose_agent_for(
    state: &AppState,
    project: &ProjectRow,
    role: &str,
    task: Option<&TaskRow>,
    queue_role: &str,
    queue_task: Option<&TaskRow>,
) -> Result<AgentChoice> {
    crate::queue::enter(&state.db, project, queue_task, queue_role).await?;
    if let Some(task) = task {
        let target: Option<String> = sqlx::query_scalar(
            "SELECT agent_id FROM retry_requests WHERE task_id = ? AND role = ?",
        )
        .bind(&task.id)
        .bind(role)
        .fetch_optional(&state.db)
        .await?;
        if let Some(id) = target {
            if let Some(agent) = crate::db::load_agent(&state.db, &id).await? {
                let member: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_agents t WHERE t.task_id = ? AND t.role = ? AND t.agent_id = ? AND t.enabled = 1 AND NOT EXISTS (SELECT 1 FROM project_agents p WHERE p.project_id = ? AND p.role = t.role AND p.agent_id = t.agent_id AND p.enabled = 0)")
                    .bind(&task.id).bind(role).bind(&id).bind(&project.id).fetch_one(&state.db).await?;
                if member == 0 {
                    return Ok(AgentChoice {
                        agent: None,
                        recoverable: false,
                        reason: Some(
                            "原作业 Agent 在任务或项目池中已停用，请修复配置或人工换人".into(),
                        ),
                        yielded: Vec::new(),
                    });
                }
                if agent.enabled == 0 || !parse_list(&agent.roles).iter().any(|r| r == role) {
                    return Ok(AgentChoice {
                        agent: None,
                        recoverable: false,
                        reason: Some("原作业 Agent 配置不可用，请更换 Agent 或修复配置".into()),
                        yielded: Vec::new(),
                    });
                }
                let gate = agent_gate(state, &agent).await?;
                let ordered = crate::queue::candidate(
                    &state.db, project, queue_task, queue_role, &agent, false,
                )
                .await?;
                if gate.is_some()
                    || !ordered
                    || running_jobs(state, &id).await? >= agent.max_concurrency.max(1)
                {
                    return Ok(AgentChoice {
                        agent: None,
                        recoverable: true,
                        reason: Some(format!(
                            "原作业继续等待 Agent {} 的可用时段、全局队列或并发释放",
                            agent.name
                        )),
                        yielded: Vec::new(),
                    });
                }
                return Ok(AgentChoice {
                    agent: Some(agent),
                    recoverable: true,
                    reason: None,
                    yielded: Vec::new(),
                });
            }
        }
    }
    if let Some(task) = task {
        if let Some(forced_id) = task.forced_agent_id.as_deref() {
            if task.forced_agent_role.as_deref() == Some(role) {
                let Some(agent) = crate::db::load_agent(&state.db, forced_id).await? else {
                    sqlx::query(
                        "UPDATE tasks SET forced_agent_id = NULL, forced_agent_role = NULL, \
                         updated_at = ? WHERE id = ?",
                    )
                    .bind(now())
                    .bind(&task.id)
                    .execute(&state.db)
                    .await?;
                    return Ok(AgentChoice {
                        agent: None,
                        recoverable: false,
                        reason: Some(format!(
                            "人工指定的{} Agent 已被删除，请重新裁决或调整项目 Agent 池",
                            role_label(role)
                        )),
                        yielded: Vec::new(),
                    });
                };
                // 人工指定的 Agent 只等待自身释放，不读取让位配置。
                if !crate::queue::candidate(
                    &state.db, project, queue_task, queue_role, &agent, true,
                )
                .await?
                {
                    return Ok(AgentChoice {
                        agent: None,
                        recoverable: true,
                        reason: Some("等待人工指定 Agent 的全局队列前序作业".into()),
                        yielded: Vec::new(),
                    });
                }
                let running = running_jobs(state, &agent.id).await?;
                if running < agent.max_concurrency.max(1) {
                    return Ok(AgentChoice {
                        agent: Some(agent),
                        recoverable: true,
                        reason: None,
                        yielded: Vec::new(),
                    });
                }
                return Ok(AgentChoice {
                    agent: None,
                    recoverable: true,
                    reason: Some(format!(
                        "等待人工指定的{} Agent {} 释放（并发 {running}/{}）",
                        role_label(role),
                        agent.name,
                        agent.max_concurrency.max(1)
                    )),
                    yielded: Vec::new(),
                });
            }
        }
    }

    // 执行者与验收者从任务专用 Agent 池按严格优先级选人：池为空或全不可用
    // 时只等待，不回退到项目池；调度只读取任务池。规划者使用项目池。
    let (mut rows, scope) = if role == ROLE_EXECUTOR || role == ROLE_VERIFIER {
        let Some(task) = task else {
            return Ok(AgentChoice {
                agent: None,
                recoverable: false,
                reason: Some(format!(
                    "{}角色只提供给任务使用，缺少任务上下文",
                    role_label(role)
                )),
                yielded: Vec::new(),
            });
        };
        let rows = sqlx::query_as::<_, TaskAgentRow>(
            "SELECT * FROM task_agents WHERE task_id = ? AND role = ? \
             ORDER BY priority ASC, agent_id ASC",
        )
        .bind(&task.id)
        .bind(role)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|row| row.into_project_row())
        .collect::<Vec<_>>();
        (rows, "任务 Agent 池")
    } else {
        crate::db::ensure_project_agents(&state.db, &project.id).await?;
        (
            sqlx::query_as::<_, ProjectAgentRow>(
                "SELECT * FROM project_agents WHERE project_id = ? AND role = ? \
                 ORDER BY priority ASC, agent_id ASC",
            )
            .bind(&project.id)
            .bind(role)
            .fetch_all(&state.db)
            .await?,
            "项目 Agent 池",
        )
    };
    if queue_role == JOB_ROLE_REPLAN_REVIEW {
        if let Some(preferred) =
            task.and_then(|task| task.agent_id.as_ref().or(task.last_agent_id.as_ref()))
        {
            rows.sort_by_key(|row| row.agent_id != *preferred);
        }
    }
    if rows.is_empty() {
        let hint = if scope == "任务 Agent 池" {
            "任务池在创建时从项目池快照，请检查项目池该角色配置；调整项目池后需新建任务"
        } else {
            "请在项目页配置"
        };
        return Ok(AgentChoice {
            agent: None,
            recoverable: false,
            reason: Some(format!("{scope}没有{}角色 Agent，{hint}", role_label(role))),
            yielded: Vec::new(),
        });
    }
    // 任务池是项目池的快照，但项目池的停用状态是更高层约束：
    // 项目池中同一角色停用后，已有任务也不再派发该 Agent。项目池缺少条目时
    // 不阻塞任务池，避免历史快照中的 Agent 因成员清理而失效。
    let project_disabled: HashSet<String> = if scope == "任务 Agent 池" {
        sqlx::query_as::<_, (String,)>(
            "SELECT agent_id FROM project_agents \
             WHERE project_id = ? AND role = ? AND enabled = 0",
        )
        .bind(&project.id)
        .bind(role)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|(agent_id,)| agent_id)
        .collect()
    } else {
        HashSet::new()
    };
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
        .fetch_all(&state.db)
        .await?;
    let counts_rows: Vec<(Option<String>, i64)> = sqlx::query_as(
        "SELECT agent_id, COUNT(*) FROM jobs WHERE status = 'running' GROUP BY agent_id",
    )
    .fetch_all(&state.db)
    .await?;
    let counts: HashMap<String, i64> = counts_rows
        .into_iter()
        .filter_map(|(id, count)| id.map(|id| (id, count)))
        .collect();
    let now = Utc::now();
    let avoided = task
        .filter(|task| task.avoid_agent_role.as_deref() == Some(role))
        .and_then(|task| task.avoid_agent_id.as_deref())
        .or_else(|| {
            // Project jobs carry the skip state on the project row.
            (task.is_none() && project.avoid_agent_role.as_deref() == Some(role))
                .then_some(project.avoid_agent_id.as_deref())
                .flatten()
        });
    // A manual retry of a blocked job names the agent that should get the next
    // dispatch for this role. It bypasses that agent's cooldown once; the
    // backoff is cleared only when the dispatch actually succeeds.
    let retry_target = match task {
        Some(task) if task.retry_role.as_deref() == Some(role) => task.retry_agent_id.as_deref(),
        Some(_) => None,
        None if project.retry_role.as_deref() == Some(role) => project.retry_agent_id.as_deref(),
        None => None,
    };
    let mut unavailable = Vec::new();
    let mut recoverable = false;
    // 并发已满但开启让位配置的 Agent：名称与并发占用。它们把本次派发让给
    // 后续优先级的 Agent，派发成功时用于事件说明，全部满额时用于等待原因。
    let mut yielded: Vec<(String, i64, i64)> = Vec::new();
    for row in &rows {
        let Some(agent) = agents.iter().find(|agent| agent.id == row.agent_id) else {
            let name = if row.agent_name.trim().is_empty() {
                row.agent_id.clone()
            } else {
                row.agent_name.clone()
            };
            unavailable.push(format!("「{name}」（已删除）"));
            continue;
        };
        if !parse_list(&agent.roles).iter().any(|item| item == role) {
            unavailable.push(format!("「{}」（已不具备该角色）", agent.name));
            continue;
        }
        if row.enabled == 0 {
            unavailable.push(format!("「{}」（{}中已禁用）", agent.name, scope));
            continue;
        }
        if agent.enabled == 0 {
            unavailable.push(format!("「{}」（全局已禁用）", agent.name));
            continue;
        }
        if project_disabled.contains(&agent.id) {
            unavailable.push(format!("「{}」（项目 Agent 池已停用）", agent.name));
            continue;
        }
        if avoided == Some(agent.id.as_str())
            && rows.iter().any(|other| {
                other.agent_id != agent.id
                    && other.enabled != 0
                    && agents
                        .iter()
                        .any(|candidate| candidate.id == other.agent_id && candidate.enabled != 0)
            })
        {
            unavailable.push(format!("「{}」（本次失败后暂不重试）", agent.name));
            continue;
        }
        let cooling_down = agent
            .cooldown_until
            .as_deref()
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|until| until.with_timezone(&Utc) > now)
            .unwrap_or(false);
        if cooling_down && retry_target != Some(agent.id.as_str()) {
            recoverable = true;
            let until = agent.cooldown_until.as_deref().unwrap_or_default();
            unavailable.push(format!("「{}」（冷却中，至 {until}）", agent.name));
            continue;
        }
        match agent_gate(state, agent).await? {
            Some(usage::AgentGate::Schedule(reason)) => {
                recoverable = true;
                unavailable.push(format!("「{}」（{reason}）", agent.name));
                continue;
            }
            Some(usage::AgentGate::Limit(reason)) => {
                recoverable = true;
                unavailable.push(format!("「{}」（{reason}）", agent.name));
                continue;
            }
            None => {}
        }
        // Strict priority: the first usable agent owns this dispatch. When it
        // is at capacity the task waits instead of falling through to a lower
        // priority agent, unless that agent lets lower priorities take over.
        if !crate::queue::candidate(&state.db, project, queue_task, queue_role, agent, false)
            .await?
        {
            return Ok(AgentChoice {
                agent: None,
                recoverable: true,
                reason: Some(format!("等待 Agent {} 的全局队列前序作业", agent.name)),
                yielded: Vec::new(),
            });
        }
        let running = counts.get(&agent.id).copied().unwrap_or(0);
        let limit = agent.max_concurrency.max(1);
        if running < limit {
            // Hand the dispatch over to this agent and report the higher
            // priority agents that yielded to it for the event message.
            return Ok(AgentChoice {
                agent: Some(agent.clone()),
                recoverable: true,
                reason: None,
                yielded: yielded.into_iter().map(|(name, _, _)| name).collect(),
            });
        }
        if agent.allow_priority_fallback == 0 {
            return Ok(AgentChoice {
                agent: None,
                recoverable: true,
                reason: Some(format!(
                    "等待 Agent {} 释放（并发 {running}/{limit}）",
                    agent.name
                )),
                yielded: Vec::new(),
            });
        }
        yielded.push((agent.name.clone(), running, limit));
    }
    if !yielded.is_empty() {
        let detail = yielded
            .iter()
            .map(|(name, running, limit)| format!("{name}（并发 {running}/{limit}）"))
            .collect::<Vec<_>>()
            .join("、");
        return Ok(AgentChoice {
            agent: None,
            recoverable: true,
            reason: Some(format!(
                "{scope}中{} Agent 并发已满且允许让位，暂无后续优先级可接手：{detail}",
                role_label(role)
            )),
            yielded: Vec::new(),
        });
    }
    Ok(AgentChoice {
        agent: None,
        recoverable,
        reason: Some(format!(
            "{scope}没有可用的{} Agent（不可用：{}）",
            role_label(role),
            unavailable.join("、")
        )),
        yielded: Vec::new(),
    })
}

async fn spawn_replan_review(
    state: &AppState,
    project: &ProjectRow,
    request: &crate::replan_review::ReviewRequest,
) -> Result<Option<String>> {
    let task = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
        .bind(&request.task_id)
        .fetch_one(&state.db)
        .await?;
    if matches!(
        task.status.as_deref(),
        Some(STATUS_PAUSED | STATUS_PAUSING | STATUS_CANCELED)
    ) || task_has_running_job(&state.db, &task.id).await?
    {
        return Ok(None);
    }
    let parent = if let Some(parent_id) = &request.reviewer_task_id {
        let parent = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
            .bind(parent_id)
            .fetch_one(&state.db)
            .await?;
        if matches!(
            parent.status.as_deref(),
            Some(STATUS_PAUSED | STATUS_PAUSING | STATUS_CANCELED)
        ) || task_has_running_job(&state.db, parent_id).await?
        {
            return Ok(None);
        }
        Some(parent)
    } else {
        None
    };
    // Serialize decisions by the same parent/planner, but do not wait for child
    // dependencies to complete: that would deadlock the request being reviewed.
    let busy: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM replan_requests WHERE project_id = ? AND reviewer_task_id IS ? AND state = 'running'")
        .bind(&project.id).bind(&request.reviewer_task_id).fetch_one(&state.db).await?;
    if busy > 0 {
        return Ok(None);
    }
    let pool_role = if parent.is_some() {
        ROLE_EXECUTOR
    } else {
        ROLE_PLANNER
    };
    let choice = choose_agent_for(
        state,
        project,
        pool_role,
        parent.as_ref(),
        JOB_ROLE_REPLAN_REVIEW,
        Some(&task),
    )
    .await?;
    let Some(agent) = choice.agent else {
        let reason = format!(
            "重规划审批：{}",
            choice.reason.as_deref().unwrap_or("等待审批 Agent")
        );
        crate::replan_review::set_waiting(state, request, &reason).await?;
        crate::queue::waiting(
            &state.db,
            project,
            Some(&task),
            JOB_ROLE_REPLAN_REVIEW,
            &reason,
        )
        .await?;
        if !choice.recoverable {
            sqlx::query("UPDATE replan_requests SET state = 'waiting', guidance = ?, updated_at = ? WHERE id = ? AND state = 'pending'")
                .bind(&reason).bind(now()).bind(&request.id).execute(&state.db).await?;
            emit(
                &state.db,
                &state.bus,
                Some(&project.id),
                Some(&task.id),
                "replan_review_waiting",
                &reason,
            )
            .await;
        }
        return Ok(None);
    };
    let job_id = domain::new_id("job");
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("INSERT INTO jobs (id, project_id, task_id, role, agent_id, status, attempt, prompt, replan_generation, created_at) VALUES (?, ?, ?, ?, ?, 'running', ?, '', ?, ?)")
        .bind(&job_id).bind(&project.id).bind(&task.id).bind(JOB_ROLE_REPLAN_REVIEW).bind(&agent.id)
        .bind(request.attempts + 1).bind(project.replan_generation).bind(now()).execute(&mut *tx).await?;
    sqlx::query("UPDATE replan_requests SET state = 'running', review_job_id = ?, attempts = attempts + 1, updated_at = ? WHERE id = ? AND state = 'pending'")
        .bind(&job_id).bind(now()).bind(&request.id).execute(&mut *tx).await?;
    tx.commit().await?;
    clear_waiting(&state.db, &project.id).await?;
    crate::queue::dispatched(
        &state.db,
        project,
        Some(&task),
        JOB_ROLE_REPLAN_REVIEW,
        &job_id,
    )
    .await?;
    emit_job(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        Some(&job_id),
        "job_queued",
        format!(
            "任务 {} 的重规划审批已派发给{} {}{}",
            task.key,
            if parent.is_some() {
                "父任务执行者"
            } else {
                "项目规划者"
            },
            agent.name,
            yield_note(&choice.yielded)
        ),
    )
    .await;
    Ok(Some(job_id))
}

struct AgentChoice {
    agent: Option<AgentRow>,
    recoverable: bool,
    reason: Option<String>,
    /// 并发已满、开启让位配置并把本次派发让给该 Agent 的名称列表（按优先级
    /// 顺序）。派发事件据此说明“按让位配置调度”。
    yielded: Vec<String>,
}

async fn agent_gate(state: &AppState, agent: &AgentRow) -> Result<Option<usage::AgentGate>> {
    usage::agent_gate(
        &state.db,
        &agent.id,
        agent.availability_json.as_deref(),
        agent.usage_limit_json.as_deref(),
        agent.usage_reset_at.as_deref(),
        Utc::now(),
    )
    .await
    .map_err(anyhow::Error::from)
    .context("读取 Agent 用量失败")
}

async fn running_jobs(state: &AppState, agent_id: &str) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE agent_id = ? AND status = 'running'")
            .bind(agent_id)
            .fetch_one(&state.db)
            .await?,
    )
}

async fn spawn_task_job(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    agent: &AgentRow,
    role: &str,
    yielded: &[String],
) -> Result<Option<String>> {
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Ok(None);
    }
    let resume: Option<String> = sqlx::query_scalar(
        "SELECT job_id FROM retry_requests WHERE task_id = ? AND role = ? AND agent_id = ?",
    )
    .bind(&task.id)
    .bind(role)
    .bind(&agent.id)
    .fetch_optional(&state.db)
    .await?;
    let job_id = resume.clone().unwrap_or_else(|| domain::new_id("job"));
    let attempt = if role == ROLE_VERIFIER {
        task.rejections + 1
    } else {
        task.attempts + task.rejections + 1
    };
    let forced = task.forced_agent_id.as_deref() == Some(agent.id.as_str())
        && task.forced_agent_role.as_deref() == Some(role);
    let retry = task.retry_role.as_deref() == Some(role)
        && task.retry_agent_id.as_deref() == Some(agent.id.as_str());
    let timestamp = now();
    if resume.is_some() {
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        if !crate::api::mark_job_resumed(&mut tx, &job_id).await? {
            return Ok(None);
        }
        sqlx::query("DELETE FROM retry_requests WHERE task_id = ?")
            .bind(&task.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    } else {
        let execution_job_id: Option<String> = if role == ROLE_VERIFIER {
            Some(sqlx::query_scalar(
                "SELECT id FROM jobs WHERE task_id = ? AND role = 'executor' ORDER BY rowid DESC LIMIT 1",
            ).bind(&task.id).fetch_one(&state.db).await?)
        } else {
            None
        };
        sqlx::query(
            "INSERT INTO jobs (id, project_id, task_id, role, agent_id, status, attempt, prompt, \
         replan_generation, created_at, execution_job_id) VALUES (?, ?, ?, ?, ?, ?, ?, '', ?, ?, ?)",
        )
        .bind(&job_id)
        .bind(&project.id)
        .bind(&task.id)
        .bind(role)
        .bind(&agent.id)
        .bind(JOB_STATUS_RUNNING)
        .bind(attempt)
        .bind(if project.goal_locked_at.is_some() {
            project.replan_generation
        } else {
            0
        })
        .bind(&timestamp)
        .bind(execution_job_id)
        .execute(&state.db)
        .await?;
    }
    let dispatched = sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, agent_id = ?, last_error = NULL, \
         waiting_reason = NULL, forced_agent_id = NULL, forced_agent_role = NULL, \
         retry_agent_id = NULL, retry_role = NULL, \
         avoid_agent_id = CASE WHEN avoid_agent_role = ? THEN NULL ELSE avoid_agent_id END, \
         avoid_agent_role = CASE WHEN avoid_agent_role = ? THEN NULL ELSE avoid_agent_role END, \
         updated_at = ? \
         WHERE id = ? AND status = ?",
    )
    .bind(if role == ROLE_VERIFIER {
        TASK_STAGE_VERIFYING
    } else {
        TASK_STAGE_EXECUTING
    })
    .bind(STATUS_RUNNING)
    .bind(&agent.id)
    .bind(role)
    .bind(role)
    .bind(&timestamp)
    .bind(&task.id)
    .bind(STATUS_QUEUED)
    .execute(&state.db)
    .await?;
    if (retry || resume.is_some()) && dispatched.rows_affected() > 0 {
        crate::db::clear_agent_backoff(&state.db, &agent.id).await?;
    }
    clear_waiting(&state.db, &project.id).await?;
    crate::queue::dispatched(&state.db, project, Some(task), role, &job_id).await?;
    emit_job(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        Some(&job_id),
        "job_queued",
        format!(
            "{} 作业已派发给 {}（任务 {}）{}{}",
            role_label(role),
            agent.name,
            task.key,
            if forced { "，由人工指定" } else { "" },
            yield_note(yielded)
        ),
    )
    .await;
    Ok(Some(job_id))
}

async fn spawn_project_job(
    state: &AppState,
    project: &ProjectRow,
    agent: &AgentRow,
    role: &str,
    yielded: &[String],
) -> Result<Option<String>> {
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Ok(None);
    }
    let job_id = domain::new_id("job");
    let retry = project.retry_role.as_deref() == Some(ROLE_PLANNER)
        && project.retry_agent_id.as_deref() == Some(agent.id.as_str());
    let timestamp = now();
    sqlx::query(
        "INSERT INTO jobs (id, project_id, task_id, role, agent_id, status, attempt, prompt, \
         replan_generation, created_at) VALUES (?, ?, NULL, ?, ?, ?, 1, '', ?, ?)",
    )
    .bind(&job_id)
    .bind(&project.id)
    .bind(role)
    .bind(&agent.id)
    .bind(JOB_STATUS_RUNNING)
    .bind(if project.goal_locked_at.is_some() {
        project.replan_generation
    } else {
        0
    })
    .bind(&timestamp)
    .execute(&state.db)
    .await?;
    let dispatched = sqlx::query(
        "UPDATE projects SET avoid_agent_id = CASE WHEN avoid_agent_role = ? THEN NULL \
         ELSE avoid_agent_id END, \
         avoid_agent_role = CASE WHEN avoid_agent_role = ? THEN NULL ELSE avoid_agent_role END, \
         retry_agent_id = NULL, retry_role = NULL, \
         updated_at = ? WHERE id = ?",
    )
    .bind(ROLE_PLANNER)
    .bind(ROLE_PLANNER)
    .bind(&timestamp)
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    if retry && dispatched.rows_affected() > 0 {
        crate::db::clear_agent_backoff(&state.db, &agent.id).await?;
    }
    clear_waiting(&state.db, &project.id).await?;
    crate::queue::dispatched(&state.db, project, None, ROLE_PLANNER, &job_id).await?;
    emit_job(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        Some(&job_id),
        "job_queued",
        format!(
            "{} 作业已派发给 {}{}",
            role_label(role),
            agent.name,
            yield_note(yielded)
        ),
    )
    .await;
    Ok(Some(job_id))
}

/// 派发事件末尾的让位说明，列出被跳过的并发已满 Agent；无让位时为空。
fn yield_note(yielded: &[String]) -> String {
    if yielded.is_empty() {
        String::new()
    } else {
        format!("（{} 并发已满，按让位配置调度）", yielded.join("、"))
    }
}

async fn has_task_job(pool: &SqlitePool, task_id: &str, role: &str) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE task_id = ? AND role = ? AND status = 'running'",
    )
    .bind(task_id)
    .bind(role)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

/// A failed job can leave its process tree running. Starting a replacement in
/// the same task worktree would let both agents edit the same files, so
/// terminate the old tree and hold the task until the pid is gone.
async fn orphan_task_process(
    pool: &SqlitePool,
    workspace: &str,
    task_id: &str,
) -> Result<Option<String>> {
    let job_ids = sqlx::query_scalar::<_, String>(
        "SELECT id FROM jobs WHERE task_id = ? AND status IN (?, ?) \
         ORDER BY created_at DESC LIMIT 5",
    )
    .bind(task_id)
    .bind(JOB_STATUS_FAILED)
    .bind(JOB_STATUS_CANCELED)
    .fetch_all(pool)
    .await?;
    for job_id in job_ids {
        let job_dir = paths::job_dir(&PathBuf::from(workspace), &job_id);
        if !crate::agents::job_process_alive(&job_dir) {
            continue;
        }
        crate::agents::kill_job_dir(&job_dir);
        if crate::agents::job_process_alive(&job_dir) {
            return Ok(Some(job_id));
        }
    }
    Ok(None)
}

async fn task_has_running_job(pool: &SqlitePool, task_id: &str) -> Result<bool> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE task_id = ? AND status = 'running'")
            .bind(task_id)
            .fetch_one(pool)
            .await?;
    Ok(count > 0)
}

async fn set_task_waiting(state: &AppState, task_id: &str, reason: &str) -> Result<()> {
    sqlx::query(
        "UPDATE tasks SET waiting_reason = ?, updated_at = ? \
         WHERE id = ? AND (waiting_reason IS NULL OR waiting_reason != ?)",
    )
    .bind(reason)
    .bind(now())
    .bind(task_id)
    .bind(reason)
    .execute(&state.db)
    .await?;
    Ok(())
}

async fn latest_message_role(pool: &SqlitePool, project_id: &str) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT role FROM messages WHERE project_id = ? ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await?)
}

async fn has_project_job(pool: &SqlitePool, project_id: &str, role: &str) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE project_id = ? AND task_id IS NULL AND role = ? \
         AND status = 'running'",
    )
    .bind(project_id)
    .bind(role)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

async fn active_project_jobs(pool: &SqlitePool, project_id: &str) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE project_id = ? AND status = 'running'")
            .bind(project_id)
            .fetch_one(pool)
            .await?,
    )
}

async fn note_waiting(
    state: &AppState,
    project: &ProjectRow,
    reason: &str,
    recoverable: bool,
) -> Result<()> {
    if project.stage != STAGE_EXECUTING || project.completion_mode == COMPLETION_DIRECT {
        crate::queue::waiting(&state.db, project, None, ROLE_PLANNER, reason).await?;
    }
    if project.blocked_reason.as_deref() == Some(reason) {
        return Ok(());
    }
    sqlx::query(
        "UPDATE projects SET blocked_reason = ?, status = ?, blocked_kind = ?, updated_at = ? \
         WHERE id = ? AND status = ? AND (blocked_reason IS NULL OR blocked_reason != ?)",
    )
    .bind(reason)
    .bind(if recoverable {
        STATUS_RUNNING
    } else {
        STATUS_BLOCKED
    })
    .bind((!recoverable && !reason.starts_with("项目费用预算")).then_some("configuration"))
    .bind(now())
    .bind(&project.id)
    .bind(STATUS_RUNNING)
    .bind(reason)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "agent_unavailable",
        reason,
    )
    .await;
    Ok(())
}

async fn clear_waiting(pool: &SqlitePool, project_id: &str) -> Result<()> {
    sqlx::query(
        "UPDATE projects SET blocked_reason = NULL, updated_at = ? \
         WHERE id = ? AND status = ? AND blocked_reason IS NOT NULL",
    )
    .bind(now())
    .bind(project_id)
    .bind(STATUS_RUNNING)
    .execute(pool)
    .await?;
    Ok(())
}

fn parse_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn role_label(role: &str) -> &'static str {
    match role {
        JOB_ROLE_ANALYSIS => "项目分析",
        JOB_ROLE_DIRECT => "直接完成",
        ROLE_PLANNER => "规划",
        ROLE_EXECUTOR => "执行",
        ROLE_VERIFIER => "验收",
        JOB_ROLE_ACCEPTANCE => "项目验收",
        JOB_ROLE_REPLAN_REVIEW => "重规划审批",
        JOB_ROLE_SUMMARY => "总结",
        _ => "未知",
    }
}
