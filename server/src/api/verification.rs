//! 任务验收历史、人工裁决与任务重试。
use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};

use crate::domain::{self, now};
use crate::events::emit;
use crate::models::*;
use crate::state::AppState;

use super::detail::load_detail;
use super::helpers::{load_project_task, require_project, role_label};
use super::ApiError;

/// 任务验收历史固定保留最新这么多条，响应中的 `truncated` 说明是否还有更早的记录。
pub(super) const VERIFICATION_HISTORY_LIMIT: i64 = 50;
/// 验收结果无法解析时回传的原文摘要长度（按字符计，避免截断多字节字符）。
const VERIFICATION_EXCERPT_CHARS: usize = 500;

#[derive(Debug, Serialize)]
pub(super) struct VerificationSummary {
    pub(super) execution_job_id: Option<String>,
    pub(super) evidence: Option<Value>,
    pub(super) id: String,
    /// 所属执行轮，从 1 开始，0 表示没有可关联的执行作业。
    pub(super) round: i64,
    pub(super) attempt: i64,
    pub(super) status: String,
    pub(super) passed: Option<bool>,
    pub(super) reason: Option<String>,
    pub(super) issues: Vec<String>,
    pub(super) parsed: bool,
    /// 无法解析时为“无法解析：原因”，供界面直接展示。
    pub(super) parse_error: Option<String>,
    /// 无法解析时保留的结果原文摘要。
    pub(super) raw_excerpt: Option<String>,
    pub(super) agent_id: Option<String>,
    pub(super) agent_name: Option<String>,
    pub(super) started_at: Option<String>,
    pub(super) finished_at: Option<String>,
    pub(super) usage_known: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct TaskVerificationsResponse {
    pub(super) project_id: String,
    pub(super) task_id: String,
    pub(super) total: i64,
    pub(super) limit: i64,
    pub(super) truncated: bool,
    pub(super) verifications: Vec<VerificationSummary>,
}

pub(super) struct VerificationVerdict {
    passed: Option<bool>,
    reason: Option<String>,
    issues: Vec<String>,
    parsed: bool,
    parse_error: Option<String>,
    raw_excerpt: Option<String>,
}

pub(super) async fn task_verifications(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> Result<Json<TaskVerificationsResponse>, ApiError> {
    let task = load_project_task(&state, &project_id, &task_id).await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE task_id = ? AND role = ?")
        .bind(&task.id)
        .bind(JOB_ROLE_VERIFIER)
        .fetch_one(&state.db)
        .await?;
    let offset = (total - VERIFICATION_HISTORY_LIMIT).max(0);
    let jobs = sqlx::query_as::<_, JobRow>(
        "SELECT * FROM jobs WHERE task_id = ? AND role = ? \
         ORDER BY created_at ASC, id ASC LIMIT ? OFFSET ?",
    )
    .bind(&task.id)
    .bind(JOB_ROLE_VERIFIER)
    .bind(VERIFICATION_HISTORY_LIMIT)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let executions: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM jobs WHERE task_id = ? AND role = ? ORDER BY created_at ASC, id ASC",
    )
    .bind(&task.id)
    .bind(JOB_ROLE_EXECUTOR)
    .fetch_all(&state.db)
    .await?;
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
        .fetch_all(&state.db)
        .await?;
    let agent_names: HashMap<String, String> = agents
        .into_iter()
        .map(|agent| (agent.id, agent.name))
        .collect();
    let verifications = jobs
        .into_iter()
        .map(|job| {
            let verdict = verification_verdict(job.result.as_deref());
            VerificationSummary {
                evidence: job
                    .result
                    .as_deref()
                    .and_then(domain::extract_json)
                    .and_then(|value| value.get("evidence").cloned()),
                round: job
                    .execution_job_id
                    .as_ref()
                    .and_then(|id| executions.iter().position(|execution| execution == id))
                    .map_or(0, |index| index as i64 + 1),
                execution_job_id: job.execution_job_id,
                id: job.id,
                attempt: job.attempt,
                status: job.status,
                passed: verdict.passed,
                reason: verdict.reason,
                issues: verdict.issues,
                parsed: verdict.parsed,
                parse_error: verdict.parse_error,
                raw_excerpt: verdict.raw_excerpt,
                agent_name: job
                    .agent_id
                    .as_ref()
                    .and_then(|agent_id| agent_names.get(agent_id).cloned()),
                agent_id: job.agent_id,
                started_at: job.started_at,
                finished_at: job.finished_at,
                usage_known: job.usage_known != 0,
            }
        })
        .collect();
    Ok(Json(TaskVerificationsResponse {
        project_id,
        task_id: task.id,
        total,
        limit: VERIFICATION_HISTORY_LIMIT,
        truncated: total > VERIFICATION_HISTORY_LIMIT,
        verifications,
    }))
}

pub(super) fn verification_verdict(result: Option<&str>) -> VerificationVerdict {
    let Some(raw) = result else {
        return unparsed_verdict(None, "验收作业没有结果");
    };
    match domain::parse_result(ROLE_VERIFIER, raw) {
        Ok(domain::ParsedResult::Verdict {
            pass,
            reason,
            issues,
        }) => VerificationVerdict {
            passed: Some(pass),
            reason: Some(reason),
            issues,
            parsed: true,
            parse_error: None,
            raw_excerpt: None,
        },
        Ok(_) => unparsed_verdict(Some(raw), "结果不是验收结论"),
        Err(error) => unparsed_verdict(Some(raw), &error),
    }
}

pub(super) fn unparsed_verdict(raw: Option<&str>, error: &str) -> VerificationVerdict {
    VerificationVerdict {
        passed: None,
        reason: None,
        issues: Vec::new(),
        parsed: false,
        parse_error: Some(format!("无法解析：{error}")),
        raw_excerpt: raw.and_then(|value| excerpt(value, VERIFICATION_EXCERPT_CHARS)),
    }
}

pub(super) fn excerpt(value: &str, max: usize) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut excerpt: String = trimmed.chars().take(max).collect();
    if trimmed.chars().count() > max {
        excerpt.push('…');
    }
    Some(excerpt)
}

pub(super) async fn task_arbitration(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    let role = arbitration_role(&task);
    let replan_request = crate::replan_review::active(&state.db, &task.id).await?;
    let agents = role_agents(&state, role).await?;
    let jobs =
        sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE task_id = ? ORDER BY created_at ASC")
            .bind(&task.id)
            .fetch_all(&state.db)
            .await?;
    let agent_names: HashMap<String, String> = agents
        .iter()
        .map(|agent| (agent.id.clone(), agent.name.clone()))
        .collect();
    Ok(Json(json!({
        "project": {
            "id": project.id,
            "name": project.name,
            "stage": project.stage,
            "status": project.status,
            "blocked_reason": project.blocked_reason,
        },
        "task": {
            "id": task.id,
            "key": task.key,
            "title": task.title,
            "stage": task.stage,
            "status": task.status,
            "last_error": task.last_error,
            "failure_summary": crate::failure::summarize_classified(task.last_error.as_deref(), task.last_failure_kind.as_deref()),
            "last_failure_kind": task.last_failure_kind,
            "waiting_reason": task.waiting_reason,
            "attempts": task.attempts,
            "rejections": task.rejections,
            "milestone": task.milestone != 0,
            "human_gate": task.human_gate,
            "forced_agent_id": task.forced_agent_id,
            "forced_agent_role": task.forced_agent_role,
        },
        "role": role,
        "agents": agents.iter().map(|agent| json!({
            "id": agent.id,
            "name": agent.name,
            "kind": agent.kind,
            "enabled": agent.enabled != 0,
            "cooldown_until": agent.cooldown_until,
            "last_error": agent.last_error,
            "failure_kind": agent.failure_kind,
            "failure_summary": crate::failure::summarize(agent.last_error.as_deref()),
            "transient_failures": agent.transient_failures,
        })).collect::<Vec<_>>(),
        "jobs": jobs.iter().map(|job| json!({
            "id": job.id,
            "role": job.role,
            "agent_id": job.agent_id,
            "agent_name": job.agent_id.as_ref().and_then(|id| agent_names.get(id).cloned()),
            "status": job.status,
            "attempt": job.attempt,
            "error": job.error,
            "failure_summary": crate::failure::summarize_classified(job.error.as_deref(), job.failure_kind.as_deref()),
            "exit_code": job.exit_code,
            "failure_kind": job.failure_kind,
            "input_tokens": job.input_tokens,
            "output_tokens": job.output_tokens,
            "cache_read_tokens": job.cache_read_tokens,
            "cache_write_tokens": job.cache_write_tokens,
            "reasoning_tokens": job.reasoning_tokens,
            "cost": job.cost,
            "duration_ms": job.duration_ms,
            "usage_known": job.usage_known != 0,
            "started_at": job.started_at,
            "finished_at": job.finished_at,
            "created_at": job.created_at,
        })).collect::<Vec<_>>(),
        "replan_request": replan_request,
        "actions": if let Some(request) = &replan_request {
            if request.state == "waiting" { vec![ARBITRATION_RETRY, ARBITRATION_GUIDE] } else { vec![] }
        } else { vec![ARBITRATION_RETRY, ARBITRATION_SWITCH_AGENT, ARBITRATION_GUIDE] },
    })))
}

pub(super) async fn submit_arbitration(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<ArbitrationRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let action = request.action.trim().to_string();
    if !matches!(
        action.as_str(),
        ARBITRATION_RETRY | ARBITRATION_SWITCH_AGENT | ARBITRATION_GUIDE
    ) {
        return Err(ApiError::bad("action 必须是 retry、switch_agent 或 guide"));
    }
    let guidance = request
        .guidance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if action == ARBITRATION_GUIDE && guidance.is_none() {
        return Err(ApiError::bad("指导内容不能为空"));
    }
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    if task.stage == TASK_STAGE_COMPLETED {
        return Err(ApiError::bad("任务已完成"));
    }
    if task.status.as_deref() != Some(STATUS_BLOCKED) {
        return Err(ApiError::bad("只有阻塞中的任务可以裁决"));
    }
    if task.human_gate.as_deref() == Some(HUMAN_GATE_MILESTONE) {
        return Err(ApiError::bad("任务正在等待人工验收，请使用人工验收操作"));
    }
    if crate::replan_review::active(&state.db, &task.id)
        .await?
        .is_some()
    {
        if action == ARBITRATION_SWITCH_AGENT {
            return Err(ApiError::bad(
                "重规划审批使用父任务执行者池或项目规划者池，请调整对应角色池后重试审批",
            ));
        }
        crate::replan_review::retry_locked(&state, &task, guidance.as_deref())
            .await
            .map_err(|error| ApiError::bad(error.to_string()))?;
        state.notify_scheduler();
        return load_detail(&state, &project_id).await.map(Json);
    }
    let role = arbitration_role(&task);
    let mut forced_agent_id: Option<String> = None;
    let mut forced_agent_role: Option<String> = None;
    let mut event_kind = "arbitration_retry";
    let mut event_message = format!("人工裁决：重试任务 {}", task.key);
    match action.as_str() {
        ARBITRATION_SWITCH_AGENT => {
            if !request.risk_confirmed {
                return Err(ApiError::bad("人工指定会绕过角色、启停、冷却、可用时段和用量限额，可能产生额外费用；请明确确认风险"));
            }
            let agent_id = request
                .agent_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ApiError::bad("更换 Agent 时必须指定 agent_id"))?;
            let Some(agent) = crate::db::load_agent(&state.db, agent_id).await? else {
                return Err(ApiError::not_found("Agent 不存在"));
            };
            forced_agent_id = Some(agent.id.clone());
            forced_agent_role = Some(role.to_string());
            event_kind = "arbitration_switch_agent";
            event_message = format!(
                "人工裁决：任务 {} 下一次{}作业强制使用 {}",
                task.key,
                role_label(role),
                agent.name
            );
        }
        ARBITRATION_GUIDE => {
            let text = guidance.clone().unwrap_or_default();
            event_kind = "arbitration_guide";
            event_message = format!("人工裁决：任务 {} 收到指导并重新排队：{text}", task.key);
        }
        _ => {}
    }
    let reset_counts = matches!(action.as_str(), ARBITRATION_RETRY | ARBITRATION_GUIDE);
    let latest_job = if reset_counts {
        latest_failed_job(&state, &project_id, Some(&task.id), role).await?
    } else {
        None
    };
    let retry_agent = latest_job.as_ref().and_then(|job| job.agent_id.clone());
    let next_guidance = guidance.unwrap_or_else(|| task.guidance.clone());
    let paused = matches!(
        project.status.as_deref(),
        Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
    );
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM retry_requests WHERE task_id = ?")
        .bind(&task.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE tasks SET status = ?, attempts = ?, rejections = ?, agent_id = NULL, \
         last_agent_id = CASE WHEN ? THEN NULL ELSE last_agent_id END, \
         avoid_agent_id = CASE WHEN ? THEN NULL ELSE avoid_agent_id END, \
         avoid_agent_role = CASE WHEN ? THEN NULL ELSE avoid_agent_role END, \
         last_error = NULL, \
         last_failure_kind = NULL, waiting_reason = NULL, human_gate = NULL, guidance = ?, \
         forced_agent_id = ?, forced_agent_role = ?, retry_agent_id = ?, retry_role = ?, \
         updated_at = ? WHERE id = ? AND status = ?",
    )
    .bind(STATUS_QUEUED)
    .bind(if reset_counts { 0 } else { task.attempts })
    .bind(if reset_counts { 0 } else { task.rejections })
    .bind(reset_counts)
    .bind(reset_counts)
    .bind(reset_counts)
    .bind(&next_guidance)
    .bind(&forced_agent_id)
    .bind(&forced_agent_role)
    .bind(retry_agent)
    .bind(reset_counts.then_some(role))
    .bind(now())
    .bind(&task.id)
    .bind(STATUS_BLOCKED)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE projects SET status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = CASE WHEN ? THEN ? ELSE NULL END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(STATUS_RUNNING)
    .bind(paused)
    .bind("人工裁决已记录，等待项目恢复后生效")
    .bind(now())
    .bind(&project_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        event_kind,
        event_message,
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

/// Locate the latest failed or explicitly stopped run for new-session context.
pub(super) async fn latest_failed_job(
    state: &AppState,
    project_id: &str,
    task_id: Option<&str>,
    role: &str,
) -> Result<Option<JobRow>, ApiError> {
    let job = match task_id {
        Some(task_id) => {
            sqlx::query_as::<_, JobRow>(
                "SELECT * FROM jobs WHERE task_id = ? AND role = ? \
                 ORDER BY created_at DESC, id DESC LIMIT 1",
            )
            .bind(task_id)
            .bind(role)
            .fetch_optional(&state.db)
            .await?
        }
        None => {
            sqlx::query_as::<_, JobRow>(
                "SELECT * FROM jobs WHERE project_id = ? AND task_id IS NULL AND role = ? \
                 ORDER BY created_at DESC, id DESC LIMIT 1",
            )
            .bind(project_id)
            .bind(role)
            .fetch_optional(&state.db)
            .await?
        }
    };
    Ok(job.filter(|job| {
        job.status == JOB_STATUS_FAILED
            || (job.status == JOB_STATUS_CANCELED
                && job.failure_kind.as_deref() == Some("user_stopped"))
    }))
}

pub(super) async fn role_agents(state: &AppState, _role: &str) -> Result<Vec<AgentRow>, ApiError> {
    let rows =
        sqlx::query_as::<_, AgentRow>("SELECT * FROM agents ORDER BY created_at ASC, id ASC")
            .fetch_all(&state.db)
            .await?;
    Ok(rows)
}

pub(super) fn arbitration_role(task: &TaskRow) -> &'static str {
    if task.stage == TASK_STAGE_VERIFYING {
        ROLE_VERIFIER
    } else {
        ROLE_EXECUTOR
    }
}

pub(super) async fn retry_task(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    // Legacy task retry is a new-session arbitration, never a session resume.
    let _ = submit_arbitration(
        State(state),
        Path((project_id, task_id)),
        Json(ArbitrationRequest {
            action: ARBITRATION_RETRY.into(),
            agent_id: None,
            guidance: None,
            risk_confirmed: false,
        }),
    )
    .await?;
    Ok(Json(json!({"ok": true})))
}
