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
use crate::events::emit;
use crate::models::*;
use crate::runner;
use crate::state::AppState;

pub async fn run(state: AppState, mut rx: mpsc::Receiver<()>) {
    if let Err(error) = recover(&state).await {
        tracing::error!(error = %error, "启动恢复失败");
    }
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            message = rx.recv() => {
                if message.is_none() {
                    break;
                }
            }
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
        sqlx::query("UPDATE jobs SET status = ?, error = ?, finished_at = ? WHERE id = ?")
            .bind(JOB_STATUS_FAILED)
            .bind("服务重启导致作业中断")
            .bind(now())
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
        } else if job.role == JOB_ROLE_GOAL {
            sqlx::query(
                "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
                 THEN status ELSE ? END, blocked_reason = NULL, updated_at = ? \
                 WHERE id = ? AND stage = ?",
            )
            .bind(STAGE_PLANNING)
            .bind(STATUS_RUNNING)
            .bind(now())
            .bind(&job.project_id)
            .bind(STAGE_PLANNING)
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
        emit(
            &state.db,
            &state.bus,
            Some(&job.project_id),
            job.task_id.as_deref(),
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
            let blocked: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage != ? AND status = ?",
            )
            .bind(&project.id)
            .bind(TASK_STAGE_COMPLETED)
            .bind(STATUS_BLOCKED)
            .fetch_one(&state.db)
            .await?;
            let incomplete: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage != ?",
            )
            .bind(&project.id)
            .bind(TASK_STAGE_COMPLETED)
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
        kill_job_dir(&PathBuf::from(&project.workspace).join("jobs").join(&job.id));
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
                     WHERE project_id = ? AND stage != ? AND status IN ('queued', 'running')",
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
            STAGE_PLANNING => {
                if has_project_job(&state.db, &project.id, JOB_ROLE_GOAL).await? {
                    continue;
                }
                let Some(agent) = choose_agent(state, ROLE_PLANNER, None).await? else {
                    note_waiting(state, &project, "等待可用的规划 Agent").await?;
                    continue;
                };
                if let Some(job_id) =
                    spawn_project_job(state, &project, &agent, JOB_ROLE_GOAL).await?
                {
                    spawns.push(job_id);
                }
            }
            STAGE_EXECUTING => {
                let tasks = sqlx::query_as::<_, TaskRow>(
                    "SELECT * FROM tasks WHERE project_id = ? ORDER BY seq ASC",
                )
                .bind(&project.id)
                .fetch_all(&state.db)
                .await?;
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
                    if let Some(agent) = choose_agent(state, ROLE_PLANNER, None).await? {
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
                    let Some(agent) = choose_agent(state, role, Some(task)).await? else {
                        if task.status.as_deref() != Some(STATUS_QUEUED) {
                            sqlx::query("UPDATE tasks SET status = ?, updated_at = ? WHERE id = ?")
                                .bind(STATUS_QUEUED)
                                .bind(now())
                                .bind(&task.id)
                                .execute(&state.db)
                                .await?;
                        }
                        note_waiting(
                            state,
                            &project,
                            &format!("等待可用的{} Agent", role_label(role)),
                        )
                        .await?;
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
                let Some(agent) = choose_agent(state, ROLE_PLANNER, None).await? else {
                    note_waiting(state, &project, "等待可用的总结 Agent").await?;
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
            .await?;
    let tasks: Vec<&TaskRow> = all_tasks
        .iter()
        .filter(|task| task.stage != TASK_STAGE_COMPLETED)
        .collect();
    if tasks.is_empty() {
        return Ok(());
    }
    let completed: HashSet<String> = all_tasks
        .iter()
        .filter(|task| task.stage == TASK_STAGE_COMPLETED)
        .map(|task| task.key.clone())
        .collect();
    let runnable = tasks.iter().any(|task| {
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
    let reason = tasks
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
    role: &str,
    task: Option<&TaskRow>,
) -> Result<Option<AgentRow>> {
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents WHERE enabled = 1")
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
    let mut candidates: Vec<AgentRow> = agents
        .into_iter()
        .filter(|agent| {
            let roles = parse_list(&agent.roles);
            roles.iter().any(|item| item == role)
                && agent
                    .cooldown_until
                    .as_deref()
                    .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                    .map(|until| until.with_timezone(&Utc) <= now)
                    .unwrap_or(true)
        })
        .collect();
    candidates.sort_by_key(|agent| (agent.priority, agent.id.clone()));
    if candidates.is_empty() {
        return Ok(None);
    }
    let last_agent = task.and_then(|task| task.last_agent_id.clone());
    let alternatives: Vec<AgentRow> = candidates
        .iter()
        .filter(|agent| Some(&agent.id) != last_agent.as_ref())
        .cloned()
        .collect();
    let pool = if alternatives.is_empty() {
        candidates
    } else {
        alternatives
    };
    for agent in pool {
        let running = counts.get(&agent.id).copied().unwrap_or(0);
        if running < agent.max_concurrency.max(1) {
            return Ok(Some(agent));
        }
    }
    Ok(None)
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
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, agent_id = ?, last_error = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(if role == ROLE_VERIFIER {
        TASK_STAGE_VERIFYING
    } else {
        TASK_STAGE_EXECUTING
    })
    .bind(STATUS_RUNNING)
    .bind(&agent.id)
    .bind(&timestamp)
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    clear_waiting(&state.db, &project.id).await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        "job_queued",
        format!(
            "{} 作业已派发给 {}（任务 {}）",
            role_label(role),
            agent.name,
            task.key
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
    clear_waiting(&state.db, &project.id).await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
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
         WHERE id = ? AND status = ? AND blocked_reason IS NULL",
    )
    .bind(reason)
    .bind(now())
    .bind(&project.id)
    .bind(STATUS_RUNNING)
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
        JOB_ROLE_GOAL => "规划",
        ROLE_PLANNER => "规划",
        ROLE_EXECUTOR => "执行",
        ROLE_VERIFIER => "验收",
        JOB_ROLE_SUMMARY => "总结",
        _ => "未知",
    }
}
