//! Single scheduler loop. It reconciles persisted state on every tick, so a
//! restart or a completed job is just another reason to run reconciliation.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
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
    if let Err(error) = recover(&state).await {
        tracing::error!(error = %error, "启动恢复失败");
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
            match reconcile(&state).await {
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
    let jobs = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE status = 'running'")
        .fetch_all(&state.db)
        .await?;
    for job in jobs {
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
            if human_gate > 0 {
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
                    "UPDATE projects SET status = ?, updated_at = ? WHERE id = ? AND stage = ?",
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
    let projects = sqlx::query_as::<_, ProjectRow>(
        "SELECT * FROM projects WHERE status IN ('running', 'pausing') ORDER BY created_at ASC",
    )
    .fetch_all(&state.db)
    .await?;
    let mut spawns = Vec::new();
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
                sqlx::query(
                    "UPDATE tasks SET status = ?, updated_at = ? \
                     WHERE project_id = ? AND stage != ? AND status IN ('queued', 'running', 'pausing')",
                )
                .bind(STATUS_PAUSED)
                .bind(now())
                .bind(&project.id)
                .bind(TASK_STAGE_COMPLETED)
                .execute(&state.db)
                .await?;
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
        match project.stage.as_str() {
            STAGE_ANALYSIS => {
                if has_project_job(&state.db, &project.id, JOB_ROLE_ANALYSIS).await? {
                    continue;
                }
                let replan_pending = crate::replan::pending(&state.db, &project).await?;
                let last_role = latest_message_role(&state.db, &project.id).await?;
                if !replan_pending && last_role.as_deref() != Some("user") {
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
                    )
                    .await?;
                    continue;
                };
                if let Some(job_id) =
                    spawn_project_job(state, &project, &agent, JOB_ROLE_ANALYSIS).await?
                {
                    spawns.push(job_id);
                }
            }
            STAGE_EXECUTING => {
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
                        )
                        .await?;
                        continue;
                    };
                    if let Some(job_id) =
                        spawn_project_job(state, &project, &agent, JOB_ROLE_DIRECT).await?
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
                         blocked_reason = NULL, updated_at = ? WHERE id = ?",
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
                        "全部任务完成，开始撰写项目报告",
                    )
                    .await;
                    if let Some(agent) = choose_agent(state, &project, ROLE_PLANNER, None)
                        .await?
                        .agent
                    {
                        if let Some(job_id) =
                            spawn_project_job(state, &project, &agent, JOB_ROLE_SUMMARY).await?
                        {
                            spawns.push(job_id);
                        }
                    }
                    continue;
                }
                for task in &tasks {
                    if task.status.as_deref() == Some(STATUS_BLOCKED)
                        || task.stage == TASK_STAGE_COMPLETED
                    {
                        continue;
                    }
                    if task.status.as_deref() != Some(STATUS_QUEUED) {
                        continue;
                    }
                    let deps = parse_list(&task.deps);
                    let ready =
                        task.stage != TASK_STAGE_PENDING || deps_complete(&deps, &completed);
                    if !ready {
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
                    let role = if task.stage == TASK_STAGE_VERIFYING {
                        ROLE_VERIFIER
                    } else {
                        ROLE_EXECUTOR
                    };
                    if has_task_job(&state.db, &task.id, role).await? {
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
                        note_waiting(state, &project, &reason).await?;
                        continue;
                    };
                    if let Some(job_id) =
                        spawn_task_job(state, &project, task, &agent, role).await?
                    {
                        spawns.push(job_id);
                    }
                }
                recompute_blocked(state, &project.id).await?;
            }
            STAGE_SUMMARIZING => {
                if has_project_job(&state.db, &project.id, JOB_ROLE_SUMMARY).await? {
                    continue;
                }
                let choice = choose_agent(state, &project, ROLE_PLANNER, None).await?;
                let Some(agent) = choice.agent else {
                    note_waiting(
                        state,
                        &project,
                        choice.reason.as_deref().unwrap_or("等待可用的总结 Agent"),
                    )
                    .await?;
                    continue;
                };
                if let Some(job_id) =
                    spawn_project_job(state, &project, &agent, JOB_ROLE_SUMMARY).await?
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

async fn choose_agent(
    state: &AppState,
    project: &ProjectRow,
    role: &str,
    task: Option<&TaskRow>,
) -> Result<AgentChoice> {
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
                        reason: Some(format!(
                            "人工指定的{} Agent 已被删除，请重新裁决或调整项目 Agent 池",
                            role_label(role)
                        )),
                    });
                };
                let running = running_jobs(state, &agent.id).await?;
                if running < agent.max_concurrency.max(1) {
                    return Ok(AgentChoice {
                        agent: Some(agent),
                        reason: None,
                    });
                }
                return Ok(AgentChoice {
                    agent: None,
                    reason: Some(format!(
                        "等待人工指定的{} Agent {} 释放（并发 {running}/{}）",
                        role_label(role),
                        agent.name,
                        agent.max_concurrency.max(1)
                    )),
                });
            }
        }
    }

    // 执行者与验收者从任务专用 Agent 池按严格优先级选人：池为空或全不可用
    // 时只等待，不回退到项目池；调度只读取任务池。规划者使用项目池。
    let (rows, scope) = if role == ROLE_EXECUTOR || role == ROLE_VERIFIER {
        let Some(task) = task else {
            return Ok(AgentChoice {
                agent: None,
                reason: Some(format!(
                    "{}角色只提供给任务使用，缺少任务上下文",
                    role_label(role)
                )),
            });
        };
        crate::db::ensure_task_agents(&state.db, &project.id, &task.id).await?;
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
    if rows.is_empty() {
        let hint = if scope == "任务 Agent 池" {
            "任务池在创建时从项目池快照，请检查项目池该角色配置；调整项目池后需新建任务"
        } else {
            "请在项目页配置"
        };
        return Ok(AgentChoice {
            agent: None,
            reason: Some(format!("{scope}没有{}角色 Agent，{hint}", role_label(role))),
        });
    }
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
        if avoided == Some(agent.id.as_str()) {
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
            let until = agent.cooldown_until.as_deref().unwrap_or_default();
            unavailable.push(format!("「{}」（冷却中，至 {until}）", agent.name));
            continue;
        }
        match agent_gate(state, agent).await? {
            Some(usage::AgentGate::Schedule(reason)) => {
                unavailable.push(format!("「{}」（{reason}）", agent.name));
                continue;
            }
            Some(usage::AgentGate::Limit(reason)) => {
                unavailable.push(format!("「{}」（{reason}）", agent.name));
                continue;
            }
            None => {}
        }
        // Strict priority: the first usable agent owns this dispatch. When it
        // is at capacity the task waits instead of falling through to a lower
        // priority agent.
        let running = counts.get(&agent.id).copied().unwrap_or(0);
        let limit = agent.max_concurrency.max(1);
        if running < limit {
            return Ok(AgentChoice {
                agent: Some(agent.clone()),
                reason: None,
            });
        }
        return Ok(AgentChoice {
            agent: None,
            reason: Some(format!(
                "等待 Agent {} 释放（并发 {running}/{limit}）",
                agent.name
            )),
        });
    }
    Ok(AgentChoice {
        agent: None,
        reason: Some(format!(
            "{scope}没有可用的{} Agent（不可用：{}）",
            role_label(role),
            unavailable.join("、")
        )),
    })
}

struct AgentChoice {
    agent: Option<AgentRow>,
    reason: Option<String>,
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
) -> Result<Option<String>> {
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Ok(None);
    }
    let job_id = domain::new_id("job");
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
    sqlx::query(
        "INSERT INTO jobs (id, project_id, task_id, role, agent_id, status, attempt, prompt, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, '', ?)",
    )
    .bind(&job_id)
    .bind(&project.id)
    .bind(&task.id)
    .bind(role)
    .bind(&agent.id)
    .bind(JOB_STATUS_RUNNING)
    .bind(attempt)
    .bind(&timestamp)
    .execute(&state.db)
    .await?;
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
    if retry && dispatched.rows_affected() > 0 {
        crate::db::clear_agent_backoff(&state.db, &agent.id).await?;
    }
    clear_waiting(&state.db, &project.id).await?;
    emit_job(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        Some(&job_id),
        "job_queued",
        format!(
            "{} 作业已派发给 {}（任务 {}）{}",
            role_label(role),
            agent.name,
            task.key,
            if forced { "，由人工指定" } else { "" }
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
) -> Result<Option<String>> {
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Ok(None);
    }
    let job_id = domain::new_id("job");
    let retry = project.retry_role.as_deref() == Some(role)
        && project.retry_agent_id.as_deref() == Some(agent.id.as_str());
    let timestamp = now();
    sqlx::query(
        "INSERT INTO jobs (id, project_id, task_id, role, agent_id, status, attempt, prompt, created_at) \
         VALUES (?, ?, NULL, ?, ?, ?, 1, '', ?)",
    )
    .bind(&job_id)
    .bind(&project.id)
    .bind(role)
    .bind(&agent.id)
    .bind(JOB_STATUS_RUNNING)
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
    .bind(role)
    .bind(role)
    .bind(&timestamp)
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    if retry && dispatched.rows_affected() > 0 {
        crate::db::clear_agent_backoff(&state.db, &agent.id).await?;
    }
    clear_waiting(&state.db, &project.id).await?;
    emit_job(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        Some(&job_id),
        "job_queued",
        format!("{} 作业已派发给 {}", role_label(role), agent.name),
    )
    .await;
    Ok(Some(job_id))
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

async fn note_waiting(state: &AppState, project: &ProjectRow, reason: &str) -> Result<()> {
    if project.blocked_reason.as_deref() == Some(reason) {
        return Ok(());
    }
    sqlx::query(
        "UPDATE projects SET blocked_reason = ?, updated_at = ? \
         WHERE id = ? AND status = ? AND (blocked_reason IS NULL OR blocked_reason != ?)",
    )
    .bind(reason)
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
        JOB_ROLE_SUMMARY => "总结",
        _ => "未知",
    }
}
