use std::path::PathBuf;

use anyhow::Result;

use crate::domain::now;
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::paths;
use crate::state::AppState;

use super::failure::plan_project_failure;

pub(super) async fn handle_direct_success(
    state: &AppState,
    project: &ProjectRow,
    summary: &str,
    report: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    let timestamp = now();
    sqlx::query(
        "UPDATE projects SET report = ?, report_feedback = NULL, stage = ?, \
         status = CASE WHEN status IN ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
         completion_mode = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(report)
    .bind(STAGE_SUMMARIZING)
    .bind(STATUS_BLOCKED)
    .bind(COMPLETION_DIRECT)
    .bind(&timestamp)
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "direct_completed",
        format!(
            "直接完成作业已完成{}，等待人工验收报告",
            if summary.trim().is_empty() {
                String::new()
            } else {
                format!("：{summary}")
            }
        ),
    )
    .await;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "report_ready",
        "项目报告已生成，等待人工验收",
    )
    .await;
    Ok(())
}

pub(super) async fn handle_direct_failure(
    state: &AppState,
    project: &ProjectRow,
    reason: &str,
    agent: Option<&AgentRow>,
    process_failure: bool,
) -> Result<()> {
    {
        let _guard = state.git_lock.lock().await;
        let _ = git::reset_hard(&paths::project_repo(&PathBuf::from(&project.workspace))).await;
    }
    let failure = plan_project_failure(
        state,
        project,
        reason,
        agent.map(|agent| (agent, process_failure)),
    )
    .await?;
    let _guard = state.schedule_lock.lock().await;
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = ?, completion_mode = ?, proposal = NULL, \
         confirmed_at = NULL, \
         avoid_agent_id = CASE WHEN ? THEN ? ELSE avoid_agent_id END, \
         avoid_agent_role = CASE WHEN ? THEN ? ELSE avoid_agent_role END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_ANALYSIS)
    .bind(failure.status())
    .bind(&failure.blocked_reason)
    .bind(COMPLETION_TASKS)
    .bind(failure.switch)
    .bind(failure.switch.then_some(ROLE_PLANNER))
    .bind(failure.switch)
    .bind(failure.switch.then_some(ROLE_PLANNER))
    .bind(now())
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        failure.event_kind("direct_failed"),
        format!("直接完成{}：{reason}", failure.label()),
    )
    .await;
    Ok(())
}
