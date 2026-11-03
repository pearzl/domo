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
        return (!state.merged_into_base).then_some(ProjectAttention::Merge);
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
