use anyhow::Result;
use serde_json::json;

use crate::domain::{self, now, AcceptanceCriterion};
use crate::events::emit;
use crate::models::*;
use crate::replan;
use crate::state::AppState;

use super::common::{apply_stale_result, has_active_tasks, load_project};
use super::failure::plan_project_failure;

pub(super) async fn handle_report(
    state: &AppState,
    project: &ProjectRow,
    report: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    let current = crate::replan::load_project(&state.db, &project.id)
        .await?
        .unwrap_or_else(|| project.clone());
    if current.replan_active != 0 {
        sqlx::query(
            "UPDATE projects SET report = ?, report_feedback = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(report)
        .bind(now())
        .bind(&project.id)
        .execute(&state.db)
        .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            None,
            "report_ready",
            "旧计划报告已生成，重规划状态保持不变",
        )
        .await;
        return Ok(());
    }
    if has_active_tasks(&state.db, &project.id).await? {
        return apply_stale_result(
            state,
            project,
            "report_superseded",
            "项目已有未完成任务，报告作废，回到执行阶段".into(),
        )
        .await;
    }
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, report = ?, report_feedback = NULL, blocked_reason = NULL, \
         updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_SUMMARIZING)
    .bind(STATUS_BLOCKED)
    .bind(report)
    .bind(now())
    .bind(&project.id)
    .execute(&state.db)
    .await?;
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

pub(super) async fn handle_project_acceptance(
    state: &AppState,
    project: &ProjectRow,
    summary: &str,
    criteria: &[AcceptanceCriterion],
    replan_reason: &str,
) -> Result<()> {
    let snapshot = serde_json::to_string(&json!({
        "kind": "project_acceptance",
        "summary": summary,
        "criteria": criteria.iter().map(|item| json!({
            "id": &item.id,
            "source": &item.source,
            "criterion": &item.criterion,
            "verdict": &item.verdict,
            "evidence": &item.evidence,
            "gap": &item.gap,
        })).collect::<Vec<_>>(),
        "replan_reason": replan_reason,
    }))
    .unwrap_or_default();
    let gaps: Vec<&AcceptanceCriterion> = criteria
        .iter()
        .filter(|item| item.blocks_report())
        .collect();
    let passed = gaps.is_empty();
    let timestamp = now();
    let replanning;
    {
        let _guard = state.schedule_lock.lock().await;
        let current = crate::replan::load_project(&state.db, &project.id)
            .await?
            .unwrap_or_else(|| project.clone());
        replanning = current.replan_active != 0;
        if !replanning && has_active_tasks(&state.db, &project.id).await? {
            return apply_stale_result(
                state,
                project,
                "acceptance_superseded",
                "项目已有未完成任务，验收结论作废，回到执行阶段".into(),
            )
            .await;
        }
        if passed && !replanning {
            sqlx::query(
                "UPDATE projects SET stage = ?, status = CASE WHEN status IN \
                 ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
                 acceptance = ?, acceptance_passed = 1, acceptance_round = 0, \
                 acceptance_at = ?, blocked_reason = NULL, report = NULL, \
                 report_feedback = NULL, updated_at = ? WHERE id = ?",
            )
            .bind(STAGE_SUMMARIZING)
            .bind(STATUS_RUNNING)
            .bind(&snapshot)
            .bind(&timestamp)
            .bind(&timestamp)
            .bind(&project.id)
            .execute(&state.db)
            .await?;
        } else if replanning {
            sqlx::query(
                "UPDATE projects SET acceptance = ?, acceptance_passed = ?, \
                 acceptance_round = CASE WHEN ? THEN 0 ELSE acceptance_round END, \
                 acceptance_at = ?, report = CASE WHEN ? THEN NULL ELSE report END, \
                 report_feedback = CASE WHEN ? THEN NULL ELSE report_feedback END, \
                 updated_at = ? WHERE id = ?",
            )
            .bind(&snapshot)
            .bind(if passed { 1 } else { 0 })
            .bind(passed)
            .bind(&timestamp)
            .bind(passed)
            .bind(passed)
            .bind(&timestamp)
            .bind(&project.id)
            .execute(&state.db)
            .await?;
        } else {
            sqlx::query(
                "UPDATE projects SET stage = ?, acceptance = ?, acceptance_passed = 0, \
                 acceptance_at = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
            )
            .bind(STAGE_SUMMARIZING)
            .bind(&snapshot)
            .bind(&timestamp)
            .bind(&timestamp)
            .bind(&project.id)
            .execute(&state.db)
            .await?;
        }
    }
    if passed {
        let suffix = if summary.trim().is_empty() {
            String::new()
        } else {
            format!("：{summary}")
        };
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            None,
            "project_accepted",
            format!("项目验收通过{suffix}"),
        )
        .await;
        return Ok(());
    }

    let gap_text = acceptance_gap_text(criteria);
    let reason = if replan_reason.trim().is_empty() {
        format!("项目验收存在未闭合条目：\n{gap_text}")
    } else {
        format!("{}\n{gap_text}", replan_reason.trim())
    };
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "project_acceptance_failed",
        format!("项目验收未通过：\n{gap_text}"),
    )
    .await;

    if replanning {
        return Ok(());
    }
    if project.acceptance_round >= domain::MAX_AUTO_ACCEPTANCE_REPLANS {
        let _guard = state.schedule_lock.lock().await;
        let blocked_reason = format!(
            "项目验收自动重规划次数已达上限（{} 次），最近一次仍未通过，等待人工裁决：\n{gap_text}",
            domain::MAX_AUTO_ACCEPTANCE_REPLANS
        );
        sqlx::query(
            "UPDATE projects SET status = CASE WHEN status IN \
             ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
             blocked_reason = ?, updated_at = ? WHERE id = ?",
        )
        .bind(STATUS_BLOCKED)
        .bind(&blocked_reason)
        .bind(now())
        .bind(&project.id)
        .execute(&state.db)
        .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            None,
            "project_acceptance_blocked",
            blocked_reason,
        )
        .await;
        return Ok(());
    }

    let outcome = replan::request(state, &project.id, &reason, REPLAN_SOURCE_ACCEPTANCE).await?;
    if outcome.is_some() {
        sqlx::query("UPDATE projects SET acceptance_round = ?, updated_at = ? WHERE id = ?")
            .bind(project.acceptance_round + 1)
            .bind(now())
            .bind(&project.id)
            .execute(&state.db)
            .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            None,
            "acceptance_replan",
            format!(
                "项目验收未通过，自动重新规划（{}/{}）：\n{gap_text}",
                project.acceptance_round + 1,
                domain::MAX_AUTO_ACCEPTANCE_REPLANS
            ),
        )
        .await;
    } else {
        let _guard = state.schedule_lock.lock().await;
        if let Some(current) = load_project(&state.db, &project.id).await? {
            if current.stage == STAGE_SUMMARIZING
                && current.acceptance_passed == 0
                && current.status.as_deref() != Some(STATUS_ARCHIVED)
            {
                sqlx::query(
                    "UPDATE projects SET status = ?, blocked_reason = ?, updated_at = ? WHERE id = ?",
                )
                .bind(STATUS_BLOCKED)
                .bind(&reason)
                .bind(now())
                .bind(&project.id)
                .execute(&state.db)
                .await?;
            }
        }
    }
    Ok(())
}

pub(super) fn acceptance_gap_text(criteria: &[AcceptanceCriterion]) -> String {
    criteria
        .iter()
        .filter(|item| item.blocks_report())
        .map(|item| {
            let verdict = if item.verdict == "unmet" {
                "未满足"
            } else {
                "无法验证"
            };
            let gap = if item.gap.trim().is_empty() {
                String::new()
            } else {
                format!("：{}", item.gap.trim())
            };
            let evidence = if item.evidence.is_empty() {
                String::new()
            } else {
                format!("（证据：{}）", item.evidence.join("；"))
            };
            format!(
                "- [{}] {}（{}）{}{}",
                item.id, item.criterion, verdict, gap, evidence
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) async fn handle_acceptance_failure(
    state: &AppState,
    project: &ProjectRow,
    reason: &str,
    agent: Option<&AgentRow>,
    process_failure: bool,
) -> Result<()> {
    handle_project_job_failure(
        state,
        project,
        reason,
        agent,
        process_failure,
        "acceptance_failed",
        "项目验收",
    )
    .await
}

pub(super) async fn handle_summary_failure(
    state: &AppState,
    project: &ProjectRow,
    reason: &str,
    agent: Option<&AgentRow>,
    process_failure: bool,
) -> Result<()> {
    handle_project_job_failure(
        state,
        project,
        reason,
        agent,
        process_failure,
        "summary_failed",
        "总结",
    )
    .await
}

pub(super) async fn handle_project_job_failure(
    state: &AppState,
    project: &ProjectRow,
    reason: &str,
    agent: Option<&AgentRow>,
    process_failure: bool,
    failed_kind: &'static str,
    label: &str,
) -> Result<()> {
    let failure = plan_project_failure(
        state,
        project,
        reason,
        agent.map(|agent| (agent, process_failure)),
    )
    .await?;
    let _guard = state.schedule_lock.lock().await;
    let current = crate::replan::load_project(&state.db, &project.id)
        .await?
        .unwrap_or_else(|| project.clone());
    if current.replan_active != 0 {
        sqlx::query(
            "UPDATE projects SET avoid_agent_id = CASE WHEN ? THEN ? ELSE avoid_agent_id END, \
             avoid_agent_role = CASE WHEN ? THEN ? ELSE avoid_agent_role END, updated_at = ? \
             WHERE id = ?",
        )
        .bind(failure.switch)
        .bind(agent.map(|agent| agent.id.as_str()))
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
            failure.event_kind(failed_kind),
            format!("旧计划{label}{}：{reason}", failure.label()),
        )
        .await;
        return Ok(());
    }
    if has_active_tasks(&state.db, &project.id).await? {
        return apply_stale_result(
            state,
            project,
            failed_kind,
            format!("{label}失败：{reason}（项目已有未完成任务，回到执行阶段）"),
        )
        .await;
    }
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = ?, \
         avoid_agent_id = CASE WHEN ? THEN ? ELSE avoid_agent_id END, \
         avoid_agent_role = CASE WHEN ? THEN ? ELSE avoid_agent_role END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_SUMMARIZING)
    .bind(failure.status())
    .bind(&failure.blocked_reason)
    .bind(failure.switch)
    .bind(agent.map(|agent| agent.id.as_str()))
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
        failure.event_kind(failed_kind),
        format!("{label}{}：{reason}", failure.label()),
    )
    .await;
    Ok(())
}
