//! Job lifecycle. Long-running agent processes never hold the scheduler lock;
//! state transitions are applied atomically after the process exits.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::json;
use sqlx::SqlitePool;
use tokio::fs;

use crate::agents::{self, AgentSpec};
use crate::domain::{self, now, parse_result, short_id, slug, ParsedResult};
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::prompts::{self, DependencyInfo, PromptData, SummaryTaskInfo, TaskInfo};
use crate::state::AppState;

pub async fn execute_job(state: AppState, job_id: String) {
    if let Err(error) = run_job(&state, &job_id).await {
        tracing::error!(job_id, error = %error, "作业执行失败");
        if let Err(inner) = fail_runtime(&state, &job_id, &format!("{error:#}")).await {
            tracing::error!(job_id, error = %inner, "记录作业失败状态时出错");
        }
    }
    state.notify_scheduler();
}

async fn run_job(state: &AppState, job_id: &str) -> Result<()> {
    let Some(job) = load_job(&state.db, job_id).await? else {
        return Ok(());
    };
    if job.status != JOB_STATUS_RUNNING {
        return Ok(());
    }
    let project = load_project(&state.db, &job.project_id)
        .await?
        .ok_or_else(|| anyhow!("项目不存在"))?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        mark_archived_job(state, &job).await?;
        return Ok(());
    }
    let task = match &job.task_id {
        Some(task_id) => load_task(&state.db, task_id).await?,
        None => None,
    };
    let agent = match &job.agent_id {
        Some(agent_id) => crate::db::load_agent(&state.db, agent_id).await?,
        None => None,
    };
    let Some(agent) = agent else {
        return Err(anyhow!("作业没有关联可用 Agent"));
    };

    let job_dir = PathBuf::from(&project.workspace).join("jobs").join(&job.id);
    fs::create_dir_all(&job_dir).await?;
    let result_path = job_dir.join("result.json");
    let _ = fs::remove_file(&result_path).await;

    let workspace = PathBuf::from(&project.workspace);
    let cwd = match job.role.as_str() {
        JOB_ROLE_GOAL | JOB_ROLE_SUMMARY => workspace.join("repo"),
        JOB_ROLE_EXECUTOR => prepare_task_worktree(state, &project, task.as_ref()).await?,
        JOB_ROLE_VERIFIER => {
            prepare_verify_worktree(state, &project, task.as_ref(), &job.id).await?
        }
        other => return Err(anyhow!("未知作业角色: {other}")),
    };

    let prompt_data = build_prompt_data(state, &project, task.as_ref(), &job).await?;
    let prompt = prompts::render(&prompt_data);
    fs::write(job_dir.join("prompt.md"), &prompt).await?;
    let context = build_context(&project, task.as_ref(), &job, &cwd, &job_dir, &prompt_data);
    fs::write(
        job_dir.join("context.json"),
        serde_json::to_vec_pretty(&context)?,
    )
    .await?;

    sqlx::query("UPDATE jobs SET prompt = ?, started_at = ? WHERE id = ?")
        .bind(&prompt)
        .bind(now())
        .bind(&job.id)
        .execute(&state.db)
        .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        job.task_id.as_deref(),
        "job_started",
        format!("{} 作业开始（{}）", role_label(&job.role), agent.name),
    )
    .await;

    let current = load_project(&state.db, &project.id)
        .await?
        .ok_or_else(|| anyhow!("项目不存在"))?;
    if current.status.as_deref() == Some(STATUS_ARCHIVED) {
        mark_archived_job(state, &job).await?;
        return Ok(());
    }

    let spec = AgentSpec {
        job_id: job.id.clone(),
        role: job.role.clone(),
        cwd: cwd.clone(),
        job_dir: job_dir.clone(),
        prompt: prompt.clone(),
    };
    let run = match tokio::time::timeout(
        state.cfg.job_timeout,
        agents::execute(&agent, &spec, state.cfg.job_timeout),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            agents::kill_job_dir(&job_dir);
            agents::AgentRun {
                stdout: String::new(),
                stderr: "作业超时，进程已终止".into(),
                exit_code: None,
                timed_out: true,
            }
        }
    };

    let raw_result = fs::read_to_string(&result_path).await.ok();
    if job.role == JOB_ROLE_VERIFIER {
        // The verifier workspace is disposable and must not leak into later work.
        let _guard = state.git_lock.lock().await;
        let _ = git::remove_worktree(&workspace.join("repo"), &cwd).await;
    }
    if job.role == JOB_ROLE_GOAL || job.role == JOB_ROLE_SUMMARY {
        let _guard = state.git_lock.lock().await;
        let _ = git::reset_hard(&workspace.join("repo")).await;
    }

    let fresh_project = load_project(&state.db, &project.id)
        .await?
        .ok_or_else(|| anyhow!("项目不存在"))?;
    if fresh_project.status.as_deref() == Some(STATUS_ARCHIVED) {
        mark_archived_job(state, &job).await?;
        return Ok(());
    }

    let process_ok = run.exit_code == Some(0) && !run.timed_out;
    let (status, result_text, error) = if !process_ok {
        let reason = if run.timed_out {
            "Agent 作业超时".to_string()
        } else {
            let detail = format!("{}\n{}", run.stderr, run.stdout);
            format!(
                "Agent 进程退出码 {:?}: {}",
                run.exit_code,
                first_lines(&detail, 30)
            )
        };
        (JOB_STATUS_FAILED, raw_result.clone(), Some(reason))
    } else if let Some(raw) = &raw_result {
        match parse_result(&job.role, raw) {
            Ok(parsed) => {
                let status = match &parsed {
                    ParsedResult::ExecutorFailed { .. } => JOB_STATUS_FAILED,
                    _ => JOB_STATUS_SUCCEEDED,
                };
                (status, Some(raw.clone()), parsed_error(&parsed))
            }
            Err(parse_error) => (
                JOB_STATUS_FAILED,
                Some(raw.clone()),
                Some(format!("结果协议不合法: {parse_error}")),
            ),
        }
    } else {
        (
            JOB_STATUS_FAILED,
            None,
            Some("Agent 没有写 result.json".to_string()),
        )
    };

    let parsed = if status == JOB_STATUS_SUCCEEDED {
        result_text
            .as_deref()
            .and_then(|raw| parse_result(&job.role, raw).ok())
    } else {
        None
    };

    if job.role == JOB_ROLE_EXECUTOR {
        if let (Some(task), Some(ParsedResult::ExecutorDone { summary })) =
            (task.as_ref(), parsed.clone())
        {
            if status == JOB_STATUS_SUCCEEDED {
                let message = format!(
                    "domo(task-{}): {} (attempt {})",
                    task.key, task.title, job.attempt
                );
                let commit = {
                    let _guard = state.git_lock.lock().await;
                    git::commit_all(&cwd, &message).await
                };
                if let Err(commit_error) = commit {
                    let error_text = format!("提交执行结果失败: {commit_error:#}");
                    let transition =
                        handle_executor_failure(state, &project, task, &error_text, &agent).await;
                    finish_job(
                        state,
                        &job,
                        JOB_STATUS_FAILED,
                        result_text,
                        &run,
                        Some(&error_text),
                        None,
                    )
                    .await?;
                    transition?;
                    return Ok(());
                }
                let transition =
                    handle_executor_success(state, &project, task, &summary, &agent).await;
                finish_job(
                    state,
                    &job,
                    JOB_STATUS_SUCCEEDED,
                    result_text,
                    &run,
                    None,
                    None,
                )
                .await?;
                transition?;
                return Ok(());
            }
        }
    }

    let transition: Result<()> = match (job.role.as_str(), &parsed) {
        (JOB_ROLE_GOAL, Some(ParsedResult::Questions { questions, summary })) => {
            handle_goal_questions(state, &project, questions, summary).await
        }
        (JOB_ROLE_GOAL, Some(ParsedResult::Plan { summary, tasks })) => {
            handle_goal_plan(state, &project, summary, tasks).await
        }
        (JOB_ROLE_GOAL, _) => {
            handle_goal_failure(state, &project, error.as_deref().unwrap_or("规划失败")).await
        }
        (JOB_ROLE_EXECUTOR, Some(ParsedResult::ExecutorDone { summary })) => {
            if let Some(task) = task.as_ref() {
                handle_executor_success(state, &project, task, summary, &agent).await
            } else {
                Ok(())
            }
        }
        (JOB_ROLE_EXECUTOR, _) => {
            if let Some(task) = task.as_ref() {
                let reason = error
                    .as_deref()
                    .or(match &parsed {
                        Some(ParsedResult::ExecutorFailed { reason }) => Some(reason.as_str()),
                        _ => None,
                    })
                    .unwrap_or("执行失败");
                handle_executor_failure(state, &project, task, reason, &agent).await
            } else {
                Ok(())
            }
        }
        (
            JOB_ROLE_VERIFIER,
            Some(ParsedResult::Verdict {
                pass,
                reason,
                issues,
            }),
        ) => {
            if let Some(task) = task.as_ref() {
                handle_verdict(state, &project, task, *pass, reason, issues, &agent).await
            } else {
                Ok(())
            }
        }
        (JOB_ROLE_VERIFIER, _) => {
            if let Some(task) = task.as_ref() {
                let reason = error.as_deref().unwrap_or("验收作业失败");
                handle_verifier_failure(state, &project, task, reason, &agent).await
            } else {
                Ok(())
            }
        }
        (JOB_ROLE_SUMMARY, Some(ParsedResult::Report { summary })) => {
            handle_report(state, &project, summary).await
        }
        (JOB_ROLE_SUMMARY, _) => {
            handle_summary_failure(state, &project, error.as_deref().unwrap_or("总结失败")).await
        }
        _ => Ok(()),
    };
    finish_job(
        state,
        &job,
        status,
        result_text,
        &run,
        error.as_deref(),
        parsed.clone(),
    )
    .await?;
    transition?;
    Ok(())
}

async fn build_prompt_data(
    state: &AppState,
    project: &ProjectRow,
    task: Option<&TaskRow>,
    job: &JobRow,
) -> Result<PromptData> {
    let mut dependencies = Vec::new();
    let mut task_info = None;
    let mut verifier_issues = Vec::new();
    if let Some(task) = task {
        let deps = parse_json_list(&task.deps);
        verifier_issues = parse_json_list(&task.verifier_issues);
        for dep in &deps {
            if let Some(row) =
                sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? AND key = ?")
                    .bind(&project.id)
                    .bind(dep)
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
        task_info = Some(TaskInfo {
            key: task.key.clone(),
            title: task.title.clone(),
            description: task.description.clone(),
            acceptance: parse_json_list(&task.acceptance),
        });
    }
    let messages = sqlx::query_as::<_, MessageRow>(
        "SELECT * FROM messages WHERE project_id = ? ORDER BY created_at ASC",
    )
    .bind(&project.id)
    .fetch_all(&state.db)
    .await?;
    let mut summary_tasks = Vec::new();
    let rows =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? ORDER BY seq ASC")
            .bind(&project.id)
            .fetch_all(&state.db)
            .await?;
    for row in rows {
        summary_tasks.push(SummaryTaskInfo {
            key: row.key,
            title: row.title,
            summary: row.result_summary.unwrap_or_default(),
        });
    }
    let workspace = PathBuf::from(&project.workspace);
    let diff_stat = match job.role.as_str() {
        JOB_ROLE_VERIFIER => {
            let branch = task.and_then(|task| task.branch.clone());
            if let Some(branch) = branch {
                git::diff_between(&workspace.join("repo"), &project.base_branch, &branch)
                    .await
                    .unwrap_or_default()
            } else {
                String::new()
            }
        }
        JOB_ROLE_SUMMARY => git::diff_between(
            &workspace.join("repo"),
            &project.base_branch,
            &project.branch,
        )
        .await
        .unwrap_or_default(),
        _ => String::new(),
    };
    let job_dir = workspace.join("jobs").join(&job.id);
    Ok(PromptData {
        role: job.role.clone(),
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        goal: project.goal.clone(),
        repo_path: project.repo_path.clone(),
        base_branch: project.base_branch.clone(),
        project_branch: project.branch.clone(),
        task: task_info,
        dependencies,
        messages,
        verifier_issues,
        summary_tasks,
        diff_stat,
        job_dir: job_dir.to_string_lossy().to_string(),
        result_path: job_dir.join("result.json").to_string_lossy().to_string(),
        attempt: job.attempt,
        report_feedback: project.report_feedback.clone(),
    })
}

fn build_context(
    project: &ProjectRow,
    task: Option<&TaskRow>,
    job: &JobRow,
    cwd: &Path,
    job_dir: &Path,
    data: &PromptData,
) -> serde_json::Value {
    json!({
        "job_id": job.id,
        "role": job.role,
        "cwd": cwd.to_string_lossy(),
        "job_dir": job_dir.to_string_lossy(),
        "project": {
            "id": project.id,
            "name": project.name,
            "goal": project.goal,
            "repo_path": project.repo_path,
            "base_branch": project.base_branch,
            "branch": project.branch,
        },
        "task": task.map(|task| json!({
            "id": task.id,
            "key": task.key,
            "title": task.title,
            "description": task.description,
            "acceptance": parse_json_list(&task.acceptance),
            "deps": parse_json_list(&task.deps),
            "stage": task.stage,
            "attempts": task.attempts,
            "rejections": task.rejections,
        })),
        "dependencies": data.dependencies.iter().map(|dep| json!({
            "key": dep.key, "title": dep.title, "summary": dep.summary,
        })).collect::<Vec<_>>(),
        "messages": data.messages.iter().map(|message| json!({
            "role": message.role, "content": message.content,
        })).collect::<Vec<_>>(),
        "verifier_issues": data.verifier_issues,
        "summary_tasks": data.summary_tasks.iter().map(|task| json!({
            "key": task.key, "title": task.title, "summary": task.summary,
        })).collect::<Vec<_>>(),
        "diff_stat": data.diff_stat,
    })
}

async fn prepare_task_worktree(
    state: &AppState,
    project: &ProjectRow,
    task: Option<&TaskRow>,
) -> Result<PathBuf> {
    let task = task.ok_or_else(|| anyhow!("执行作业缺少任务"))?;
    if let Some(worktree) = &task.worktree {
        let path = PathBuf::from(worktree);
        if path.join(".git").exists() {
            return Ok(path);
        }
    }
    let workspace = PathBuf::from(&project.workspace);
    let path = workspace.join("tasks").join(&task.id);
    if path.exists() {
        let _ = tokio::fs::remove_dir_all(&path).await;
    }
    let branch = format!(
        "domo-task/{}/{}-{}",
        short_id(&project.id),
        task.seq,
        slug(&task.key)
    );
    {
        let _guard = state.git_lock.lock().await;
        git::create_task_worktree(&workspace.join("repo"), &project.branch, &path, &branch).await?;
    }
    sqlx::query("UPDATE tasks SET worktree = ?, branch = ?, updated_at = ? WHERE id = ?")
        .bind(path.to_string_lossy().to_string())
        .bind(&branch)
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
    Ok(path)
}

async fn prepare_verify_worktree(
    state: &AppState,
    project: &ProjectRow,
    task: Option<&TaskRow>,
    job_id: &str,
) -> Result<PathBuf> {
    let task = task.ok_or_else(|| anyhow!("验收作业缺少任务"))?;
    let branch = task
        .branch
        .clone()
        .ok_or_else(|| anyhow!("任务没有执行分支，无法验收"))?;
    let workspace = PathBuf::from(&project.workspace);
    let path = workspace
        .join("verify")
        .join(format!("{}-{}", task.id, short_id(job_id)));
    if path.exists() {
        let _guard = state.git_lock.lock().await;
        let _ = git::remove_worktree(&workspace.join("repo"), &path).await;
        if path.exists() {
            let _ = tokio::fs::remove_dir_all(&path).await;
        }
    }
    {
        let _guard = state.git_lock.lock().await;
        git::create_verify_worktree(&workspace.join("repo"), &branch, &path).await?;
    }
    Ok(path)
}

async fn handle_goal_questions(
    state: &AppState,
    project: &ProjectRow,
    questions: &[String],
    summary: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    let content = if summary.trim().is_empty() {
        questions
            .iter()
            .map(|q| format!("- {q}"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        format!(
            "{}\n\n{}",
            summary,
            questions
                .iter()
                .map(|q| format!("- {q}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    insert_message(&state.db, &project.id, "planner", &content).await?;
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_GOAL_CONFIRM)
    .bind(STATUS_BLOCKED)
    .bind(now())
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "goal_questions",
        "规划者提出了需要确认的问题",
    )
    .await;
    Ok(())
}

async fn handle_goal_plan(
    state: &AppState,
    project: &ProjectRow,
    summary: &str,
    tasks: &[domain::NormalizedTask],
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    let mut tx = state.db.begin().await?;
    let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE project_id = ?")
        .bind(&project.id)
        .fetch_one(&mut *tx)
        .await?;
    if existing > 0 {
        return Err(anyhow!("项目已经存在任务，拒绝重复写入规划"));
    }
    let timestamp = now();
    for (index, task) in tasks.iter().enumerate() {
        let task_id = domain::new_id("tsk");
        sqlx::query(
            "INSERT INTO tasks (id, project_id, seq, key, title, description, acceptance, deps, \
             stage, status, attempts, rejections, verifier_issues, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, '[]', ?, ?)",
        )
        .bind(&task_id)
        .bind(&project.id)
        .bind((index + 1) as i64)
        .bind(&task.key)
        .bind(&task.title)
        .bind(&task.description)
        .bind(models_list_json(&task.acceptance))
        .bind(models_list_json(&task.deps))
        .bind(TASK_STAGE_PENDING)
        .bind(STATUS_QUEUED)
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = NULL, report = NULL, report_feedback = NULL, \
         updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_EXECUTING)
    .bind(STATUS_RUNNING)
    .bind(&timestamp)
    .bind(&project.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let note = if summary.trim().is_empty() {
        format!("规划完成，共 {} 个任务", tasks.len())
    } else {
        format!("规划完成，共 {} 个任务：{}", tasks.len(), summary)
    };
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "plan_created",
        note,
    )
    .await;
    Ok(())
}

async fn handle_goal_failure(state: &AppState, project: &ProjectRow, reason: &str) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = ?, updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_GOAL_CONFIRM)
    .bind(STATUS_BLOCKED)
    .bind(reason)
    .bind(now())
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "plan_failed",
        format!("规划失败：{reason}"),
    )
    .await;
    Ok(())
}

async fn handle_executor_success(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    summary: &str,
    agent: &AgentRow,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, agent_id = ?, last_agent_id = NULL, \
         last_error = NULL, result_summary = ?, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_VERIFYING)
    .bind(STATUS_QUEUED)
    .bind(&agent.id)
    .bind(summary)
    .bind(now())
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        "task_executed",
        format!("任务 {} 执行完成，等待验收", task.key),
    )
    .await;
    Ok(())
}

async fn handle_executor_failure(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    reason: &str,
    agent: &AgentRow,
) -> Result<()> {
    apply_agent_failure(state, agent, reason).await?;
    let _guard = state.schedule_lock.lock().await;
    let attempts = task.attempts + 1;
    let blocked = attempts >= 3;
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, attempts = ?, last_agent_id = ?, \
         last_error = ?, agent_id = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_EXECUTING)
    .bind(if blocked {
        STATUS_BLOCKED
    } else {
        STATUS_QUEUED
    })
    .bind(attempts)
    .bind(&agent.id)
    .bind(reason)
    .bind(now())
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        if blocked {
            "task_blocked"
        } else {
            "task_retry"
        },
        if blocked {
            format!("任务 {} 连续失败 3 次，已阻塞：{reason}", task.key)
        } else {
            format!("任务 {} 执行失败，将更换 Agent 重试：{reason}", task.key)
        },
    )
    .await;
    Ok(())
}

async fn handle_verdict(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    pass: bool,
    reason: &str,
    issues: &[String],
    agent: &AgentRow,
) -> Result<()> {
    if pass {
        let branch = task
            .branch
            .clone()
            .ok_or_else(|| anyhow!("任务缺少执行分支"))?;
        let workspace = PathBuf::from(&project.workspace);
        let message = format!("domo(task-{}): {}", task.key, task.title);
        let merge = {
            let _guard = state.git_lock.lock().await;
            git::squash_merge(&workspace.join("repo"), &branch, &message).await
        };
        match merge {
            Ok(_) => {
                let _guard = state.schedule_lock.lock().await;
                sqlx::query(
                    "UPDATE tasks SET stage = ?, status = NULL, agent_id = ?, last_error = NULL, \
                     updated_at = ? WHERE id = ?",
                )
                .bind(TASK_STAGE_COMPLETED)
                .bind(&agent.id)
                .bind(now())
                .bind(&task.id)
                .execute(&state.db)
                .await?;
                emit(
                    &state.db,
                    &state.bus,
                    Some(&project.id),
                    Some(&task.id),
                    "task_completed",
                    format!("任务 {} 验收通过并合并到项目分支", task.key),
                )
                .await;
            }
            Err(error) => {
                let reason = format!("验收通过但合并失败：{error:#}");
                let _guard = state.schedule_lock.lock().await;
                sqlx::query(
                    "UPDATE tasks SET stage = ?, status = ?, last_error = ?, updated_at = ? WHERE id = ?",
                )
                .bind(TASK_STAGE_VERIFYING)
                .bind(STATUS_BLOCKED)
                .bind(&reason)
                .bind(now())
                .bind(&task.id)
                .execute(&state.db)
                .await?;
                emit(
                    &state.db,
                    &state.bus,
                    Some(&project.id),
                    Some(&task.id),
                    "merge_conflict",
                    format!("任务 {} 合并冲突：{reason}", task.key),
                )
                .await;
            }
        }
    } else {
        let _guard = state.schedule_lock.lock().await;
        let rejections = task.rejections + 1;
        let blocked = rejections >= 3;
        let merged_issues = merge_issues(&task.verifier_issues, issues);
        sqlx::query(
            "UPDATE tasks SET stage = ?, status = ?, rejections = ?, verifier_issues = ?, \
             last_error = ?, agent_id = ?, updated_at = ? WHERE id = ?",
        )
        .bind(TASK_STAGE_EXECUTING)
        .bind(if blocked {
            STATUS_BLOCKED
        } else {
            STATUS_QUEUED
        })
        .bind(rejections)
        .bind(&merged_issues)
        .bind(reason)
        .bind(&agent.id)
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            Some(&task.id),
            if blocked {
                "task_blocked"
            } else {
                "task_rejected"
            },
            if blocked {
                format!("任务 {} 连续 3 次验收未通过，已阻塞：{reason}", task.key)
            } else {
                format!("任务 {} 验收未通过，退回执行：{reason}", task.key)
            },
        )
        .await;
    }
    Ok(())
}

async fn handle_verifier_failure(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    reason: &str,
    agent: &AgentRow,
) -> Result<()> {
    apply_agent_failure(state, agent, reason).await?;
    let _guard = state.schedule_lock.lock().await;
    let attempts = task.attempts + 1;
    let blocked = attempts >= 3;
    sqlx::query(
        "UPDATE tasks SET attempts = ?, status = ?, last_agent_id = ?, last_error = ?, \
         agent_id = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(attempts)
    .bind(if blocked {
        STATUS_BLOCKED
    } else {
        STATUS_QUEUED
    })
    .bind(&agent.id)
    .bind(reason)
    .bind(now())
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        if blocked {
            "task_blocked"
        } else {
            "task_retry"
        },
        format!("任务 {} 验收作业失败：{reason}", task.key),
    )
    .await;
    Ok(())
}

async fn handle_report(state: &AppState, project: &ProjectRow, report: &str) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
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

async fn handle_summary_failure(
    state: &AppState,
    project: &ProjectRow,
    reason: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = ?, updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_SUMMARIZING)
    .bind(STATUS_BLOCKED)
    .bind(reason)
    .bind(now())
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "summary_failed",
        format!("总结失败：{reason}"),
    )
    .await;
    Ok(())
}

async fn finish_job(
    state: &AppState,
    job: &JobRow,
    status: &str,
    result: Option<String>,
    run: &agents::AgentRun,
    error: Option<&str>,
    _parsed: Option<ParsedResult>,
) -> Result<()> {
    sqlx::query(
        "UPDATE jobs SET status = ?, result = ?, stdout = ?, stderr = ?, error = ?, \
         exit_code = ?, finished_at = ? WHERE id = ?",
    )
    .bind(status)
    .bind(result)
    .bind(&run.stdout)
    .bind(&run.stderr)
    .bind(error)
    .bind(run.exit_code)
    .bind(now())
    .bind(&job.id)
    .execute(&state.db)
    .await?;
    if status == JOB_STATUS_SUCCEEDED {
        if let Some(agent_id) = &job.agent_id {
            let _ = sqlx::query(
                "UPDATE agents SET last_error = NULL, cooldown_until = NULL, updated_at = ? \
                 WHERE id = ? AND cooldown_until IS NULL",
            )
            .bind(now())
            .bind(agent_id)
            .execute(&state.db)
            .await;
        }
    }
    let _ = sqlx::query("UPDATE projects SET updated_at = ? WHERE id = ?")
        .bind(now())
        .bind(&job.project_id)
        .execute(&state.db)
        .await;
    emit(
        &state.db,
        &state.bus,
        Some(&job.project_id),
        job.task_id.as_deref(),
        "job_finished",
        format!(
            "{} 作业{}",
            role_label(&job.role),
            if status == JOB_STATUS_SUCCEEDED {
                "完成"
            } else {
                "失败"
            }
        ),
    )
    .await;
    Ok(())
}

async fn fail_runtime(state: &AppState, job_id: &str, reason: &str) -> Result<()> {
    let Some(job) = load_job(&state.db, job_id).await? else {
        return Ok(());
    };
    if job.status != JOB_STATUS_RUNNING {
        return Ok(());
    }
    sqlx::query("UPDATE jobs SET status = ?, error = ?, finished_at = ? WHERE id = ?")
        .bind(JOB_STATUS_FAILED)
        .bind(reason)
        .bind(now())
        .bind(job_id)
        .execute(&state.db)
        .await?;
    let project_id = job.project_id.clone();
    if let Some(project) = load_project(&state.db, &project_id).await? {
        if let Some(task_id) = &job.task_id {
            if let Some(task) = load_task(&state.db, task_id).await? {
                let agent = match &job.agent_id {
                    Some(agent_id) => crate::db::load_agent(&state.db, agent_id).await?,
                    None => None,
                };
                match job.role.as_str() {
                    JOB_ROLE_EXECUTOR => {
                        if let Some(agent) = agent {
                            handle_executor_failure(state, &project, &task, reason, &agent).await?;
                        } else {
                            block_task(state, &project, &task, reason).await?;
                        }
                    }
                    JOB_ROLE_VERIFIER => {
                        if let Some(agent) = agent {
                            handle_verifier_failure(state, &project, &task, reason, &agent).await?;
                        } else {
                            block_task(state, &project, &task, reason).await?;
                        }
                    }
                    JOB_ROLE_GOAL => handle_goal_failure(state, &project, reason).await?,
                    JOB_ROLE_SUMMARY => handle_summary_failure(state, &project, reason).await?,
                    _ => {}
                }
            }
        } else if job.role == JOB_ROLE_GOAL {
            handle_goal_failure(state, &project, reason).await?;
        } else if job.role == JOB_ROLE_SUMMARY {
            handle_summary_failure(state, &project, reason).await?;
        }
    }
    emit(
        &state.db,
        &state.bus,
        Some(&job.project_id),
        job.task_id.as_deref(),
        "job_failed",
        format!("作业基础设施失败：{reason}"),
    )
    .await;
    Ok(())
}

async fn block_task(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    reason: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    sqlx::query("UPDATE tasks SET status = ?, last_error = ?, updated_at = ? WHERE id = ?")
        .bind(STATUS_BLOCKED)
        .bind(reason)
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        "task_blocked",
        format!("任务 {} 已阻塞：{reason}", task.key),
    )
    .await;
    Ok(())
}

async fn mark_archived_job(state: &AppState, job: &JobRow) -> Result<()> {
    sqlx::query("UPDATE jobs SET status = ?, error = ?, finished_at = ? WHERE id = ?")
        .bind(JOB_STATUS_CANCELED)
        .bind("项目已归档，作业被取消")
        .bind(now())
        .bind(&job.id)
        .execute(&state.db)
        .await?;
    Ok(())
}

async fn apply_agent_failure(state: &AppState, agent: &AgentRow, reason: &str) -> Result<()> {
    let cooldown = if is_rate_limited(reason) {
        Some((chrono::Utc::now() + chrono::Duration::minutes(30)).to_rfc3339())
    } else {
        None
    };
    sqlx::query("UPDATE agents SET last_error = ?, cooldown_until = COALESCE(?, cooldown_until), updated_at = ? WHERE id = ?")
        .bind(reason)
        .bind(cooldown)
        .bind(now())
        .bind(&agent.id)
        .execute(&state.db)
        .await?;
    Ok(())
}

fn is_rate_limited(reason: &str) -> bool {
    let lowered = reason.to_lowercase();
    [
        "429",
        "rate limit",
        "quota",
        "too many requests",
        "overloaded",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

async fn insert_message(
    pool: &SqlitePool,
    project_id: &str,
    role: &str,
    content: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO messages (id, project_id, role, content, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(domain::new_id("msg"))
    .bind(project_id)
    .bind(role)
    .bind(content)
    .bind(now())
    .execute(pool)
    .await?;
    Ok(())
}

fn merge_issues(existing: &str, issues: &[String]) -> String {
    let mut merged: Vec<String> = parse_json_list(existing);
    for issue in issues {
        if !merged.iter().any(|item| item == issue) {
            merged.push(issue.clone());
        }
    }
    while merged.len() > 20 {
        merged.remove(0);
    }
    serde_json::to_string(&merged).unwrap_or_else(|_| "[]".into())
}

fn parse_json_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn models_list_json(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".into())
}

fn parsed_error(parsed: &ParsedResult) -> Option<String> {
    match parsed {
        ParsedResult::ExecutorFailed { reason } => Some(reason.clone()),
        _ => None,
    }
}

fn role_label(role: &str) -> &'static str {
    match role {
        JOB_ROLE_GOAL => "规划",
        JOB_ROLE_EXECUTOR => "执行",
        JOB_ROLE_VERIFIER => "验收",
        JOB_ROLE_SUMMARY => "总结",
        _ => "未知",
    }
}

fn first_lines(text: &str, count: usize) -> String {
    text.lines()
        .take(count)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

async fn load_job(pool: &SqlitePool, id: &str) -> Result<Option<JobRow>> {
    Ok(
        sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

async fn load_project(pool: &SqlitePool, id: &str) -> Result<Option<ProjectRow>> {
    Ok(
        sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

async fn load_task(pool: &SqlitePool, id: &str) -> Result<Option<TaskRow>> {
    Ok(
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

#[cfg(test)]
mod tests {
    use super::is_rate_limited;

    #[test]
    fn detects_rate_limit_failures() {
        assert!(is_rate_limited("HTTP 429 Too Many Requests"));
        assert!(is_rate_limited("rate limit exceeded"));
        assert!(is_rate_limited("quota exhausted"));
        assert!(is_rate_limited("provider overloaded"));
        assert!(!is_rate_limited("tests failed"));
    }
}
