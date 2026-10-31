//! Job lifecycle. Long-running agent processes never hold the scheduler lock;
//! state transitions are applied atomically after the process exits.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::json;
use sqlx::SqlitePool;
use tokio::fs;

use crate::agents::{self, AgentSpec};
use crate::domain::{self, now, parse_result, short_id, slug, FailureKind, ParsedResult};
use crate::events::{emit, emit_job};
use crate::git;
use crate::models::*;
use crate::paths;
use crate::prompts::{
    self, DependencyInfo, DerivedTaskInfo, PromptData, SummaryTaskInfo, TaskInfo,
};
use crate::replan;
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

    let job_dir = paths::job_dir(&PathBuf::from(&project.workspace), &job.id);
    fs::create_dir_all(&job_dir).await?;
    state.register_job(job_dir.clone());
    let result_path = job_dir.join("result.json");
    let _ = fs::remove_file(&result_path).await;

    let workspace = PathBuf::from(&project.workspace);
    let cwd = match job.role.as_str() {
        JOB_ROLE_ANALYSIS | JOB_ROLE_DIRECT | JOB_ROLE_SUMMARY => paths::project_repo(&workspace),
        JOB_ROLE_EXECUTOR => prepare_task_worktree(state, &project, task.as_ref()).await?,
        JOB_ROLE_VERIFIER => {
            prepare_verify_worktree(state, &project, task.as_ref(), &job.id).await?
        }
        other => return Err(anyhow!("未知作业角色: {other}")),
    };
    ensure_worktree_root(&cwd).await?;
    if job.role == JOB_ROLE_DIRECT {
        let _guard = state.git_lock.lock().await;
        git::reset_hard(&cwd).await?;
    }

    let prompt_data = build_prompt_data(state, &project, task.as_ref(), &job, &cwd).await?;
    let prompt = prompts::render(&prompt_data);
    fs::write(job_dir.join("prompt.md"), &prompt).await?;
    let context = build_context(
        &project,
        task.as_ref(),
        &job,
        &cwd,
        &job_dir,
        &prompt_data,
        &agent,
    );
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
    emit_job(
        &state.db,
        &state.bus,
        Some(&project.id),
        job.task_id.as_deref(),
        Some(&job.id),
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
    if current.status.as_deref() == Some(STATUS_PAUSED) {
        mark_canceled_job(state, &job, "项目已暂停，作业未启动").await?;
        return Ok(());
    }
    if let Some(task_id) = &job.task_id {
        if let Some(current_task) = load_task(&state.db, task_id).await? {
            if current_task.status.as_deref() == Some(STATUS_PAUSED) {
                mark_canceled_job(state, &job, "任务已暂停，作业未启动").await?;
                return Ok(());
            }
        }
    }

    let progress = agents::JobProgress::new();
    let spec = AgentSpec {
        job_id: job.id.clone(),
        role: job.role.clone(),
        cwd: cwd.clone(),
        job_dir: job_dir.clone(),
        prompt: prompt.clone(),
        opencode_session_id: crate::opencode::session_id(&job.id),
        progress: progress.clone(),
    };
    let mut run = run_agent_job(state, &job, &job_dir, &agent, &spec, &progress).await?;
    if agent.kind == AGENT_OPENCODE && !state.is_shutting_down() {
        if run.process_failure() {
            // The server-side session can outlive the killed CLI process. Stop it
            // before taking the settlement snapshot so no further usage accrues.
            crate::opencode::settle(&spec.opencode_session_id).await;
        }
        run.usage = crate::opencode::fetch_usage(&spec.opencode_session_id).await;
    }
    state.unregister_job(&job_dir);

    let raw_result = fs::read_to_string(&result_path).await.ok();
    if job.role == JOB_ROLE_VERIFIER {
        // The verifier workspace is disposable and must not leak into later work.
        let _guard = state.git_lock.lock().await;
        let _ = git::remove_worktree(&paths::project_repo(&workspace), &cwd).await;
    }
    if matches!(job.role.as_str(), JOB_ROLE_ANALYSIS | JOB_ROLE_SUMMARY) {
        let _guard = state.git_lock.lock().await;
        let _ = git::reset_hard(&paths::project_repo(&workspace)).await;
    }

    let fresh_project = load_project(&state.db, &project.id)
        .await?
        .ok_or_else(|| anyhow!("项目不存在"))?;
    if fresh_project.status.as_deref() == Some(STATUS_ARCHIVED) {
        mark_archived_job(state, &job).await?;
        return Ok(());
    }
    let Some(current_job) = load_job(&state.db, &job.id).await? else {
        return Ok(());
    };
    if current_job.status != JOB_STATUS_RUNNING {
        return Ok(());
    }
    if let Some(task_id) = &job.task_id {
        if let Some(current_task) = load_task(&state.db, task_id).await? {
            if current_task.status.as_deref() == Some(STATUS_CANCELED) {
                return Ok(());
            }
        }
    }

    let process_ok = run.exit_code == Some(0) && !run.timed_out && !run.stalled;
    let (mut status, result_text, mut error) = if !process_ok {
        let reason = if run.timed_out {
            "Agent 作业超时".to_string()
        } else if run.stalled {
            format!(
                "Agent 作业卡死：连续 {} 秒无输出或进度，已终止进程",
                state.cfg.stall_timeout.as_secs()
            )
        } else {
            let detail = format!("{}\n{}", run.stderr, run.stdout);
            format!(
                "Agent 进程退出码 {:?}: {}",
                run.exit_code,
                crate::failure::error_detail(&detail, 30)
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
                Some(match job.role.as_str() {
                    JOB_ROLE_ANALYSIS | JOB_ROLE_DIRECT | JOB_ROLE_SUMMARY => parse_error,
                    _ => format!("结果协议不合法: {parse_error}"),
                }),
            ),
        }
    } else {
        (
            JOB_STATUS_FAILED,
            None,
            Some("Agent 没有写 result.json".to_string()),
        )
    };

    let mut parsed = if status == JOB_STATUS_SUCCEEDED {
        result_text
            .as_deref()
            .and_then(|raw| parse_result(&job.role, raw).ok())
    } else {
        None
    };
    if job.role == JOB_ROLE_ANALYSIS {
        // A failed analysis is settled only after the failure handling decides
        // whether to switch agents; otherwise the job stays running and the
        // scheduler would dispatch a parallel analysis for the same project.
        if status != JOB_STATUS_SUCCEEDED {
            let reason = error.as_deref().unwrap_or("项目分析失败");
            let transition = handle_analysis_failure(
                state,
                &project,
                reason,
                Some(&agent),
                run.process_failure(),
            )
            .await;
            let finished = finish_job(
                state,
                &job,
                status,
                result_text,
                &run,
                error.as_deref(),
                parsed.clone(),
            )
            .await?;
            if finished {
                transition?;
            }
            return Ok(());
        }
        if parsed.is_none() {
            let reason = error.as_deref().unwrap_or("项目分析失败");
            let transition = handle_analysis_failure(
                state,
                &project,
                reason,
                Some(&agent),
                run.process_failure(),
            )
            .await;
            let finished = finish_job(
                state,
                &job,
                JOB_STATUS_FAILED,
                result_text,
                &run,
                Some(reason),
                None,
            )
            .await?;
            if finished {
                transition?;
            }
            return Ok(());
        }
    }
    if status == JOB_STATUS_SUCCEEDED {
        if let Some(ParsedResult::ExecutorDerive { tasks, .. }) = parsed.as_ref() {
            if let Some(reason) =
                validate_derive_request(state, &project, task.as_ref(), tasks).await?
            {
                status = JOB_STATUS_FAILED;
                error = Some(reason);
                parsed = None;
            }
        }
    }

    if job.role == JOB_ROLE_DIRECT && status == JOB_STATUS_SUCCEEDED {
        if let Some(ParsedResult::DirectDone { summary, report }) = parsed.clone() {
            let message = format!("domo(direct): {} ({})", project.name, short_id(&job.id));
            let commit = {
                let _guard = state.git_lock.lock().await;
                git::commit_all(&cwd, &message).await
            };
            if let Err(commit_error) = commit {
                let error_text = format!("提交直接完成结果失败: {commit_error:#}");
                let transition =
                    handle_direct_failure(state, &project, &error_text, None, false).await;
                let finished = finish_job(
                    state,
                    &job,
                    JOB_STATUS_FAILED,
                    result_text,
                    &run,
                    Some(&error_text),
                    None,
                )
                .await?;
                if finished {
                    transition?;
                }
                return Ok(());
            }
            let transition = handle_direct_success(state, &project, &summary, &report).await;
            let finished = finish_job(
                state,
                &job,
                JOB_STATUS_SUCCEEDED,
                result_text,
                &run,
                None,
                None,
            )
            .await?;
            if finished {
                transition?;
            }
            return Ok(());
        }
    }

    if job.role == JOB_ROLE_EXECUTOR && status == JOB_STATUS_SUCCEEDED {
        match parsed.clone() {
            Some(ParsedResult::ExecutorReplan { reason }) => {
                let finished = finish_job(
                    state,
                    &job,
                    JOB_STATUS_SUCCEEDED,
                    result_text,
                    &run,
                    None,
                    None,
                )
                .await?;
                if finished {
                    replan::request(state, &project.id, &reason, REPLAN_SOURCE_EXECUTOR).await?;
                }
                return Ok(());
            }
            Some(ParsedResult::ExecutorDerive { summary, tasks }) => {
                let Some(task) = task.as_ref() else {
                    return Err(anyhow!("派生命中缺少任务"));
                };
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
                        handle_executor_failure(state, &project, task, &error_text, &agent, false)
                            .await;
                    let finished = finish_job(
                        state,
                        &job,
                        JOB_STATUS_FAILED,
                        result_text,
                        &run,
                        Some(&error_text),
                        None,
                    )
                    .await?;
                    if finished {
                        transition?;
                    }
                    return Ok(());
                }
                let transition =
                    handle_executor_derive(state, &project, task, &summary, &tasks, &agent).await;
                let finished = finish_job(
                    state,
                    &job,
                    JOB_STATUS_SUCCEEDED,
                    result_text,
                    &run,
                    None,
                    None,
                )
                .await?;
                if finished {
                    transition?;
                }
                return Ok(());
            }
            Some(ParsedResult::ExecutorDone { summary }) => {
                let Some(task) = task.as_ref() else {
                    return Ok(());
                };
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
                        handle_executor_failure(state, &project, task, &error_text, &agent, false)
                            .await;
                    let finished = finish_job(
                        state,
                        &job,
                        JOB_STATUS_FAILED,
                        result_text,
                        &run,
                        Some(&error_text),
                        None,
                    )
                    .await?;
                    if finished {
                        transition?;
                    }
                    return Ok(());
                }
                let transition = if task.parent_id.is_some() {
                    handle_derived_task_success(state, &project, task, &summary, &agent).await
                } else {
                    handle_executor_success(state, &project, task, &summary, &agent).await
                };
                let finished = finish_job(
                    state,
                    &job,
                    JOB_STATUS_SUCCEEDED,
                    result_text,
                    &run,
                    None,
                    None,
                )
                .await?;
                if finished {
                    transition?;
                }
                return Ok(());
            }
            _ => {}
        }
    }

    let transition: Result<()> = match (job.role.as_str(), &parsed) {
        (JOB_ROLE_ANALYSIS, Some(ParsedResult::Questions { questions, summary })) => {
            handle_analysis_questions(state, &project, questions, summary).await
        }
        (
            JOB_ROLE_ANALYSIS,
            Some(ParsedResult::AnalysisPlan {
                summary,
                goal,
                assumptions,
                acceptance,
                tasks,
                skip_confirmation,
            }),
        ) => {
            handle_analysis_plan(
                state,
                &project,
                summary,
                goal.as_deref(),
                assumptions,
                acceptance,
                tasks,
                *skip_confirmation,
            )
            .await
        }
        (
            JOB_ROLE_ANALYSIS,
            Some(ParsedResult::AnalysisDirect {
                summary,
                goal,
                plan,
                verification,
                skip_confirmation,
            }),
        ) => {
            handle_analysis_direct(
                state,
                &project,
                summary,
                goal.as_deref(),
                plan,
                verification,
                *skip_confirmation,
            )
            .await
        }
        (JOB_ROLE_ANALYSIS, _) => {
            let process_failure = run.process_failure();
            handle_analysis_failure(
                state,
                &project,
                error.as_deref().unwrap_or("项目分析失败"),
                Some(&agent),
                process_failure,
            )
            .await
        }
        (JOB_ROLE_DIRECT, _) => {
            let process_failure = run.process_failure();
            handle_direct_failure(
                state,
                &project,
                error.as_deref().unwrap_or("直接完成失败"),
                Some(&agent),
                process_failure,
            )
            .await
        }
        (JOB_ROLE_EXECUTOR, Some(ParsedResult::ExecutorDone { summary })) => {
            if let Some(task) = task.as_ref() {
                handle_executor_success(state, &project, task, summary, &agent).await
            } else {
                Ok(())
            }
        }
        (JOB_ROLE_EXECUTOR, Some(ParsedResult::ExecutorReplan { .. })) => Ok(()),
        (JOB_ROLE_EXECUTOR, _) => {
            if let Some(task) = task.as_ref() {
                let reason = error
                    .as_deref()
                    .or(match &parsed {
                        Some(ParsedResult::ExecutorFailed { reason }) => Some(reason.as_str()),
                        _ => None,
                    })
                    .unwrap_or("执行失败");
                let process_failure = run.process_failure();
                handle_executor_failure(state, &project, task, reason, &agent, process_failure)
                    .await
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
                let process_failure = run.process_failure();
                handle_verifier_failure(state, &project, task, reason, &agent, process_failure)
                    .await
            } else {
                Ok(())
            }
        }
        (JOB_ROLE_SUMMARY, Some(ParsedResult::Report { summary })) => {
            handle_report(state, &project, summary).await
        }
        (JOB_ROLE_SUMMARY, _) => {
            let process_failure = run.process_failure();
            handle_summary_failure(
                state,
                &project,
                error.as_deref().unwrap_or("总结失败"),
                Some(&agent),
                process_failure,
            )
            .await
        }
        _ => Ok(()),
    };
    let finished = finish_job(
        state,
        &job,
        status,
        result_text,
        &run,
        error.as_deref(),
        parsed.clone(),
    )
    .await?;
    if finished {
        transition?;
    }
    Ok(())
}

/// Runs one agent process under two watchdogs. The job deadline bounds the whole
/// run; the stall watchdog ends a run whose stdout/stderr stopped growing for
/// `stall_timeout`. Both kill only the recorded process group, leaving other
/// jobs of the same agent untouched, and let the normal failure path apply.
async fn run_agent_job(
    state: &AppState,
    job: &JobRow,
    job_dir: &Path,
    agent: &AgentRow,
    spec: &AgentSpec,
    progress: &agents::JobProgress,
) -> Result<agents::AgentRun> {
    let execution = agents::execute(agent, spec, state.cfg.job_timeout);
    tokio::pin!(execution);
    let deadline = tokio::time::sleep(state.cfg.job_timeout);
    tokio::pin!(deadline);
    let stall_watch = wait_for_stall(progress, state.cfg.stall_timeout);
    tokio::pin!(stall_watch);

    let mut stalled: Option<String> = None;
    let mut stall_kill_attempted = false;
    let mut timed_out = false;
    let mut run: agents::AgentRun;
    loop {
        tokio::select! {
            biased;
            result = &mut execution => {
                run = result?;
                break;
            }
            _ = &mut deadline, if !timed_out => {
                timed_out = true;
                agents::kill_job_dir(job_dir);
            }
            _ = &mut stall_watch, if stalled.is_none() && !timed_out => {
                let (stdout_bytes, stderr_bytes) = progress.output_bytes();
                let idle_secs = progress.idle_for().as_secs();
                let reason = format!(
                    "Agent 作业卡死：连续 {idle_secs} 秒无 stdout/stderr 输出或进度\
                     （stdout {stdout_bytes} 字节，stderr {stderr_bytes} 字节），已终止进程"
                );
                emit_job(
                    &state.db,
                    &state.bus,
                    Some(&job.project_id),
                    job.task_id.as_deref(),
                    Some(&job.id),
                    "job_stalled",
                    format!("{}作业无响应，已判定卡死：{reason}", role_label(&job.role)),
                )
                .await;
                stalled = Some(reason);
                stall_kill_attempted = agents::kill_job_dir(job_dir);
            }
        }
    }

    if timed_out {
        run.timed_out = true;
        run.stdout.clear();
        run.stderr = format!(
            "作业超时（{} 秒），进程已终止",
            state.cfg.job_timeout.as_secs()
        );
    }
    if let Some(reason) = stalled {
        // Windows reports an exit code for a process killed by `taskkill`, so
        // the signal check alone cannot distinguish a watchdog kill from a
        // process that exited on its own. A verified kill attempt does.
        if run.exit_code.is_none() || stall_kill_attempted {
            run.stalled = true;
            if !run.stderr.is_empty() {
                run.stderr.push('\n');
            }
            run.stderr.push_str(&reason);
        } else {
            // The process exited on its own before the kill took effect.
            emit_job(
                &state.db,
                &state.bus,
                Some(&job.project_id),
                job.task_id.as_deref(),
                Some(&job.id),
                "job_stall_recovered",
                format!(
                    "{}作业在卡死判定生效前已自行退出，按实际退出状态处理",
                    role_label(&job.role)
                ),
            )
            .await;
        }
    }
    Ok(run)
}

/// Resolves once no output has been observed for `threshold`. A zero threshold
/// disables stall detection by never resolving.
async fn wait_for_stall(progress: &agents::JobProgress, threshold: std::time::Duration) {
    if threshold.is_zero() {
        std::future::pending::<()>().await;
    }
    loop {
        let idle = progress.idle_for();
        if idle >= threshold {
            return;
        }
        tokio::time::sleep(threshold - idle).await;
    }
}

async fn build_prompt_data(
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
        task_info = Some(TaskInfo {
            key: task.key.clone(),
            title: task.title.clone(),
            description: task.description.clone(),
            acceptance: parse_json_list(&task.acceptance),
            guidance: task.guidance.clone(),
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
        // 报告评议记录只属于总结作业和直接重做反馈。
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
    for row in &rows {
        if row.status.as_deref() == Some(STATUS_CANCELED) {
            canceled_tasks.push(SummaryTaskInfo {
                key: row.key.clone(),
                title: row.title.clone(),
                summary: row.last_error.clone().unwrap_or_default(),
            });
            continue;
        }
        let info = SummaryTaskInfo {
            key: row.key.clone(),
            title: row.title.clone(),
            summary: row.result_summary.clone().unwrap_or_default(),
        };
        if row.stage == TASK_STAGE_COMPLETED {
            completed_tasks.push(info.clone());
        }
        summary_tasks.push(info);
    }
    let workspace = PathBuf::from(&project.workspace);
    let diff_stat = match job.role.as_str() {
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
        JOB_ROLE_SUMMARY => git::diff_between(
            &paths::project_repo(&workspace),
            &project.base_branch,
            &project.branch,
        )
        .await
        .unwrap_or_default(),
        _ => String::new(),
    };
    let job_dir = paths::job_dir(&workspace, &job.id);
    Ok(PromptData {
        role: job.role.clone(),
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        goal: project.goal.clone(),
        cwd: cwd.to_string_lossy().to_string(),
        repo_path: project.repo_path.clone(),
        base_branch: project.base_branch.clone(),
        project_branch: project.branch.clone(),
        task: task_info,
        dependencies,
        derived_from,
        derived_tasks,
        messages,
        verifier_issues,
        summary_tasks,
        completed_tasks,
        canceled_tasks,
        diff_stat,
        job_dir: job_dir.to_string_lossy().to_string(),
        result_path: job_dir.join("result.json").to_string_lossy().to_string(),
        attempt: job.attempt,
        report_feedback: project.report_feedback.clone(),
        replan_reason: project.replan_reason.clone(),
        replan_source: project.replan_source.clone(),
        confirm_mode: project.confirm_mode.clone(),
    })
}

fn build_context(
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
        "project": {
            "id": project.id,
            "name": project.name,
            "goal": project.goal,
            "repo_path": project.repo_path,
            "base_branch": project.base_branch,
            "branch": project.branch,
            "replan_reason": project.replan_reason,
            "replan_source": project.replan_source,
            "confirm_mode": project.confirm_mode,
            "completion_mode": project.completion_mode,
            "confirmed_at": project.confirmed_at,
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
            "guidance": task.guidance,
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
    })
}

/// Every job runs in a dedicated Git worktree root. A mismatch means the agent
/// would read or write another checkout, so the job is rejected before spawn.
async fn ensure_worktree_root(cwd: &Path) -> Result<()> {
    let expected = fs::canonicalize(cwd).await?;
    let root = fs::canonicalize(git::repo_root(cwd).await?).await?;
    if root != expected {
        return Err(anyhow!(
            "工作目录 {} 不是独立 Git 工作树根目录（实际根目录 {}）",
            expected.display(),
            root.display()
        ));
    }
    Ok(())
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
    let repo = paths::project_repo(&workspace);
    let path = paths::task_worktree(&workspace, task.seq);
    let branch = format!(
        "domo-task/{}/{}-{}",
        short_id(&project.id),
        task.seq,
        slug(&task.key)
    );
    let base_branch = if let Some(parent_id) = &task.parent_id {
        let parent = load_task(&state.db, parent_id)
            .await?
            .ok_or_else(|| anyhow!("派生子任务的父任务不存在"))?;
        parent
            .branch
            .clone()
            .ok_or_else(|| anyhow!("父任务还没有执行分支，无法派生子任务"))?
    } else {
        project.branch.clone()
    };
    {
        let _guard = state.git_lock.lock().await;
        if path.exists() {
            let _ = git::remove_worktree(&repo, &path).await;
            if path.exists() {
                let _ = tokio::fs::remove_dir_all(&path).await;
            }
        }
        // A crash can leave the worktree registered while its directory is
        // gone. Prune that metadata before adding the path again.
        git::prune_worktrees(&repo).await?;
        git::create_task_worktree(&repo, &base_branch, &path, &branch).await?;
    }
    sqlx::query(
        "UPDATE tasks SET worktree = ?, branch = ?, artifacts_released_at = NULL, updated_at = ? \
         WHERE id = ?",
    )
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
    let repo = paths::project_repo(&workspace);
    let path = paths::verify_worktree(&workspace, task.seq, job_id);
    {
        let _guard = state.git_lock.lock().await;
        if path.exists() {
            let _ = git::remove_worktree(&repo, &path).await;
            if path.exists() {
                let _ = tokio::fs::remove_dir_all(&path).await;
            }
        }
        git::prune_worktrees(&repo).await?;
        git::create_verify_worktree(&repo, &branch, &path).await?;
    }
    Ok(path)
}

async fn handle_analysis_questions(
    state: &AppState,
    project: &ProjectRow,
    questions: &[String],
    summary: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    if has_active_tasks(&state.db, &project.id).await? {
        return apply_stale_result(
            state,
            project,
            "plan_ignored",
            "项目已有活动任务，忽略规划者提问，回到执行阶段".into(),
        )
        .await;
    }
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
         THEN status ELSE ? END, blocked_reason = NULL, proposal = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_ANALYSIS)
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
        "analysis_questions",
        "规划者提出了需要确认的问题",
    )
    .await;
    Ok(())
}

pub(crate) async fn apply_analysis_plan(
    state: &AppState,
    project: &ProjectRow,
    goal: &str,
    tasks: &[domain::NormalizedTask],
    summary: &str,
    proposal_json: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let existing: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage != ? \
         AND COALESCE(status, '') != ?",
    )
    .bind(&project.id)
    .bind(TASK_STAGE_COMPLETED)
    .bind(STATUS_CANCELED)
    .fetch_one(&mut *tx)
    .await?;
    if existing > 0 {
        tx.rollback().await?;
        apply_stale_result(
            state,
            project,
            "plan_ignored",
            "项目已有活动任务，忽略规划结果，回到执行阶段".into(),
        )
        .await?;
        return Ok(());
    }
    let max_seq: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM tasks WHERE project_id = ?")
            .bind(&project.id)
            .fetch_one(&mut *tx)
            .await?;
    let max_queue: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(queue_order), 0) FROM tasks WHERE project_id = ?")
            .bind(&project.id)
            .fetch_one(&mut *tx)
            .await?;
    let timestamp = now();
    for (index, task) in tasks.iter().enumerate() {
        let task_id = domain::new_id("tsk");
        sqlx::query(
            "INSERT INTO tasks (id, project_id, seq, queue_order, key, title, description, \
             acceptance, deps, stage, status, attempts, rejections, verifier_issues, created_at, \
             updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, '[]', ?, ?)",
        )
        .bind(&task_id)
        .bind(&project.id)
        .bind(max_seq + (index + 1) as i64)
        .bind(max_queue + (index + 1) as i64)
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
        crate::db::ensure_task_agents_tx(&mut tx, &task_id).await?;
    }
    sqlx::query(
        "UPDATE projects SET goal = ?, proposal = ?, stage = ?, \
         status = CASE WHEN status IN ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
         blocked_reason = NULL, report = NULL, report_feedback = NULL, completion_mode = ?, \
         confirmed_at = COALESCE(confirmed_at, ?), updated_at = ? WHERE id = ?",
    )
    .bind(goal)
    .bind(proposal_json)
    .bind(STAGE_EXECUTING)
    .bind(STATUS_RUNNING)
    .bind(COMPLETION_TASKS)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(&project.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let note = if summary.trim().is_empty() {
        format!("项目分析完成，共 {} 个任务", tasks.len())
    } else {
        format!("项目分析完成，共 {} 个任务：{}", tasks.len(), summary)
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

fn analysis_proposal_content(
    summary: &str,
    goal: Option<&str>,
    assumptions: &[String],
    acceptance: &[String],
    tasks: &[domain::NormalizedTask],
    plan: &[String],
    verification: &[String],
) -> String {
    let mut content = summary.trim().to_string();
    if let Some(goal) = goal.map(str::trim).filter(|value| !value.is_empty()) {
        content.push_str(&format!("\n\n目标：{goal}"));
    }
    let push_list = |content: &mut String, title: &str, items: &[String]| {
        if items.is_empty() {
            return;
        }
        content.push_str(&format!("\n\n{title}："));
        for item in items {
            content.push_str(&format!("\n- {item}"));
        }
    };
    push_list(&mut content, "关键假设", assumptions);
    push_list(&mut content, "验收口径", acceptance);
    if !tasks.is_empty() {
        content.push_str("\n\n任务：");
        for task in tasks {
            content.push_str(&format!(
                "\n- {} {}（依赖：{}）",
                task.key,
                task.title,
                if task.deps.is_empty() {
                    "无".to_string()
                } else {
                    task.deps.join("、")
                }
            ));
            for item in &task.acceptance {
                content.push_str(&format!("\n  - 验收：{item}"));
            }
        }
    }
    push_list(&mut content, "执行步骤", plan);
    push_list(&mut content, "自检方式", verification);
    content
}

#[allow(clippy::too_many_arguments)]
async fn handle_analysis_plan(
    state: &AppState,
    project: &ProjectRow,
    summary: &str,
    goal: Option<&str>,
    assumptions: &[String],
    acceptance: &[String],
    tasks: &[domain::NormalizedTask],
    skip_confirmation: bool,
) -> Result<()> {
    let goal = goal
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(project.goal.trim())
        .to_string();
    let proposal = AnalysisProposal {
        kind: "plan".into(),
        summary: summary.to_string(),
        goal: Some(goal.clone()),
        assumptions: assumptions.to_vec(),
        acceptance: acceptance.to_vec(),
        tasks: tasks
            .iter()
            .map(|task| ProposalTask {
                key: task.key.clone(),
                title: task.title.clone(),
                description: task.description.clone(),
                acceptance: task.acceptance.clone(),
                deps: task.deps.clone(),
            })
            .collect(),
        plan: Vec::new(),
        verification: Vec::new(),
        skip_confirmation,
    };
    let proposal_json = serde_json::to_string(&proposal)?;
    let auto = project.confirmed_at.is_some()
        || (skip_confirmation && project.confirm_mode == CONFIRM_AUTO);
    if auto {
        return apply_analysis_plan(state, project, &goal, tasks, summary, &proposal_json).await;
    }
    let _guard = state.schedule_lock.lock().await;
    let content = analysis_proposal_content(
        summary,
        Some(&goal),
        assumptions,
        acceptance,
        tasks,
        &[],
        &[],
    );
    insert_message(&state.db, &project.id, "planner", &content).await?;
    sqlx::query(
        "UPDATE projects SET goal = ?, proposal = ?, stage = ?, \
         status = CASE WHEN status IN ('pausing', 'paused', 'archived') THEN status ELSE ? END, \
         completion_mode = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(&goal)
    .bind(&proposal_json)
    .bind(STAGE_ANALYSIS)
    .bind(STATUS_BLOCKED)
    .bind(COMPLETION_TASKS)
    .bind(now())
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        "analysis_ready",
        "规划者已给出项目方案，等待人工确认",
    )
    .await;
    Ok(())
}

async fn handle_analysis_direct(
    state: &AppState,
    project: &ProjectRow,
    summary: &str,
    goal: Option<&str>,
    plan: &[String],
    verification: &[String],
    skip_confirmation: bool,
) -> Result<()> {
    let goal = goal
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(project.goal.trim())
        .to_string();
    let proposal = AnalysisProposal {
        kind: "direct".into(),
        summary: summary.to_string(),
        goal: Some(goal.clone()),
        assumptions: Vec::new(),
        acceptance: Vec::new(),
        tasks: Vec::new(),
        plan: plan.to_vec(),
        verification: verification.to_vec(),
        skip_confirmation,
    };
    let proposal_json = serde_json::to_string(&proposal)?;
    let auto = project.confirmed_at.is_some()
        || (skip_confirmation && project.confirm_mode == CONFIRM_AUTO);
    let _guard = state.schedule_lock.lock().await;
    let content =
        analysis_proposal_content(summary, Some(&goal), &[], &[], &[], plan, verification);
    insert_message(&state.db, &project.id, "planner", &content).await?;
    if auto {
        sqlx::query(
            "UPDATE projects SET goal = ?, proposal = ?, confirmed_at = ?, stage = ?, \
             status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
             THEN status ELSE ? END, completion_mode = ?, blocked_reason = NULL, updated_at = ? \
             WHERE id = ?",
        )
        .bind(&goal)
        .bind(&proposal_json)
        .bind(now())
        .bind(STAGE_EXECUTING)
        .bind(STATUS_RUNNING)
        .bind(COMPLETION_DIRECT)
        .bind(now())
        .bind(&project.id)
        .execute(&state.db)
        .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            None,
            "analysis_auto_confirmed",
            "方案已由自动确认通过，开始直接完成",
        )
        .await;
    } else {
        sqlx::query(
            "UPDATE projects SET goal = ?, proposal = ?, stage = ?, \
             status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
             THEN status ELSE ? END, completion_mode = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(&goal)
        .bind(&proposal_json)
        .bind(STAGE_ANALYSIS)
        .bind(STATUS_BLOCKED)
        .bind(COMPLETION_TASKS)
        .bind(now())
        .bind(&project.id)
        .execute(&state.db)
        .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            None,
            "analysis_direct",
            "规划者建议直接完成，等待人工确认",
        )
        .await;
    }
    Ok(())
}

async fn handle_direct_success(
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

async fn handle_direct_failure(
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

async fn handle_analysis_failure(
    state: &AppState,
    project: &ProjectRow,
    reason: &str,
    agent: Option<&AgentRow>,
    process_failure: bool,
) -> Result<()> {
    let failure = plan_project_failure(
        state,
        project,
        reason,
        agent.map(|agent| (agent, process_failure)),
    )
    .await?;
    let _guard = state.schedule_lock.lock().await;
    if has_active_tasks(&state.db, &project.id).await? {
        return apply_stale_result(
            state,
            project,
            "plan_failed",
            format!("项目分析失败：{reason}（项目已有活动任务，保持在执行阶段）"),
        )
        .await;
    }
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = ?, proposal = NULL, \
         avoid_agent_id = CASE WHEN ? THEN ? ELSE avoid_agent_id END, \
         avoid_agent_role = CASE WHEN ? THEN ? ELSE avoid_agent_role END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_ANALYSIS)
    .bind(failure.status())
    .bind(&failure.blocked_reason)
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
        failure.event_kind("analysis_failed"),
        format!("项目分析{}：{reason}", failure.label()),
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
    let pause = task_pause_intent(state, &task.id).await?;
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, agent_id = ?, last_agent_id = NULL, \
         last_error = NULL, waiting_reason = NULL, result_summary = ?, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_VERIFYING)
    .bind(pause.unwrap_or(STATUS_QUEUED))
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
        format!(
            "任务 {} 执行完成，{}",
            task.key,
            if pause.is_some() {
                "任务已暂停，等待恢复后验收"
            } else {
                "等待验收"
            }
        ),
    )
    .await;
    Ok(())
}

async fn validate_derive_request(
    state: &AppState,
    project: &ProjectRow,
    task: Option<&TaskRow>,
    tasks: &[domain::NormalizedTask],
) -> Result<Option<String>> {
    let Some(task) = task else {
        return Ok(Some("执行作业缺少任务，无法派生".into()));
    };
    let active_keys: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM tasks WHERE project_id = ? AND COALESCE(status, '') != ?",
    )
    .bind(&project.id)
    .bind(STATUS_CANCELED)
    .fetch_all(&state.db)
    .await?;
    let active_keys: HashSet<String> = active_keys.into_iter().collect();
    Ok(
        domain::validate_derive_scope(task.parent_id.is_some(), &active_keys, tasks)
            .err()
            .map(|reason| format!("派生请求不合法: {reason}")),
    )
}

async fn handle_executor_derive(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    summary: &str,
    tasks: &[domain::NormalizedTask],
    agent: &AgentRow,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    let Some(parent) = load_task(&state.db, &task.id).await? else {
        return Ok(());
    };
    if parent.stage == TASK_STAGE_COMPLETED || parent.status.as_deref() == Some(STATUS_CANCELED) {
        return Ok(());
    }
    if parent.parent_id.is_some() {
        return Err(anyhow!("派生任务不能再次派生"));
    }
    let pause = task_pause_intent(state, &parent.id).await?;

    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let active_keys: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM tasks WHERE project_id = ? AND COALESCE(status, '') != ?",
    )
    .bind(&project.id)
    .bind(STATUS_CANCELED)
    .fetch_all(&mut *tx)
    .await?;
    let active_keys: HashSet<String> = active_keys.into_iter().collect();
    domain::validate_derive_scope(false, &active_keys, tasks)
        .map_err(|reason| anyhow!("{reason}"))?;
    let max_seq: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM tasks WHERE project_id = ?")
            .bind(&project.id)
            .fetch_one(&mut *tx)
            .await?;
    let max_queue: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(queue_order), 0) FROM tasks WHERE project_id = ?")
            .bind(&project.id)
            .fetch_one(&mut *tx)
            .await?;
    let timestamp = now();
    let mut child_keys = Vec::new();
    for (index, child) in tasks.iter().enumerate() {
        let task_id = domain::new_id("tsk");
        sqlx::query(
            "INSERT INTO tasks (id, project_id, seq, queue_order, key, title, description, \
             acceptance, deps, parent_id, stage, status, attempts, rejections, verifier_issues, \
             created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, '[]', ?, ?)",
        )
        .bind(&task_id)
        .bind(&project.id)
        .bind(max_seq + (index + 1) as i64)
        .bind(max_queue + (index + 1) as i64)
        .bind(&child.key)
        .bind(&child.title)
        .bind(&child.description)
        .bind(models_list_json(&child.acceptance))
        .bind(models_list_json(&child.deps))
        .bind(&parent.id)
        .bind(TASK_STAGE_PENDING)
        .bind(STATUS_QUEUED)
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&mut *tx)
        .await?;
        crate::db::ensure_task_agents_tx(&mut tx, &task_id).await?;
        child_keys.push(child.key.clone());
    }
    let mut deps = parse_json_list(&parent.deps);
    for key in &child_keys {
        if !deps.contains(key) {
            deps.push(key.clone());
        }
    }
    sqlx::query(
        "UPDATE tasks SET deps = ?, stage = ?, status = ?, agent_id = ?, last_agent_id = NULL, \
         last_error = NULL, result_summary = ?, updated_at = ? WHERE id = ?",
    )
    .bind(models_list_json(&deps))
    .bind(TASK_STAGE_PENDING)
    .bind(pause.unwrap_or(STATUS_QUEUED))
    .bind(&agent.id)
    .bind(summary)
    .bind(&timestamp)
    .bind(&parent.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&parent.id),
        "task_derived",
        format!(
            "任务 {} 派生 {} 个子任务：{}",
            parent.key,
            child_keys.len(),
            child_keys.join(", ")
        ),
    )
    .await;
    Ok(())
}

async fn handle_derived_task_success(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    summary: &str,
    agent: &AgentRow,
) -> Result<()> {
    let parent_id = task
        .parent_id
        .clone()
        .ok_or_else(|| anyhow!("任务不是派生任务"))?;
    let parent = load_task(&state.db, &parent_id)
        .await?
        .ok_or_else(|| anyhow!("派生任务的父任务不存在"))?;
    if parent.status.as_deref() == Some(STATUS_CANCELED) {
        return Ok(());
    }
    let parent_worktree = match parent.worktree.as_deref() {
        Some(path) if Path::new(path).join(".git").exists() => PathBuf::from(path),
        _ => prepare_task_worktree(state, project, Some(&parent)).await?,
    };
    let branch = load_task(&state.db, &task.id)
        .await?
        .and_then(|current| current.branch)
        .ok_or_else(|| anyhow!("派生任务缺少执行分支"))?;
    let message = format!("domo(task-{}): {}", task.key, task.title);
    let merge = {
        let _guard = state.git_lock.lock().await;
        git::squash_merge(&parent_worktree, &branch, &message).await
    };
    match merge {
        Ok(_) => {
            let guard = state.schedule_lock.lock().await;
            if let Some(current) = load_task(&state.db, &task.id).await? {
                if current.status.as_deref() == Some(STATUS_CANCELED)
                    || current.stage == TASK_STAGE_COMPLETED
                {
                    return Ok(());
                }
            }
            sqlx::query(
                "UPDATE tasks SET stage = ?, status = NULL, agent_id = ?, last_error = NULL, \
                 result_summary = ?, updated_at = ? WHERE id = ?",
            )
            .bind(TASK_STAGE_COMPLETED)
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
                "task_completed",
                format!(
                    "派生任务 {} 已跳过验收并合入父任务 {} 的分支",
                    task.key, parent.key
                ),
            )
            .await;
            drop(guard);
            cleanup_task_artifacts(state, project, task).await;
        }
        Err(error) => {
            let reason = format!("合入父任务分支失败：{error:#}");
            handle_executor_failure(state, project, task, &reason, agent, false).await?;
        }
    }
    Ok(())
}

async fn cleanup_task_artifacts(state: &AppState, project: &ProjectRow, task: &TaskRow) {
    if let Err(error) = crate::artifacts::cleanup_task(state, project, task, false).await {
        tracing::warn!(
            task = %task.id,
            error = %format!("{error:#}"),
            "清理任务工作区失败"
        );
    }
    let Some(parent_id) = task.parent_id.as_deref() else {
        return;
    };
    let parent = match load_task(&state.db, parent_id).await {
        Ok(Some(parent)) => parent,
        _ => return,
    };
    if parent.stage != TASK_STAGE_COMPLETED {
        return;
    }
    if let Err(error) = crate::artifacts::cleanup_task(state, project, &parent, false).await {
        tracing::warn!(
            task = %parent.id,
            error = %format!("{error:#}"),
            "清理父任务工作区失败"
        );
    }
}

async fn handle_executor_failure(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    reason: &str,
    agent: &AgentRow,
    process_failure: bool,
) -> Result<()> {
    let kind = domain::classify_failure(reason, process_failure);
    let effect = apply_agent_failure(state, agent, reason, kind).await?;
    let _guard = state.schedule_lock.lock().await;
    let pause = task_pause_intent(state, &task.id).await?;
    let attempts = task.attempts + 1;
    let mut blocked = !kind.is_agent_failure() && attempts >= 3;
    if kind.is_agent_failure()
        && effect.switch
        && !has_usable_agent(state, &project.id, ROLE_EXECUTOR, Some(&agent.id)).await?
    {
        blocked = true;
    }
    let status = match pause {
        Some(paused) => paused,
        None if blocked => STATUS_BLOCKED,
        None => STATUS_QUEUED,
    };
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, attempts = ?, last_agent_id = ?, \
         last_failure_kind = ?, last_error = ?, agent_id = NULL, waiting_reason = NULL, \
         avoid_agent_id = CASE WHEN ? THEN ? ELSE avoid_agent_id END, \
         avoid_agent_role = CASE WHEN ? THEN ? ELSE avoid_agent_role END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_EXECUTING)
    .bind(status)
    .bind(attempts)
    .bind(if effect.switch {
        Some(agent.id.as_str())
    } else {
        None
    })
    .bind(kind.as_str())
    .bind(reason)
    .bind(effect.switch)
    .bind(if effect.switch {
        Some(agent.id.as_str())
    } else {
        None
    })
    .bind(effect.switch)
    .bind(if effect.switch {
        Some(ROLE_EXECUTOR)
    } else {
        None
    })
    .bind(now())
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    let kind_label = failure_kind_label(kind);
    if status == STATUS_BLOCKED {
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            Some(&task.id),
            "task_blocked",
            format!(
                "任务 {} 连续失败或没有可用 Agent，已阻塞（{}）：{reason}",
                task.key, kind_label
            ),
        )
        .await;
    } else {
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            Some(&task.id),
            "task_retry",
            format!(
                "任务 {} 执行失败（{}），{}：{reason}",
                task.key,
                kind_label,
                if effect.switch {
                    "已切换 Agent 重试"
                } else {
                    "将在原 Agent 上重试"
                }
            ),
        )
        .await;
    }
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
        let current = load_task(&state.db, &task.id)
            .await?
            .unwrap_or_else(|| task.clone());
        if current.milestone != 0 {
            let _guard = state.schedule_lock.lock().await;
            let verdict = if reason.trim().is_empty() {
                "验收通过".to_string()
            } else {
                reason.trim().to_string()
            };
            let blocked_reason = format!("任务 {} 验收通过，等待人工验收：{verdict}", task.key);
            sqlx::query(
                "UPDATE tasks SET stage = ?, status = ?, agent_id = ?, human_gate = ?, \
                 last_error = ?, forced_agent_id = NULL, forced_agent_role = NULL, updated_at = ? \
                 WHERE id = ?",
            )
            .bind(TASK_STAGE_VERIFYING)
            .bind(STATUS_BLOCKED)
            .bind(&agent.id)
            .bind(HUMAN_GATE_MILESTONE)
            .bind(&blocked_reason)
            .bind(now())
            .bind(&task.id)
            .execute(&state.db)
            .await?;
            sqlx::query(
                "UPDATE projects SET status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
                 THEN status ELSE ? END, blocked_reason = ?, updated_at = ? WHERE id = ?",
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
                Some(&task.id),
                "milestone_acceptance_required",
                format!(
                    "任务 {} 是里程碑任务，验收通过后等待人工确认：{verdict}",
                    task.key
                ),
            )
            .await;
            return Ok(());
        }
        let branch = task
            .branch
            .clone()
            .ok_or_else(|| anyhow!("任务缺少执行分支"))?;
        let workspace = PathBuf::from(&project.workspace);
        let message = format!("domo(task-{}): {}", task.key, task.title);
        let merge = {
            let _guard = state.git_lock.lock().await;
            git::squash_merge(&paths::project_repo(&workspace), &branch, &message).await
        };
        match merge {
            Ok(_) => {
                let guard = state.schedule_lock.lock().await;
                sqlx::query(
                    "UPDATE tasks SET stage = ?, status = NULL, agent_id = ?, last_error = NULL, \
                     human_gate = NULL, forced_agent_id = NULL, forced_agent_role = NULL, \
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
                drop(guard);
                cleanup_task_artifacts(state, project, task).await;
            }
            Err(error) => {
                handoff_merge_conflict(state, project, task, &error, agent).await?;
            }
        }
    } else {
        let _guard = state.schedule_lock.lock().await;
        let rejections = task.rejections + 1;
        let blocked = rejections >= 3;
        let pause = task_pause_intent(state, &task.id).await?;
        let status = match pause {
            Some(paused) => paused,
            None if blocked => STATUS_BLOCKED,
            None => STATUS_QUEUED,
        };
        let merged_issues = merge_issues(&task.verifier_issues, issues);
        sqlx::query(
            "UPDATE tasks SET stage = ?, status = ?, rejections = ?, verifier_issues = ?, \
             last_error = ?, agent_id = ?, waiting_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(TASK_STAGE_EXECUTING)
        .bind(status)
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
            if status == STATUS_BLOCKED {
                "task_blocked"
            } else {
                "task_rejected"
            },
            if status == STATUS_BLOCKED {
                format!("任务 {} 连续 3 次验收未通过，已阻塞：{reason}", task.key)
            } else {
                format!("任务 {} 验收未通过，退回执行：{reason}", task.key)
            },
        )
        .await;
    }
    Ok(())
}

/// A rejected verdict returns the task to the executor with verifier issues and
/// counts against the rejection limit. A merge conflict uses the same handoff
/// path but keeps the rejection count intact: the executor only resolves the
/// conflict, then the verifier checks the task again.
async fn handoff_merge_conflict(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    merge_error: &anyhow::Error,
    agent: &AgentRow,
) -> Result<()> {
    let reason = format!("验收通过但合并失败：{merge_error:#}");
    let worktree = match prepare_task_worktree(state, project, Some(task)).await {
        Ok(path) => path,
        Err(error) => {
            return block_task_after_merge_failure(
                state,
                project,
                task,
                &format!("{reason}；准备冲突处理工作区失败：{error:#}"),
            )
            .await;
        }
    };
    let conflicts = {
        let _guard = state.git_lock.lock().await;
        git::merge_conflict_files(&worktree, &project.branch).await
    };
    let conflicts = match conflicts {
        Ok(files) if !files.is_empty() => files,
        Ok(_) => {
            return block_task_after_merge_failure(
                state,
                project,
                task,
                &format!("{reason}；任务分支与项目交付分支没有文件冲突"),
            )
            .await;
        }
        Err(error) => {
            return block_task_after_merge_failure(
                state,
                project,
                task,
                &format!("{reason}；在任务工作区复现冲突失败：{error:#}"),
            )
            .await;
        }
    };
    let issue = format!(
        "验收通过后合并到项目交付分支时发生合并冲突，冲突文件：{}。工作区已处于合并冲突状态，\
         请直接编辑冲突文件消除冲突标记，保留任务需要的改动；DOMO 会负责提交并在解决后重新验收。",
        conflicts.join("、")
    );
    let _guard = state.schedule_lock.lock().await;
    let pause = task_pause_intent(state, &task.id).await?;
    let issues = merge_issues(&task.verifier_issues, std::slice::from_ref(&issue));
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, verifier_issues = ?, last_error = ?, agent_id = ?, \
         waiting_reason = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_EXECUTING)
    .bind(pause.unwrap_or(STATUS_QUEUED))
    .bind(&issues)
    .bind(&issue)
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
        "merge_conflict",
        format!(
            "任务 {} 合并冲突：合入项目分支失败，已退回执行者解决，之后重新验收：{}",
            task.key,
            conflicts.join("、")
        ),
    )
    .await;
    Ok(())
}

async fn block_task_after_merge_failure(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    reason: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, last_error = ?, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_VERIFYING)
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
        "merge_conflict",
        format!("任务 {} 合并冲突：{reason}", task.key),
    )
    .await;
    Ok(())
}

async fn handle_verifier_failure(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    reason: &str,
    agent: &AgentRow,
    process_failure: bool,
) -> Result<()> {
    let kind = domain::classify_failure(reason, process_failure);
    let effect = apply_agent_failure(state, agent, reason, kind).await?;
    let _guard = state.schedule_lock.lock().await;
    let pause = task_pause_intent(state, &task.id).await?;
    let attempts = task.attempts + 1;
    let mut blocked = !kind.is_agent_failure() && attempts >= 3;
    if kind.is_agent_failure()
        && effect.switch
        && !has_usable_agent(state, &project.id, ROLE_VERIFIER, Some(&agent.id)).await?
    {
        blocked = true;
    }
    let status = match pause {
        Some(paused) => paused,
        None if blocked => STATUS_BLOCKED,
        None => STATUS_QUEUED,
    };
    sqlx::query(
        "UPDATE tasks SET attempts = ?, status = ?, last_agent_id = ?, last_failure_kind = ?, \
         last_error = ?, agent_id = NULL, waiting_reason = NULL, \
         avoid_agent_id = CASE WHEN ? THEN ? ELSE avoid_agent_id END, \
         avoid_agent_role = CASE WHEN ? THEN ? ELSE avoid_agent_role END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(attempts)
    .bind(status)
    .bind(if effect.switch {
        Some(agent.id.as_str())
    } else {
        None
    })
    .bind(kind.as_str())
    .bind(reason)
    .bind(effect.switch)
    .bind(if effect.switch {
        Some(agent.id.as_str())
    } else {
        None
    })
    .bind(effect.switch)
    .bind(if effect.switch {
        Some(ROLE_VERIFIER)
    } else {
        None
    })
    .bind(now())
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        if status == STATUS_BLOCKED {
            "task_blocked"
        } else {
            "task_retry"
        },
        format!(
            "任务 {} 验收作业失败（{}）：{reason}",
            task.key,
            failure_kind_label(kind)
        ),
    )
    .await;
    Ok(())
}

async fn handle_report(state: &AppState, project: &ProjectRow, report: &str) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
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

async fn handle_summary_failure(
    state: &AppState,
    project: &ProjectRow,
    reason: &str,
    agent: Option<&AgentRow>,
    process_failure: bool,
) -> Result<()> {
    let failure = plan_project_failure(
        state,
        project,
        reason,
        agent.map(|agent| (agent, process_failure)),
    )
    .await?;
    let _guard = state.schedule_lock.lock().await;
    if has_active_tasks(&state.db, &project.id).await? {
        return apply_stale_result(
            state,
            project,
            "summary_failed",
            format!("总结失败：{reason}（项目已有未完成任务，回到执行阶段）"),
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
        failure.event_kind("summary_failed"),
        format!("总结{}：{reason}", failure.label()),
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
) -> Result<bool> {
    let failure_kind = if status == JOB_STATUS_FAILED {
        error.map(|reason| domain::classify_failure(reason, run.process_failure()).as_str())
    } else {
        None
    };
    let usage = run.usage.clone().or_else(|| {
        result
            .as_deref()
            .and_then(crate::usage::UsageRecord::from_result)
    });
    let finished_at = now();
    let started_at: Option<String> = sqlx::query_scalar("SELECT started_at FROM jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_optional(&state.db)
        .await?
        .flatten();
    let duration_ms = usage
        .as_ref()
        .and_then(|usage| usage.duration_ms)
        .or_else(|| {
            started_at
                .as_deref()
                .and_then(|started| crate::usage::elapsed_ms(Some(started), &finished_at))
        });
    let updated = sqlx::query(
        "UPDATE jobs SET status = ?, result = ?, stdout = ?, stderr = ?, error = ?, \
         exit_code = ?, failure_kind = ?, input_tokens = ?, output_tokens = ?, \
         cache_read_tokens = ?, cache_write_tokens = ?, reasoning_tokens = ?, cost = ?, \
         duration_ms = ?, usage_known = ?, finished_at = ? WHERE id = ? AND status = 'running'",
    )
    .bind(status)
    .bind(result)
    .bind(&run.stdout)
    .bind(&run.stderr)
    .bind(error)
    .bind(run.exit_code)
    .bind(failure_kind)
    .bind(usage.as_ref().and_then(|usage| usage.input_tokens))
    .bind(usage.as_ref().and_then(|usage| usage.output_tokens))
    .bind(usage.as_ref().and_then(|usage| usage.cache_read_tokens))
    .bind(usage.as_ref().and_then(|usage| usage.cache_write_tokens))
    .bind(usage.as_ref().and_then(|usage| usage.reasoning_tokens))
    .bind(usage.as_ref().and_then(|usage| usage.cost))
    .bind(duration_ms)
    .bind(if usage.as_ref().is_some_and(|usage| usage.known()) {
        1
    } else {
        0
    })
    .bind(&finished_at)
    .bind(&job.id)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Ok(false);
    }
    if status == JOB_STATUS_SUCCEEDED {
        if let Some(agent_id) = &job.agent_id {
            let _ = sqlx::query(
                "UPDATE agents SET last_error = NULL, cooldown_until = NULL, \
                 failure_kind = NULL, transient_failures = 0, updated_at = ? WHERE id = ?",
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
    emit_job(
        &state.db,
        &state.bus,
        Some(&job.project_id),
        job.task_id.as_deref(),
        Some(&job.id),
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
    Ok(true)
}

async fn fail_runtime(state: &AppState, job_id: &str, reason: &str) -> Result<()> {
    let Some(job) = load_job(&state.db, job_id).await? else {
        return Ok(());
    };
    if job.status != JOB_STATUS_RUNNING {
        return Ok(());
    }
    let finished_at = now();
    let duration_ms = job
        .started_at
        .as_deref()
        .and_then(|started| crate::usage::elapsed_ms(Some(started), &finished_at));
    sqlx::query(
        "UPDATE jobs SET status = ?, error = ?, duration_ms = COALESCE(duration_ms, ?), \
         finished_at = ? WHERE id = ?",
    )
    .bind(JOB_STATUS_FAILED)
    .bind(reason)
    .bind(duration_ms)
    .bind(&finished_at)
    .bind(job_id)
    .execute(&state.db)
    .await?;
    let project_id = job.project_id.clone();
    let job_agent = match &job.agent_id {
        Some(agent_id) => crate::db::load_agent(&state.db, agent_id).await?,
        None => None,
    };
    if let Some(project) = load_project(&state.db, &project_id).await? {
        if let Some(task_id) = &job.task_id {
            if let Some(task) = load_task(&state.db, task_id).await? {
                let agent = job_agent.as_ref();
                match job.role.as_str() {
                    JOB_ROLE_EXECUTOR => {
                        if let Some(agent) = agent {
                            handle_executor_failure(state, &project, &task, reason, agent, true)
                                .await?;
                        } else {
                            block_task(state, &project, &task, reason).await?;
                        }
                    }
                    JOB_ROLE_VERIFIER => {
                        if let Some(agent) = agent {
                            handle_verifier_failure(state, &project, &task, reason, agent, true)
                                .await?;
                        } else {
                            block_task(state, &project, &task, reason).await?;
                        }
                    }
                    JOB_ROLE_ANALYSIS => {
                        handle_analysis_failure(state, &project, reason, agent, true).await?
                    }
                    _ => {}
                }
            }
        } else if job.role == JOB_ROLE_ANALYSIS {
            handle_analysis_failure(state, &project, reason, job_agent.as_ref(), true).await?;
        } else if job.role == JOB_ROLE_DIRECT {
            handle_direct_failure(state, &project, reason, job_agent.as_ref(), true).await?;
        } else if job.role == JOB_ROLE_SUMMARY {
            handle_summary_failure(state, &project, reason, job_agent.as_ref(), true).await?;
        }
    }
    emit_job(
        &state.db,
        &state.bus,
        Some(&job.project_id),
        job.task_id.as_deref(),
        Some(&job.id),
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
    let finished_at = now();
    let duration_ms = job
        .started_at
        .as_deref()
        .and_then(|started| crate::usage::elapsed_ms(Some(started), &finished_at));
    sqlx::query(
        "UPDATE jobs SET status = ?, error = ?, duration_ms = COALESCE(duration_ms, ?), \
         finished_at = ? WHERE id = ?",
    )
    .bind(JOB_STATUS_CANCELED)
    .bind("项目已归档，作业被取消")
    .bind(duration_ms)
    .bind(&finished_at)
    .bind(&job.id)
    .execute(&state.db)
    .await?;
    Ok(())
}

async fn mark_canceled_job(state: &AppState, job: &JobRow, reason: &str) -> Result<()> {
    let finished_at = now();
    let duration_ms = job
        .started_at
        .as_deref()
        .and_then(|started| crate::usage::elapsed_ms(Some(started), &finished_at));
    sqlx::query(
        "UPDATE jobs SET status = ?, error = ?, duration_ms = COALESCE(duration_ms, ?), \
         finished_at = ? WHERE id = ? AND status = 'running'",
    )
    .bind(JOB_STATUS_CANCELED)
    .bind(reason)
    .bind(duration_ms)
    .bind(&finished_at)
    .bind(&job.id)
    .execute(&state.db)
    .await?;
    Ok(())
}

struct AgentFailureEffect {
    switch: bool,
}

async fn apply_agent_failure(
    state: &AppState,
    agent: &AgentRow,
    reason: &str,
    kind: FailureKind,
) -> Result<AgentFailureEffect> {
    let mut switch = false;
    let mut transient_failures = agent.transient_failures;
    let mut cooldown: Option<String> = None;
    match kind {
        FailureKind::RateLimit => {
            let until = chrono::Utc::now() + domain::rate_limit_cooldown(reason);
            cooldown = Some(until.to_rfc3339());
            transient_failures = 0;
            switch = true;
        }
        FailureKind::Transient => {
            let next = transient_failures + 1;
            transient_failures = next;
            if next >= domain::transient_retry_limit() {
                cooldown = Some((chrono::Utc::now() + chrono::Duration::minutes(30)).to_rfc3339());
                switch = true;
            }
        }
        FailureKind::Permanent => {
            transient_failures = 0;
            switch = true;
        }
        FailureKind::Task => {}
    }
    sqlx::query(
        "UPDATE agents SET last_error = ?, failure_kind = COALESCE(?, failure_kind), \
         transient_failures = ?, cooldown_until = COALESCE(?, cooldown_until), updated_at = ? \
         WHERE id = ?",
    )
    .bind(reason)
    .bind(kind.is_agent_failure().then(|| kind.as_str()))
    .bind(transient_failures)
    .bind(cooldown)
    .bind(now())
    .bind(&agent.id)
    .execute(&state.db)
    .await?;
    Ok(AgentFailureEffect { switch })
}

/// Failure handling shared by project jobs. Their skip state lives on the
/// project row instead of a task, and there is no attempt budget.
struct ProjectFailure {
    kind: FailureKind,
    switch: bool,
    blocked: bool,
    blocked_reason: Option<String>,
}

impl ProjectFailure {
    fn status(&self) -> &'static str {
        if self.blocked {
            STATUS_BLOCKED
        } else {
            STATUS_RUNNING
        }
    }

    fn label(&self) -> String {
        let kind = failure_kind_label(self.kind);
        if self.blocked {
            format!("失败（{kind}，没有其他可用 Agent），等待人工处理")
        } else if self.switch {
            format!("失败（{kind}），已切换 Agent 重试")
        } else {
            format!("失败（{kind}），将在原 Agent 上重试")
        }
    }

    fn event_kind(&self, failed: &str) -> &'static str {
        if self.blocked {
            "project_blocked"
        } else if self.switch {
            "project_retry"
        } else {
            match failed {
                "direct_failed" => "direct_failed",
                "summary_failed" => "summary_retry",
                _ => "analysis_retry",
            }
        }
    }
}

async fn plan_project_failure(
    state: &AppState,
    project: &ProjectRow,
    reason: &str,
    agent: Option<(&AgentRow, bool)>,
) -> Result<ProjectFailure> {
    let Some((agent, process_failure)) = agent else {
        return Ok(ProjectFailure {
            kind: FailureKind::Task,
            switch: false,
            blocked: true,
            blocked_reason: Some(reason.to_string()),
        });
    };
    let kind = domain::classify_failure(reason, process_failure);
    let effect = apply_agent_failure(state, agent, reason, kind).await?;
    // Project jobs (analysis, direct, summary) all dispatch from the planner
    // group of the project pool.
    let blocked = kind.is_agent_failure()
        && effect.switch
        && !has_usable_agent(state, &project.id, ROLE_PLANNER, Some(&agent.id)).await?;
    Ok(ProjectFailure {
        kind,
        switch: effect.switch,
        blocked,
        blocked_reason: blocked.then(|| reason.to_string()),
    })
}

async fn has_usable_agent(
    state: &AppState,
    project_id: &str,
    role: &str,
    exclude: Option<&str>,
) -> Result<bool> {
    crate::db::ensure_project_agents(&state.db, project_id).await?;
    let rows = sqlx::query_as::<_, ProjectAgentRow>(
        "SELECT * FROM project_agents WHERE project_id = ? AND role = ? AND enabled = 1",
    )
    .bind(project_id)
    .bind(role)
    .fetch_all(&state.db)
    .await?;
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents")
        .fetch_all(&state.db)
        .await?;
    let now = chrono::Utc::now();
    for row in rows {
        if exclude == Some(row.agent_id.as_str()) {
            continue;
        }
        let Some(agent) = agents.iter().find(|agent| agent.id == row.agent_id) else {
            continue;
        };
        if agent.enabled == 0 {
            continue;
        }
        if !parse_json_list(&agent.roles)
            .iter()
            .any(|item| item == role)
        {
            continue;
        }
        let cooling = agent
            .cooldown_until
            .as_deref()
            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
            .map(|until| until.with_timezone(&chrono::Utc) > now)
            .unwrap_or(false);
        if cooling {
            continue;
        }
        if crate::usage::agent_gate(
            &state.db,
            &agent.id,
            agent.availability_json.as_deref(),
            agent.usage_limit_json.as_deref(),
            agent.usage_reset_at.as_deref(),
            now,
        )
        .await?
        .is_some()
        {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

/// A graceful task pause only takes effect when the in-flight job ends, so
/// transitions that would normally enqueue the next stage land in paused.
async fn task_pause_intent(state: &AppState, task_id: &str) -> Result<Option<&'static str>> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM tasks WHERE id = ?")
        .bind(task_id)
        .fetch_optional(&state.db)
        .await?;
    Ok(match status.as_deref() {
        Some(STATUS_PAUSING) | Some(STATUS_PAUSED) => Some(STATUS_PAUSED),
        _ => None,
    })
}

fn failure_kind_label(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::RateLimit => "限流",
        FailureKind::Transient => "供应商波动",
        FailureKind::Permanent => "永久错误",
        FailureKind::Task => "任务失败",
    }
}

async fn has_active_tasks(pool: &SqlitePool, project_id: &str) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage != ? \
         AND COALESCE(status, '') != ?",
    )
    .bind(project_id)
    .bind(TASK_STAGE_COMPLETED)
    .bind(STATUS_CANCELED)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

/// An analysis or summary result is stale when the project already carries
/// active tasks, for example after tasks were added by hand while the job was
/// in flight. The project returns to the executing stage and the stale result
/// only remains in the audit log.
async fn apply_stale_result(
    state: &AppState,
    project: &ProjectRow,
    kind: &str,
    message: String,
) -> Result<()> {
    sqlx::query(
        "UPDATE projects SET stage = ?, status = CASE WHEN status IN \
         ('pausing', 'paused', 'archived') THEN status ELSE ? END, blocked_reason = NULL, \
         report = NULL, report_feedback = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(STAGE_EXECUTING)
    .bind(STATUS_RUNNING)
    .bind(now())
    .bind(&project.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        None,
        kind,
        message,
    )
    .await;
    Ok(())
}

pub(crate) async fn insert_message(
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
        JOB_ROLE_ANALYSIS => "项目分析",
        JOB_ROLE_DIRECT => "直接完成",
        JOB_ROLE_EXECUTOR => "执行",
        JOB_ROLE_VERIFIER => "验收",
        JOB_ROLE_SUMMARY => "总结",
        _ => "未知",
    }
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
