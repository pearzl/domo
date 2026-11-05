use crate::models::{
    ProjectAttention, STAGE_ANALYSIS, STAGE_COMPLETED, STAGE_SUMMARIZING, STATUS_ARCHIVED,
    STATUS_BLOCKED, STATUS_PAUSED, STATUS_PAUSING,
};

/// 列表待办分类所需的当前项目事实，避免读取完整项目详情。
pub(crate) struct ProjectAttentionState<'a> {
    pub stage: &'a str,
    pub status: Option<&'a str>,
    pub goal: &'a str,
    pub blocked_reason: Option<&'a str>,
    pub proposal_pending: bool,
    pub report_ready: bool,
    pub merged_into_base: bool,
    /// 普通工作区没有交付分支，完成即交付，不产生合入待办。
    pub requires_merge: bool,
    pub latest_project_job_failed: bool,
    pub analysis_running: bool,
}

pub(crate) fn project_attention(state: ProjectAttentionState<'_>) -> Option<ProjectAttention> {
    if matches!(
        state.status,
        Some(STATUS_ARCHIVED | STATUS_PAUSED | STATUS_PAUSING)
    ) {
        return None;
    }
    if state.stage == STAGE_ANALYSIS {
        let manual_block = state.status == Some(STATUS_BLOCKED)
            && !state.goal.trim().is_empty()
            && state
                .blocked_reason
                .is_some_and(|reason| !reason.trim().is_empty());
        if manual_block {
            return Some(blocked_attention(state.latest_project_job_failed));
        }
        if state.analysis_running {
            return None;
        }
        if state.proposal_pending {
            return Some(ProjectAttention::Confirm);
        }
        return (state.status == Some(STATUS_BLOCKED)).then_some(ProjectAttention::Blocked);
    }
    if state.stage == STAGE_SUMMARIZING {
        if state.report_ready {
            return Some(ProjectAttention::Review);
        }
        return (state.status == Some(STATUS_BLOCKED))
            .then_some(blocked_attention(state.latest_project_job_failed));
    }
    if state.stage == STAGE_COMPLETED {
        return (state.requires_merge && !state.merged_into_base)
            .then_some(ProjectAttention::Merge);
    }
    (state.status == Some(STATUS_BLOCKED))
        .then_some(blocked_attention(state.latest_project_job_failed))
}

fn blocked_attention(latest_project_job_failed: bool) -> ProjectAttention {
    if latest_project_job_failed {
        ProjectAttention::Failed
    } else {
        ProjectAttention::Blocked
    }
}

/// Human gates are independent of the project's ability to run other work.
pub(crate) async fn items(
    pool: &sqlx::SqlitePool,
    project: &crate::models::ProjectRow,
    merged_into_base: bool,
) -> Result<Vec<crate::models::AttentionItem>, sqlx::Error> {
    use crate::models::*;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Ok(Vec::new());
    }
    let latest_project_job_status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM jobs WHERE project_id = ? AND task_id IS NULL \
         ORDER BY rowid DESC LIMIT 1",
    )
    .bind(&project.id)
    .fetch_optional(pool)
    .await?;
    let analysis_running: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE project_id = ? AND task_id IS NULL \
         AND role = 'analysis' AND status = 'running'",
    )
    .bind(&project.id)
    .fetch_one(pool)
    .await?;
    let proposal = if project.replan_active != 0 {
        project.replan_proposal.as_deref()
    } else {
        project.proposal.as_deref()
    };
    let project_attention = project_attention(ProjectAttentionState {
        stage: &project.stage,
        status: project.status.as_deref(),
        goal: &project.goal,
        blocked_reason: project.blocked_reason.as_deref(),
        proposal_pending: proposal.is_some(),
        report_ready: project
            .report
            .as_deref()
            .is_some_and(|report| !report.is_empty()),
        merged_into_base,
        requires_merge: project.is_git(),
        latest_project_job_failed: latest_project_job_status.as_deref() == Some(JOB_STATUS_FAILED),
        analysis_running: analysis_running > 0,
    });
    let tasks =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? ORDER BY seq")
            .bind(&project.id)
            .fetch_all(pool)
            .await?;
    let mut result = Vec::new();
    for task in tasks.iter().filter(|task| {
        task.status.as_deref() == Some(STATUS_BLOCKED) && task.stage != TASK_STAGE_COMPLETED
    }) {
        let deciding: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM replan_requests WHERE task_id = ? AND state IN ('pending', 'running', 'planned', 'ready')")
            .bind(&task.id).fetch_one(pool).await?;
        if deciding > 0 {
            continue;
        }
        let mut affected = std::collections::HashSet::from([task.key.clone()]);
        loop {
            let before = affected.len();
            for candidate in &tasks {
                if parse_string_list(&candidate.deps)
                    .iter()
                    .any(|dep| affected.contains(dep))
                {
                    affected.insert(candidate.key.clone());
                }
            }
            if before == affected.len() {
                break;
            }
        }
        let since: Option<String> = sqlx::query_scalar(
            "SELECT MAX(created_at) FROM events WHERE task_id = ? AND kind = 'task_state_changed' \
             AND json_extract(payload, '$.status') = 'blocked'",
        )
        .bind(&task.id)
        .fetch_one(pool)
        .await?;
        result.push(AttentionItem {
            id: task.id.clone(),
            kind: if task.human_gate.is_some() {
                ProjectAttention::Review
            } else {
                ProjectAttention::Blocked
            },
            task: Some(ProjectAttentionTask {
                id: task.id.clone(),
                key: task.key.clone(),
                title: task.title.clone(),
            }),
            reason: task.last_error.clone(),
            since: since.unwrap_or_else(|| task.updated_at.clone()),
            affected_tasks: affected.len().saturating_sub(1),
            blocks_milestone: tasks
                .iter()
                .any(|candidate| candidate.milestone != 0 && affected.contains(&candidate.key)),
            blocks_summary: true,
            project_advancing: project.status.as_deref() == Some(STATUS_RUNNING),
        });
    }
    if let Some(kind) = project_attention {
        if result.is_empty()
            || matches!(kind, ProjectAttention::Confirm | ProjectAttention::Merge)
            || project.stage == STAGE_SUMMARIZING
        {
            let since: Option<String> = sqlx::query_scalar(
                "SELECT MAX(created_at) FROM events WHERE project_id = ? AND task_id IS NULL \
                 AND (kind IN ('analysis_questions', 'analysis_proposed', 'analysis_direct', 'report_ready') \
                 OR (kind = 'project_state_changed' AND json_extract(payload, '$.status') = 'blocked'))",
            ).bind(&project.id).fetch_one(pool).await?;
            result.push(AttentionItem {
                id: project.id.clone(),
                kind,
                task: None,
                reason: project.blocked_reason.clone(),
                since: since.unwrap_or_else(|| project.updated_at.clone()),
                affected_tasks: tasks
                    .iter()
                    .filter(|task| {
                        task.stage != TASK_STAGE_COMPLETED
                            && task.status.as_deref() != Some(STATUS_CANCELED)
                    })
                    .count(),
                blocks_milestone: false,
                blocks_summary: project.stage != STAGE_COMPLETED,
                project_advancing: project.status.as_deref() == Some(STATUS_RUNNING),
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(stage: &str) -> ProjectAttentionState<'_> {
        ProjectAttentionState {
            stage,
            status: Some("running"),
            goal: "交付功能",
            blocked_reason: None,
            proposal_pending: false,
            report_ready: false,
            merged_into_base: false,
            requires_merge: true,
            latest_project_job_failed: false,
            analysis_running: false,
        }
    }

    #[test]
    fn classifies_each_actionable_project() {
        let confirm = project_attention(ProjectAttentionState {
            status: Some(STATUS_BLOCKED),
            proposal_pending: true,
            ..state(STAGE_ANALYSIS)
        });
        assert_eq!(confirm, Some(ProjectAttention::Confirm));

        let review = project_attention(ProjectAttentionState {
            status: Some(STATUS_BLOCKED),
            report_ready: true,
            latest_project_job_failed: true,
            ..state(STAGE_SUMMARIZING)
        });
        assert_eq!(review, Some(ProjectAttention::Review));

        let failed = project_attention(ProjectAttentionState {
            status: Some(STATUS_BLOCKED),
            blocked_reason: Some("规划作业失败"),
            latest_project_job_failed: true,
            ..state(STAGE_ANALYSIS)
        });
        assert_eq!(failed, Some(ProjectAttention::Failed));

        let merge = project_attention(state(STAGE_COMPLETED));
        assert_eq!(merge, Some(ProjectAttention::Merge));

        let delivered = project_attention(ProjectAttentionState {
            requires_merge: false,
            ..state(STAGE_COMPLETED)
        });
        assert_eq!(delivered, None);

        let blocked = project_attention(ProjectAttentionState {
            status: Some(STATUS_BLOCKED),
            blocked_reason: Some("任务需要人工裁决"),
            ..state("executing")
        });
        assert_eq!(blocked, Some(ProjectAttention::Blocked));
    }

    #[test]
    fn inactive_projects_do_not_report_attention() {
        let running = project_attention(ProjectAttentionState {
            analysis_running: true,
            proposal_pending: true,
            ..state(STAGE_ANALYSIS)
        });
        assert_eq!(running, None);

        for status in [STATUS_ARCHIVED, STATUS_PAUSED, STATUS_PAUSING] {
            let paused = project_attention(ProjectAttentionState {
                status: Some(status),
                proposal_pending: true,
                ..state(STAGE_ANALYSIS)
            });
            assert_eq!(paused, None);
        }

        let merged = project_attention(ProjectAttentionState {
            merged_into_base: true,
            ..state(STAGE_COMPLETED)
        });
        assert_eq!(merged, None);

        let executing = project_attention(ProjectAttentionState {
            latest_project_job_failed: true,
            ..state("executing")
        });
        assert_eq!(executing, None);
    }
}
