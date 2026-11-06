//! 项目详情聚合与任务视图构建。
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::domain::deps_complete;
use crate::git;
use crate::models::*;
use crate::state::AppState;
use crate::usage;

use super::agents::agent_view;
use super::helpers::{parse_list, require_project};
use super::ApiError;

pub(super) async fn load_detail(state: &AppState, id: &str) -> Result<ProjectDetail, ApiError> {
    let mut project = require_project(state, id).await?;
    project.user_action = crate::user_action::for_project(&project);
    let tasks = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM tasks WHERE project_id = ? ORDER BY queue_order ASC, seq ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let messages = sqlx::query_as::<_, MessageRow>(
        "SELECT * FROM messages WHERE project_id = ? ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let mut events = sqlx::query_as::<_, EventRow>(
        "SELECT * FROM events WHERE project_id = ? ORDER BY id DESC LIMIT 500",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    events.reverse();
    let events = events.into_iter().map(EventView::from_row).collect();
    let agent_rows =
        sqlx::query_as::<_, AgentRow>("SELECT * FROM agents ORDER BY created_at ASC, id ASC")
            .fetch_all(&state.db)
            .await?;
    let mut agents: Vec<AgentView> = Vec::new();
    for row in agent_rows {
        agents.push(agent_view(state, row).await?);
    }
    let agent_pool = crate::db::load_project_agent_pool(&state.db, id).await?;
    let agent_names: HashMap<String, String> = agents
        .iter()
        .map(|agent| (agent.id.clone(), agent.name.clone()))
        .collect();
    let usage_rows = usage::load_jobs(&state.db, Some(id)).await?;
    let project_usage = usage::project_usage(&usage_rows, id, &agent_names);
    let job_rows = sqlx::query_as::<_, JobRow>(
        "SELECT * FROM jobs WHERE project_id = ? ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let mut jobs = Vec::with_capacity(job_rows.len());
    for job in job_rows {
        let can_continue = crate::continuation::can_continue(state, &job).await;
        let diagnostic = crate::continuation::diagnostic(state, &job).await;
        jobs.push(JobSummary {
            can_continue,
            diagnostic,
            execution_job_id: job.execution_job_id,
            contract_id: job.contract_id,
            id: job.id,
            project_id: job.project_id,
            task_id: job.task_id,
            role: job.role,
            agent_id: job.agent_id.clone(),
            agent_name: job
                .agent_id
                .as_ref()
                .and_then(|agent_id| agent_names.get(agent_id).cloned()),
            status: job.status,
            attempt: job.attempt,
            failure_summary: crate::failure::summarize_classified(
                job.error.as_deref(),
                job.failure_kind.as_deref(),
            ),
            error: job.error,
            exit_code: job.exit_code,
            failure_kind: job.failure_kind,
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
        });
    }
    let merged_at = project.merged_at.clone();
    let merged_commit = project.merged_commit.clone();
    let merged_into_base = project_merged_into_base(&project).await;
    let proposal_raw = if project.replan_active != 0 {
        project.replan_proposal.as_deref()
    } else {
        project.proposal.as_deref()
    };
    let proposal = proposal_raw.and_then(|raw| serde_json::from_str::<AnalysisProposal>(raw).ok());
    let mut task_actions = HashMap::new();
    for task in &tasks {
        if let Some(action) = crate::user_action::for_task(&state.db, task).await? {
            task_actions.insert(task.id.clone(), action);
        }
    }
    let mut tasks = task_views(tasks);
    for task in &mut tasks {
        task.user_action = task_actions.remove(&task.id);
        if let Some(action) = &task.user_action {
            task.waiting_reason = Some(action.summary.clone());
        }
    }
    Ok(ProjectDetail {
        attention_items: crate::attention::items(&state.db, &project, merged_into_base).await?,
        delivery: crate::delivery::snapshot(&state.db, id).await?,
        project,
        merged_at: merged_at.clone(),
        merged_commit: merged_commit.clone(),
        merged_into_base,
        tasks,
        jobs,
        messages,
        events,
        agents,
        agent_pool,
        usage: project_usage,
        proposal,
    })
}

/// 目标分支是否已经包含项目分支提交。比较当前分支提交，覆盖 DOMO 记录的
/// 合入和用户在 DOMO 之外完成的合入；分支不可读时退回已记录的合入状态。
pub(super) async fn project_merged_into_base(project: &ProjectRow) -> bool {
    if project.stage != STAGE_COMPLETED || !project.is_git() {
        return false;
    }
    let repo = PathBuf::from(&project.repo_path);
    let Ok(project_commit) = git::commit_of(&repo, &project.branch).await else {
        return project.merged_at.is_some();
    };
    let Ok(base_commit) = git::commit_of(&repo, &project.base_branch).await else {
        return project.merged_at.is_some();
    };
    git::is_ancestor(&repo, &project_commit, &base_commit)
        .await
        .unwrap_or(false)
}

pub(super) fn job_duration(
    started_at: &Option<String>,
    finished_at: &Option<String>,
) -> Option<i64> {
    usage::elapsed_ms(started_at.as_deref(), finished_at.as_deref()?)
}

/// Queue positions rank ready, queued tasks by the stable queue order. Paused
/// and not-yet-ready tasks are excluded from the dispatch ordering.
pub(super) fn task_views(rows: Vec<TaskRow>) -> Vec<TaskView> {
    let completed: HashSet<String> = rows
        .iter()
        .filter(|task| task.stage == TASK_STAGE_COMPLETED)
        .map(|task| task.key.clone())
        .collect();
    let mut ready: Vec<&TaskRow> = rows
        .iter()
        .filter(|task| {
            task.status.as_deref() == Some(STATUS_QUEUED)
                && task.stage != TASK_STAGE_COMPLETED
                && (task.stage != TASK_STAGE_PENDING
                    || deps_complete(&parse_list(&task.deps), &completed))
        })
        .collect();
    ready.sort_by_key(|task| (task.queue_order, task.seq));
    let positions: HashMap<String, i64> = ready
        .iter()
        .enumerate()
        .map(|(index, task)| (task.id.clone(), (index + 1) as i64))
        .collect();
    rows.into_iter()
        .map(|row| {
            let position = positions.get(&row.id).copied();
            let mut view = TaskView::from_row(row);
            if view.stage == TASK_STAGE_PENDING
                && view.status.as_deref() == Some(STATUS_QUEUED)
                && !deps_complete(&view.deps, &completed)
            {
                view.status = None;
            }
            view.queue_position = position;
            view.waiting_reason = task_waiting_reason(&view, &completed);
            view
        })
        .collect()
}

pub(super) fn task_waiting_reason(task: &TaskView, completed: &HashSet<String>) -> Option<String> {
    match task.status.as_deref() {
        Some(STATUS_CANCELED) => return Some("任务已取消".into()),
        Some(STATUS_PAUSED) => return Some("任务已暂停".into()),
        Some(STATUS_PAUSING) => return Some("暂停中，等待在途作业结束".into()),
        Some(STATUS_BLOCKED) => {
            if let Some(action) = &task.user_action {
                return Some(action.summary.clone());
            }
            if task.last_failure_kind.as_deref() == Some("configuration") {
                return task
                    .last_error
                    .clone()
                    .or_else(|| Some("请检查 Agent 配置".into()));
            }
            return Some(
                if task.human_gate.as_deref() == Some(HUMAN_GATE_MILESTONE) {
                    "等待人工验收".into()
                } else {
                    "等待人工裁决".into()
                },
            );
        }
        _ => {}
    }
    if task.stage == TASK_STAGE_COMPLETED {
        return None;
    }
    if task.stage == TASK_STAGE_PENDING {
        let missing: Vec<String> = task
            .deps
            .iter()
            .filter(|dep| !completed.contains(*dep))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Some(format!("等待前置任务：{}", missing.join("、")));
        }
    }
    if task.queue_position.is_some() {
        return task
            .waiting_reason
            .clone()
            .or_else(|| Some("等待派发".into()));
    }
    task.waiting_reason.clone()
}
