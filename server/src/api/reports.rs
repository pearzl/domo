//! 项目报告提交、人工评议与验收处置。

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use crate::domain::now;
use crate::events::emit;
use crate::models::*;
use crate::replan;
use crate::state::AppState;

use super::helpers::require_project;
use super::ApiError;

pub(super) async fn submit_report(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ReportRequest>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    let feedback = request
        .feedback
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let event_kind: Option<&'static str>;
    let event_message: String;
    let mut replan_reason: Option<String> = None;
    {
        let _guard = state.schedule_lock.lock().await;
        let current = require_project(&state, &id).await?;
        if current.replan_generation != project.replan_generation
            || current.stage != project.stage
            || current.report != project.report
        {
            return Err(ApiError::conflict("报告所属计划已变化，请刷新后重试"));
        }
        let project = current;
        if project.status.as_deref() == Some(STATUS_ARCHIVED) {
            return Err(ApiError::bad("项目已归档"));
        }
        match request.action.as_str() {
            // 多轮评议：先记录意见，由规划者在下一轮重写时参考。
            "comment" => {
                if project.stage != STAGE_SUMMARIZING {
                    return Err(ApiError::bad("项目不在总结阶段，无法对报告发表意见"));
                }
                require_opinion(&feedback)?;
                record_report_opinion(&state, &id, &feedback).await?;
                event_kind = None;
                event_message = String::new();
            }
            "accept" => {
                if project.stage != STAGE_SUMMARIZING || project.report.is_none() {
                    return Err(ApiError::bad("当前没有等待验收的项目报告"));
                }
                if !feedback.is_empty() {
                    let round = record_report_opinion(&state, &id, &feedback).await?;
                    event_message = format!("项目报告已通过（附第 {round} 条意见），项目完成");
                } else {
                    event_message = "项目报告已通过，项目完成".into();
                }
                sqlx::query(
                    "UPDATE projects SET stage = ?, status = NULL, blocked_reason = NULL, \
                     report_feedback = NULL, updated_at = ? WHERE id = ?",
                )
                .bind(STAGE_COMPLETED)
                .bind(now())
                .bind(&id)
                .execute(&state.db)
                .await?;
                event_kind = Some("project_completed");
            }
            // 处置一：要求重新撰写（可多轮重复，意见持久化 + 触发重写作业）。
            "reject" => {
                if project.stage != STAGE_SUMMARIZING || project.report.is_none() {
                    return Err(ApiError::bad("当前没有等待验收的项目报告"));
                }
                require_opinion(&feedback)?;
                let round = record_report_opinion(&state, &id, &feedback).await?;
                if project.completion_mode == COMPLETION_DIRECT {
                    sqlx::query(
                        "UPDATE projects SET stage = ?, status = ?, report = NULL, \
                         report_feedback = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
                    )
                    .bind(STAGE_EXECUTING)
                    .bind(STATUS_RUNNING)
                    .bind(&feedback)
                    .bind(now())
                    .bind(&id)
                    .execute(&state.db)
                    .await?;
                    event_kind = Some("report_rejected");
                    event_message = format!(
                        "项目报告被要求重做（第 {round} 条意见），重新直接完成：{feedback}"
                    );
                } else {
                    sqlx::query(
                        "UPDATE projects SET stage = ?, status = ?, report = NULL, \
                         report_feedback = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
                    )
                    .bind(STAGE_SUMMARIZING)
                    .bind(STATUS_RUNNING)
                    .bind(&feedback)
                    .bind(now())
                    .bind(&id)
                    .execute(&state.db)
                    .await?;
                    event_kind = Some("report_rejected");
                    event_message = format!("项目报告被要求重写（第 {round} 条意见）：{feedback}");
                }
            }
            // 处置二：要求重新规划，项目回到项目分析阶段（复用重规划路径）。
            "replan" => {
                if project.stage != STAGE_SUMMARIZING || project.report.is_none() {
                    return Err(ApiError::bad("当前没有等待验收的项目报告"));
                }
                require_opinion(&feedback)?;
                let round = record_report_opinion(&state, &id, &feedback).await?;
                event_kind = None;
                event_message = format!(
                    "项目报告被要求重新规划（第 {round} 条意见），项目回到项目分析阶段：{feedback}"
                );
                replan_reason = Some(feedback.clone());
            }
            _ => {
                return Err(ApiError::bad(
                    "action 必须是 accept、reject、replan 或 comment",
                ))
            }
        }
    }
    // replan::request 自己加调度锁并记录重规划请求，方案确认时才作废旧计划，
    // 因此放在锁外调用。
    if let Some(reason) = replan_reason {
        if replan::request_report(&state, &project, &reason)
            .await?
            .is_none()
        {
            return Err(ApiError::conflict("报告所属计划已变化，请刷新后重试"));
        }
        state.notify_scheduler();
        return Ok(Json(json!({"ok": true, "action": "replan"})));
    }
    if let Some(event_kind) = event_kind {
        emit(
            &state.db,
            &state.bus,
            Some(&id),
            None,
            event_kind,
            event_message,
        )
        .await;
    }
    state.notify_scheduler();
    Ok(Json(json!({"ok": true, "action": request.action})))
}

/// 意见不能为空：评议记录必须可回溯，空意见不会产生任何处置。
pub(super) fn require_opinion(feedback: &str) -> Result<(), ApiError> {
    if feedback.trim().is_empty() {
        return Err(ApiError::bad("必须填写意见"));
    }
    Ok(())
}

/// 把一条人工意见写入 messages（角色 review），并返回它在评议记录中的序号。
pub(super) async fn record_report_opinion(
    state: &AppState,
    project_id: &str,
    feedback: &str,
) -> Result<i64, ApiError> {
    let existing: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE project_id = ? AND role = ?")
            .bind(project_id)
            .bind(REPORT_MESSAGE_ROLE)
            .fetch_one(&state.db)
            .await?;
    crate::runner::insert_message(&state.db, project_id, REPORT_MESSAGE_ROLE, feedback)
        .await
        .map_err(ApiError::internal)?;
    emit(
        &state.db,
        &state.bus,
        Some(project_id),
        None,
        "report_opinion",
        format!(
            "人工提交报告意见（第 {} 条）：{}",
            existing + 1,
            preview_opinion(feedback)
        ),
    )
    .await;
    Ok(existing + 1)
}

pub(super) fn preview_opinion(feedback: &str) -> String {
    let first = feedback.lines().next().unwrap_or(feedback);
    let mut text: String = first.chars().take(80).collect();
    if first.chars().count() > 80 {
        text.push('…');
    }
    text
}
