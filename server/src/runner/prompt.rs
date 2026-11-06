use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::json;

use crate::git;
use crate::models::*;
use crate::paths;
use crate::prompts::{DependencyInfo, DerivedTaskInfo, PromptData, SummaryTaskInfo, TaskInfo};
use crate::state::AppState;

use super::common::{load_task, parse_json_list};

pub(super) async fn build_prompt_data(
    state: &AppState,
    project: &ProjectRow,
    task: Option<&TaskRow>,
    job: &JobRow,
    cwd: &Path,
) -> Result<PromptData> {
    let mut dependencies = Vec::new();
    let mut task_info = None;
    let mut verifier_issues = Vec::new();
    let mut derived_from = None;
    let mut derived_tasks = Vec::new();
    if let Some(task) = task {
        let deps = parse_json_list(&task.deps);
        verifier_issues = parse_json_list(&task.verifier_issues);
        for dep in &deps {
            if let Some(row) = sqlx::query_as::<_, TaskRow>(
                "SELECT * FROM tasks WHERE project_id = ? AND key = ? \
                 AND COALESCE(status, '') != ? ORDER BY seq DESC LIMIT 1",
            )
            .bind(&project.id)
            .bind(dep)
            .bind(STATUS_CANCELED)
            .fetch_optional(&state.db)
            .await?
            {
                dependencies.push(DependencyInfo {
                    key: row.key.clone(),
                    title: row.title.clone(),
                    summary: row.result_summary.unwrap_or_default(),
                });
            }
        }
        if let Some(parent_id) = &task.parent_id {
            if let Some(row) = load_task(&state.db, parent_id).await? {
                derived_from = Some(DependencyInfo {
                    key: row.key.clone(),
                    title: row.title.clone(),
                    summary: row.result_summary.unwrap_or_default(),
                });
            }
        }
        let children = sqlx::query_as::<_, TaskRow>(
            "SELECT * FROM tasks WHERE project_id = ? AND parent_id = ? ORDER BY seq ASC",
        )
        .bind(&project.id)
        .bind(&task.id)
        .fetch_all(&state.db)
        .await?;
        for row in children {
            derived_tasks.push(DerivedTaskInfo {
                key: row.key,
                title: row.title,
                stage: row.stage,
                status: row.status,
                summary: row.result_summary.unwrap_or_default(),
            });
        }
        let verification_result: Option<String> = sqlx::query_scalar(
            "SELECT result FROM jobs WHERE task_id = ? AND role = 'verifier' AND status = 'succeeded' ORDER BY rowid DESC LIMIT 1")
            .bind(&task.id).fetch_optional(&state.db).await?.flatten();
        let verification_guidance = verification_result
            .as_deref()
            .and_then(crate::domain::extract_json)
            .filter(|result| result["pass"] == false)
            .and_then(|result| result["guidance"].as_str().map(str::to_string));
        let guidance = match verification_guidance {
            Some(guidance) => format!("{}\n验收处置指导：{guidance}", task.guidance),
            None => task.guidance.clone(),
        };
        task_info = Some(TaskInfo {
            key: task.key.clone(),
            title: task.title.clone(),
            description: task.description.clone(),
            acceptance: parse_json_list(&task.acceptance),
            guidance,
        });
    }
    let messages = sqlx::query_as::<_, MessageRow>(
        "SELECT * FROM messages WHERE project_id = ? ORDER BY created_at ASC",
    )
    .bind(&project.id)
    .fetch_all(&state.db)
    .await?;
    let mut messages = messages;
    if matches!(job.role.as_str(), JOB_ROLE_ANALYSIS | JOB_ROLE_DIRECT) {
        // 报告评议由只读报告重写作业处理，不改变分析与实施输入。
        messages.retain(|message| message.role != REPORT_MESSAGE_ROLE);
    }
    let mut summary_tasks = Vec::new();
    let mut completed_tasks = Vec::new();
    let mut canceled_tasks = Vec::new();
    let rows =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? ORDER BY seq ASC")
            .bind(&project.id)
            .fetch_all(&state.db)
            .await?;
    for row in rows
        .iter()
        .filter(|row| row.status.as_deref() == Some(STATUS_CANCELED))
    {
        canceled_tasks.push(SummaryTaskInfo {
            key: row.key.clone(),
            title: row.title.clone(),
            summary: row.last_error.clone().unwrap_or_default(),
        });
    }
    let partition = crate::replan::partition_tasks(rows);
    for row in &partition.retained {
        summary_tasks.push(SummaryTaskInfo {
            key: row.key.clone(),
            title: row.title.clone(),
            summary: row.result_summary.clone().unwrap_or_default(),
        });
    }
    for row in partition.retained {
        completed_tasks.push(SummaryTaskInfo {
            key: row.key,
            title: row.title,
            summary: row.result_summary.unwrap_or_default(),
        });
    }
    for row in partition.canceled {
        canceled_tasks.push(SummaryTaskInfo {
            key: row.key,
            title: row.title,
            summary: row.result_summary.unwrap_or_default(),
        });
    }
    let workspace = PathBuf::from(&project.workspace);
    let diff_stat = if project.is_git() {
        match job.role.as_str() {
            JOB_ROLE_VERIFIER => {
                let branch = task.and_then(|task| task.branch.clone());
                if let Some(branch) = branch {
                    git::diff_between(
                        &paths::project_repo(&workspace),
                        &project.base_branch,
                        &branch,
                    )
                    .await
                    .unwrap_or_default()
                } else {
                    String::new()
                }
            }
            JOB_ROLE_SUMMARY | JOB_ROLE_ACCEPTANCE => git::diff_between(
                &paths::project_repo(&workspace),
                &project.base_branch,
                &project.branch,
            )
            .await
            .unwrap_or_default(),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let job_dir = paths::job_dir(&workspace, &job.id);
    let proposal = project
        .proposal
        .as_deref()
        .and_then(|raw| serde_json::from_str::<AnalysisProposal>(raw).ok());
    let mut acceptance_criteria = proposal
        .as_ref()
        .map(|value| value.acceptance.clone())
        .unwrap_or_default();
    if acceptance_criteria.is_empty() {
        acceptance_criteria = proposal
            .as_ref()
            .map(|value| value.verification.clone())
            .unwrap_or_default();
    }
    let project_summary = proposal.as_ref().map(|value| value.summary.clone());
    let project_assumptions = proposal
        .as_ref()
        .map(|value| value.assumptions.clone())
        .unwrap_or_default();
    Ok(PromptData {
        role: job.role.clone(),
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        goal: project.goal.clone(),
        cwd: cwd.to_string_lossy().to_string(),
        repo_path: project.repo_path.clone(),
        workspace_type: project.workspace_type.clone(),
        base_branch: project.base_branch.clone(),
        project_branch: project.branch.clone(),
        task: task_info,
        dependencies,
        derived_from,
        derived_tasks,
        messages,
        verifier_issues,
        execution_error: job
            .error
            .clone()
            .or_else(|| task.and_then(|task| task.last_error.clone())),
        summary_tasks,
        completed_tasks,
        canceled_tasks,
        diff_stat,
        job_dir: job_dir.to_string_lossy().to_string(),
        project_resources_dir: paths::project_resources(&workspace)
            .to_string_lossy()
            .to_string(),
        task_resources_dir: task.map(|task| {
            paths::task_resources(&workspace, task.seq)
                .to_string_lossy()
                .to_string()
        }),
        result_path: job_dir.join("result.json").to_string_lossy().to_string(),
        attempt: job.attempt,
        report_feedback: project.report_feedback.clone(),
        replan_reason: project.replan_reason.clone(),
        replan_source: project.replan_source.clone(),
        acceptance_criteria,
        acceptance_result: project.acceptance.clone(),
        direct_result: if project.completion_mode == COMPLETION_DIRECT
            && job.role == JOB_ROLE_SUMMARY
        {
            sqlx::query_scalar::<_, Option<String>>("SELECT result FROM jobs WHERE project_id = ? AND role = 'direct' AND status = 'succeeded' AND replan_generation = ? ORDER BY rowid DESC LIMIT 1")
                .bind(&project.id).bind(project.replan_generation).fetch_optional(&state.db).await?.flatten()
        } else {
            None
        },
        project_summary,
        project_assumptions,
        confirm_mode: project.confirm_mode.clone(),
    })
}

pub(super) fn build_context(
    project: &ProjectRow,
    task: Option<&TaskRow>,
    job: &JobRow,
    cwd: &Path,
    job_dir: &Path,
    data: &PromptData,
    agent: &AgentRow,
) -> serde_json::Value {
    json!({
        "job_id": job.id,
        "role": job.role,
        "agent": {
            "id": agent.id,
            "name": agent.name,
            "kind": agent.kind,
            "model": agent.model,
        },
        "cwd": cwd.to_string_lossy(),
        "job_dir": job_dir.to_string_lossy(),
        "work_resources": {
            "project": data.project_resources_dir,
            "task": data.task_resources_dir,
        },
        "project": {
            "id": project.id,
            "name": project.name,
            "goal": project.goal,
            "repo_path": project.repo_path,
            "workspace_type": project.workspace_type,
            "base_branch": project.base_branch,
            "branch": project.branch,
            "replan_reason": project.replan_reason,
            "replan_source": project.replan_source,
            "confirm_mode": project.confirm_mode,
            "completion_mode": project.completion_mode,
            "confirmed_at": project.confirmed_at,
            "acceptance_at": project.acceptance_at,
        },
        "task": task.map(|task| json!({
            "id": task.id,
            "key": task.key,
            "title": task.title,
            "description": task.description,
            "acceptance": parse_json_list(&task.acceptance),
            "deps": parse_json_list(&task.deps),
            "parent_id": task.parent_id,
            "stage": task.stage,
            "attempts": task.attempts,
            "rejections": task.rejections,
            "guidance": data.task.as_ref().map(|info| info.guidance.as_str()).unwrap_or(&task.guidance),
        })),
        "parent_task": data.derived_from.as_ref().map(|parent| json!({
            "key": parent.key, "title": parent.title, "summary": parent.summary,
        })),
        "derived_tasks": data.derived_tasks.iter().map(|task| json!({
            "key": task.key, "title": task.title, "stage": task.stage,
            "status": task.status, "summary": task.summary,
        })).collect::<Vec<_>>(),
        "dependencies": data.dependencies.iter().map(|dep| json!({
            "key": dep.key, "title": dep.title, "summary": dep.summary,
        })).collect::<Vec<_>>(),
        "messages": data.messages.iter().map(|message| json!({
            "role": message.role, "content": message.content,
        })).collect::<Vec<_>>(),
        "verifier_issues": data.verifier_issues,
        "execution_error": data.execution_error,
        "summary_tasks": data.summary_tasks.iter().map(|task| json!({
            "key": task.key, "title": task.title, "summary": task.summary,
        })).collect::<Vec<_>>(),
        "completed_tasks": data.completed_tasks.iter().map(|task| json!({
            "key": task.key, "title": task.title, "summary": task.summary,
        })).collect::<Vec<_>>(),
        "canceled_tasks": data.canceled_tasks.iter().map(|task| json!({
            "key": task.key, "title": task.title, "summary": task.summary,
        })).collect::<Vec<_>>(),
        "diff_stat": data.diff_stat,
        "acceptance_criteria": data.acceptance_criteria,
        "acceptance_result": data.acceptance_result,
        "direct_result": data.direct_result,
        "report_feedback": data.report_feedback,
        "project_summary": data.project_summary,
        "project_assumptions": data.project_assumptions,
    })
}
