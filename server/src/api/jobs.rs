//! 作业查询、重试与输出/活动读取。
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;

use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use serde::Deserialize;
use tokio_stream::wrappers::{BroadcastStream, WatchStream};
use tokio_stream::StreamExt;

use crate::activity;
use crate::domain::now;
use crate::events::emit;
use crate::models::*;
use crate::paths;
use crate::state::AppState;
use crate::usage;

use super::detail::{job_duration, load_detail};
use super::helpers::{parse_list, require_project, role_label};
use super::ApiError;

/// Marks a failed job for an in-place resume. The next `run_job` uses the same
/// job id and therefore the same agent session.
pub(super) async fn mark_job_resumed(
    conn: &mut sqlx::SqliteConnection,
    job_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE jobs SET status = ?, started_at = NULL, finished_at = NULL, duration_ms = NULL, \
         error = NULL, failure_kind = NULL, exit_code = NULL, pid = NULL, result = NULL, \
         stdout = NULL, stderr = NULL, usage_known = 0, input_tokens = NULL, \
         output_tokens = NULL, cache_read_tokens = NULL, cache_write_tokens = NULL, \
         reasoning_tokens = NULL, cost = NULL, resume = 1 \
         WHERE id = ? AND status = ?",
    )
    .bind(JOB_STATUS_RUNNING)
    .bind(job_id)
    .bind(JOB_STATUS_FAILED)
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub(super) fn spawn_saved_job(state: &AppState, job_id: String) {
    let state = state.clone();
    tokio::spawn(async move {
        crate::runner::execute_job(state, job_id).await;
    });
}

/// Resumes the latest failed task job in place: the task and project return to
/// running, the same agent keeps the job, and the session is reused.
pub(super) async fn resume_task_job(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    job: &JobRow,
    guidance: &str,
    role: &str,
) -> Result<(), ApiError> {
    if matches!(
        project.status.as_deref(),
        Some(STATUS_PAUSING) | Some(STATUS_PAUSED) | Some(STATUS_ARCHIVED)
    ) {
        return Err(ApiError::bad("项目已暂停或归档，不能直接重试"));
    }
    let agent_id = job
        .agent_id
        .clone()
        .ok_or_else(|| ApiError::bad("作业没有关联 Agent"))?;
    let Some(agent) = crate::db::load_agent(&state.db, &agent_id).await? else {
        return Err(ApiError::not_found("Agent 不存在"));
    };
    if agent.enabled == 0 {
        return Err(ApiError::bad("Agent 已停用，请更换 Agent 后重试"));
    }
    if !parse_list(&agent.roles).iter().any(|item| item == role) {
        return Err(ApiError::bad("Agent 已不具备该角色，请更换 Agent 后重试"));
    }
    let stage = if role == ROLE_VERIFIER {
        TASK_STAGE_VERIFYING
    } else {
        TASK_STAGE_EXECUTING
    };
    let mut tx = state.db.begin().await?;
    let updated = sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, agent_id = ?, last_agent_id = ?, \
         attempts = 0, rejections = 0, last_error = NULL, last_failure_kind = NULL, \
         waiting_reason = NULL, human_gate = NULL, avoid_agent_id = NULL, \
         avoid_agent_role = NULL, forced_agent_id = NULL, forced_agent_role = NULL, \
         retry_agent_id = NULL, retry_role = NULL, guidance = ?, updated_at = ? \
         WHERE id = ? AND status = ?",
    )
    .bind(stage)
    .bind(STATUS_RUNNING)
    .bind(&agent_id)
    .bind(&agent_id)
    .bind(guidance)
    .bind(now())
    .bind(&task.id)
    .bind(STATUS_BLOCKED)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::bad("任务状态已变化"));
    }
    if !mark_job_resumed(&mut tx, &job.id).await? {
        return Err(ApiError::bad("作业状态已变化"));
    }
    sqlx::query(
        "UPDATE projects SET status = ?, blocked_reason = NULL, avoid_agent_id = NULL, \
         avoid_agent_role = NULL, retry_agent_id = NULL, retry_role = NULL, updated_at = ? \
         WHERE id = ? AND status != ?",
    )
    .bind(STATUS_RUNNING)
    .bind(now())
    .bind(&project.id)
    .bind(STATUS_ARCHIVED)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    crate::db::clear_agent_backoff(&state.db, &agent_id).await?;
    spawn_saved_job(state, job.id.clone());
    Ok(())
}

pub(super) async fn retry_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let job = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("作业不存在"))?;
    if job.status != JOB_STATUS_FAILED {
        return Err(ApiError::bad("只有失败的作业可以重试"));
    }
    let project = require_project(&state, &job.project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    let latest: Option<String> = match job.task_id.as_deref() {
        Some(task_id) => {
            sqlx::query_scalar(
                "SELECT id FROM jobs WHERE task_id = ? AND role = ? \
                 ORDER BY created_at DESC, id DESC LIMIT 1",
            )
            .bind(task_id)
            .bind(&job.role)
            .fetch_optional(&state.db)
            .await?
        }
        None => {
            sqlx::query_scalar(
                "SELECT id FROM jobs WHERE project_id = ? AND task_id IS NULL AND role = ? \
                 ORDER BY created_at DESC, id DESC LIMIT 1",
            )
            .bind(&project.id)
            .bind(&job.role)
            .fetch_optional(&state.db)
            .await?
        }
    };
    if latest.as_deref() != Some(job.id.as_str()) {
        return Err(ApiError::bad("该作业已有后续尝试，请重试最新的失败作业"));
    }
    let running: i64 =
        match job.task_id.as_deref() {
            Some(task_id) => sqlx::query_scalar(
                "SELECT COUNT(*) FROM jobs WHERE task_id = ? AND role = ? AND status = 'running'",
            )
            .bind(task_id)
            .bind(&job.role)
            .fetch_one(&state.db)
            .await?,
            None => {
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM jobs WHERE project_id = ? AND task_id IS NULL \
                 AND role = ? AND status = 'running'",
                )
                .bind(&project.id)
                .bind(&job.role)
                .fetch_one(&state.db)
                .await?
            }
        };
    if running > 0 {
        return Err(ApiError::bad("该作业所属阶段仍在运行"));
    }
    let paused = matches!(
        project.status.as_deref(),
        Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
    );
    let task_key = match job.task_id.as_deref() {
        Some(task_id) => {
            let task =
                sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ? AND project_id = ?")
                    .bind(task_id)
                    .bind(&project.id)
                    .fetch_optional(&state.db)
                    .await?
                    .ok_or_else(|| ApiError::not_found("任务不存在"))?;
            if task.stage == TASK_STAGE_COMPLETED {
                return Err(ApiError::bad("任务已完成"));
            }
            if task.status.as_deref() != Some(STATUS_BLOCKED) {
                return Err(ApiError::bad("只有阻塞中的任务可以重试"));
            }
            if task.human_gate.as_deref() == Some(HUMAN_GATE_MILESTONE) {
                return Err(ApiError::bad("任务正在等待人工验收，请使用人工验收操作"));
            }
            let expected = if task.stage == TASK_STAGE_VERIFYING {
                ROLE_VERIFIER
            } else {
                ROLE_EXECUTOR
            };
            if job.role != expected {
                return Err(ApiError::bad("作业与任务当前阶段不一致"));
            }
            resume_task_job(&state, &project, &task, &job, &task.guidance, &job.role).await?;
            Some(task.key)
        }
        None => {
            let stage_matches = match job.role.as_str() {
                JOB_ROLE_ANALYSIS => project.stage == STAGE_ANALYSIS,
                JOB_ROLE_DIRECT => {
                    project.stage == STAGE_EXECUTING && project.completion_mode == COMPLETION_DIRECT
                }
                JOB_ROLE_ACCEPTANCE => {
                    project.stage == STAGE_SUMMARIZING
                        && project.report.is_none()
                        && project.acceptance_passed == 0
                }
                JOB_ROLE_SUMMARY => project.stage == STAGE_SUMMARIZING,
                _ => false,
            };
            if !stage_matches {
                return Err(ApiError::bad("作业与项目当前阶段不一致"));
            }
            if job.role == JOB_ROLE_SUMMARY && project.report.is_some() {
                return Err(ApiError::bad("报告正在等待人工验收"));
            }
            if project.status.as_deref() != Some(STATUS_BLOCKED) {
                return Err(ApiError::bad("项目没有阻塞"));
            }
            None
        }
    };
    if job.task_id.is_none() {
        sqlx::query(
            "UPDATE projects SET status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
             THEN status ELSE ? END, blocked_reason = CASE WHEN ? THEN ? ELSE NULL END, \
             avoid_agent_id = NULL, avoid_agent_role = NULL, retry_agent_id = ?, retry_role = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind(STATUS_RUNNING)
        .bind(paused)
        .bind("人工重试已记录，等待项目恢复后生效")
        .bind(&job.agent_id)
        .bind(ROLE_PLANNER)
        .bind(now())
        .bind(&project.id)
        .execute(&state.db)
        .await?;
        if job.role == JOB_ROLE_ACCEPTANCE {
            sqlx::query(
                "UPDATE projects SET acceptance = NULL, acceptance_passed = 0, \
                 acceptance_round = 0, acceptance_at = NULL, updated_at = ? WHERE id = ?",
            )
            .bind(now())
            .bind(&project.id)
            .execute(&state.db)
            .await?;
        }
    }
    let subject = task_key
        .map(|key| format!("任务 {key} "))
        .unwrap_or_default();
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        job.task_id.as_deref(),
        "job_retry",
        format!("人工重试{subject}{}作业", role_label(&job.role)),
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project.id).await.map(Json)
}

pub(super) async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<JobView>, ApiError> {
    let job = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("作业不存在"))?;
    let agent_name = match &job.agent_id {
        Some(agent_id) => crate::db::load_agent(&state.db, agent_id)
            .await?
            .map(|agent| agent.name),
        None => None,
    };
    let pid = match job.pid {
        Some(pid) => Some(pid),
        None => job_pid_file(&state, &job).await,
    };
    let failure_summary = crate::failure::summarize(job.error.as_deref());
    Ok(Json(JobView {
        id: job.id,
        project_id: job.project_id,
        task_id: job.task_id,
        role: job.role,
        agent_id: job.agent_id,
        agent_name,
        status: job.status,
        attempt: job.attempt,
        prompt: job.prompt,
        result: job.result,
        stdout: job.stdout,
        stderr: job.stderr,
        error: job.error,
        exit_code: job.exit_code,
        pid,
        failure_kind: job.failure_kind,
        failure_summary,
        input_tokens: job.input_tokens,
        output_tokens: job.output_tokens,
        cache_read_tokens: job.cache_read_tokens,
        cache_write_tokens: job.cache_write_tokens,
        reasoning_tokens: job.reasoning_tokens,
        cost: job.cost,
        duration_ms: job
            .duration_ms
            .or_else(|| job_duration(&job.started_at, &job.finished_at)),
        usage_known: job.usage_known != 0,
        started_at: job.started_at,
        finished_at: job.finished_at,
        created_at: job.created_at,
    }))
}

pub(super) async fn job_pid_file(state: &AppState, job: &JobRow) -> Option<i64> {
    let project = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
        .bind(&job.project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()?;
    let path = paths::job_dir(&PathBuf::from(project.workspace), &job.id).join("pid");
    let raw = tokio::fs::read_to_string(path).await.ok()?;
    raw.trim().split(':').next()?.parse::<i64>().ok()
}

#[derive(Debug, serde::Serialize)]
pub(super) struct JobOutput {
    stdout: String,
    stderr: String,
    pid: Option<i64>,
    running: bool,
}

pub(super) async fn get_job_activity(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<activity::JobActivity>, ApiError> {
    let job = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("作业不存在"))?;
    let project = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
        .bind(&job.project_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("项目不存在"))?;
    let dir = paths::job_dir(&PathBuf::from(project.workspace), &job.id);
    let raw = match tokio::fs::read(dir.join("stdout.log")).await {
        Ok(bytes) => bytes,
        Err(_) => job.stdout.clone().unwrap_or_default().into_bytes(),
    };
    Ok(Json(activity::report_from_bytes(&raw)))
}

pub(super) async fn get_job_output(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<JobOutput>, ApiError> {
    let job = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("作业不存在"))?;
    let project = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
        .bind(&job.project_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("项目不存在"))?;
    let dir = paths::job_dir(&PathBuf::from(project.workspace), &job.id);
    let stdout = match tokio::fs::read_to_string(dir.join("stdout.log")).await {
        Ok(text) => text,
        Err(_) => job.stdout.clone().unwrap_or_default(),
    };
    let stderr = match tokio::fs::read_to_string(dir.join("stderr.log")).await {
        Ok(text) => text,
        Err(_) => job.stderr.clone().unwrap_or_default(),
    };
    let pid = match job.pid {
        Some(pid) => Some(pid),
        None => job_pid_file(&state, &job).await,
    };
    Ok(Json(JobOutput {
        stdout,
        stderr,
        pid,
        running: job.status == JOB_STATUS_RUNNING,
    }))
}

#[derive(Debug, Deserialize)]
pub(super) struct UsageQuery {
    project_id: Option<String>,
    agent_id: Option<String>,
    role: Option<String>,
    #[serde(alias = "start")]
    from: Option<String>,
    #[serde(alias = "end")]
    to: Option<String>,
    group_by: Option<String>,
}

pub(super) async fn usage_report(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
) -> Result<Json<usage::UsageReport>, ApiError> {
    let group_by = usage::UsageGroupBy::parse(query.group_by.as_deref()).map_err(ApiError::bad)?;
    let from = parse_usage_time(query.from.as_deref())?;
    let to = parse_usage_time(query.to.as_deref())?;
    let filter = usage::UsageFilter {
        project_id: query.project_id.clone(),
        agent_id: query.agent_id.clone(),
        role: query.role.clone(),
        from,
        to,
    };
    let rows = usage::load_jobs(&state.db, query.project_id.as_deref()).await?;
    let agent_rows = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
        .fetch_all(&state.db)
        .await?;
    let names: HashMap<String, String> = agent_rows
        .into_iter()
        .map(|agent| (agent.id, agent.name))
        .collect();
    Ok(Json(usage::summarize(&rows, &filter, group_by, &names)))
}

pub(super) fn parse_usage_time(
    raw: Option<&str>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|value| Some(value.with_timezone(&chrono::Utc)))
        .map_err(|_| ApiError::bad(format!("时间格式必须为 RFC3339: {raw}")))
}

#[derive(Debug, Deserialize)]
pub(super) struct EventQuery {
    project_id: Option<String>,
    after: Option<i64>,
    limit: Option<i64>,
}

pub(super) async fn list_events(
    State(state): State<AppState>,
    Query(query): Query<EventQuery>,
) -> Result<Json<Vec<EventView>>, ApiError> {
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let rows = if let Some(project_id) = &query.project_id {
        sqlx::query_as::<_, EventRow>(
            "SELECT * FROM events WHERE project_id = ? AND id > ? ORDER BY id ASC LIMIT ?",
        )
        .bind(project_id)
        .bind(query.after.unwrap_or(0))
        .bind(limit)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, EventRow>("SELECT * FROM events WHERE id > ? ORDER BY id ASC LIMIT ?")
            .bind(query.after.unwrap_or(0))
            .bind(limit)
            .fetch_all(&state.db)
            .await?
    };
    Ok(Json(rows.into_iter().map(EventView::from_row).collect()))
}

pub(super) async fn get_event(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<EventView>, ApiError> {
    let event = sqlx::query_as::<_, EventRow>("SELECT * FROM events WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("事件不存在"))?;
    Ok(Json(EventView::full(event)))
}

pub(super) async fn stream_events(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.bus.subscribe();
    let events = BroadcastStream::new(receiver).filter_map(|message| match message {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
            Some(Ok(Event::default().data(data)))
        }
        Err(_) => None,
    });
    // The broadcast bus outlives every connection, so the response only ends
    // when shutdown is requested. Without this the graceful drain would wait
    // for an event stream that can never finish. `map_while` stops at the
    // `None` sentinel, so the stream never yields an absent item.
    let shutdown = WatchStream::new(state.shutdown_receiver())
        .filter(|stopping| *stopping)
        .map(|_| None::<Result<Event, Infallible>>);
    let stream = events.map(Some).merge(shutdown).map_while(|item| item);
    Sse::new(stream).keep_alive(KeepAlive::default())
}
