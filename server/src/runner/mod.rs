//! Job lifecycle. Long-running agent processes never hold the scheduler lock;
//! state transitions are applied atomically after the process exits.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Result};
use tokio::fs;

use crate::agents::{self, AgentSpec};
use crate::domain::{now, parse_result, short_id, ParsedResult};
use crate::events::emit_job;
use crate::git;
use crate::models::*;
use crate::paths;
use crate::prompts;
use crate::replan;
use crate::state::AppState;

mod agent;
mod analysis;
mod common;
mod direct;
mod executor;
mod failure;
mod lifecycle;
mod project;
mod prompt;
mod verifier;
mod worktree;

use self::agent::{append_log_separator, continuation_allowed, merge_usage, run_agent_job};
pub(crate) use self::analysis::apply_analysis_plan;
use self::analysis::{
    handle_analysis_direct, handle_analysis_failure, handle_analysis_plan,
    handle_analysis_questions,
};
pub(crate) use self::common::{analysis_needs_dispatch, insert_message};
use self::common::{
    load_job, load_project, load_task, parsed_error, result_is_failure, role_label,
};
use self::direct::{handle_direct_failure, handle_direct_success};
use self::executor::{
    handle_derived_task_success, handle_executor_derive, handle_executor_failure,
    handle_executor_success, validate_derive_request,
};
use self::lifecycle::{fail_runtime, finish_job, mark_archived_job, mark_canceled_job};
use self::project::{
    handle_acceptance_failure, handle_project_acceptance, handle_report, handle_summary_failure,
};
use self::prompt::{build_context, build_prompt_data};
use self::verifier::{handle_verdict, handle_verifier_failure};
use self::worktree::{ensure_worktree_root, prepare_task_worktree, prepare_verify_worktree};

/// A clean run without a result file gets up to five continuation rounds in the
/// same job before the protocol failure is settled.
const MAX_CONTINUATIONS: usize = 5;
/// A timeout may resume the same session with a fresh run window. The total
/// job time stays bounded by this count plus the initial run.
const MAX_TIMEOUT_CONTINUATIONS: usize = 2;

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
    let _ = fs::write(job_dir.join("stdout.log"), b"").await;
    let _ = fs::write(job_dir.join("stderr.log"), b"").await;

    let workspace = PathBuf::from(&project.workspace);
    let cwd = if project.is_git() {
        match job.role.as_str() {
            JOB_ROLE_ANALYSIS | JOB_ROLE_DIRECT | JOB_ROLE_ACCEPTANCE | JOB_ROLE_SUMMARY => {
                paths::project_repo(&workspace)
            }
            JOB_ROLE_EXECUTOR => prepare_task_worktree(state, &project, task.as_ref()).await?,
            JOB_ROLE_VERIFIER => {
                prepare_verify_worktree(state, &project, task.as_ref(), &job.id).await?
            }
            other => return Err(anyhow!("未知作业角色: {other}")),
        }
    } else {
        // 普通工作区的全部角色共用所选本地目录，不创建 worktree。
        PathBuf::from(&project.repo_path)
    };
    ensure_worktree_root(&cwd, project.is_git()).await?;
    if job.role == JOB_ROLE_DIRECT && project.is_git() {
        let _guard = state.git_lock.lock().await;
        git::reset_hard(&cwd).await?;
    }

    let prompt_data = build_prompt_data(state, &project, task.as_ref(), &job, &cwd).await?;
    let full_prompt = prompts::render(&prompt_data);
    let resume = job.resume != 0;
    let prompt = if resume {
        prompts::continuation(&prompt_data)
    } else {
        full_prompt.clone()
    };
    fs::write(job_dir.join("prompt.md"), &full_prompt).await?;
    if resume {
        fs::write(job_dir.join("resume.md"), &prompt).await?;
    }
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

    sqlx::query("UPDATE jobs SET prompt = ?, started_at = ?, resume = 0 WHERE id = ?")
        .bind(&full_prompt)
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

    if std::env::var("DOMO_MOCK_RUNTIME_FAIL_ROLE").is_ok_and(|role| role == job.role) {
        return Err(anyhow!("mock 运行时失败"));
    }
    let progress = agents::JobProgress::new();
    let mut job_deadline = Instant::now() + state.cfg.job_timeout;
    let mut run_timeout = state.cfg.job_timeout;
    let mut spec = AgentSpec {
        job_id: job.id.clone(),
        role: job.role.clone(),
        cwd: cwd.clone(),
        job_dir: job_dir.clone(),
        prompt,
        original_prompt: full_prompt,
        session_id: agents::session_for_job(&agent, &job.id),
        continuation: resume,
        progress: progress.clone(),
    };
    let mut run =
        run_agent_job(state, &job, &job_dir, &agent, &spec, &progress, run_timeout).await?;
    spec.session_id = run.session_id.clone();
    let mut combined_stdout = run.stdout.clone();
    let mut combined_stderr = run.stderr.clone();
    let mut continuations = 0;
    let mut timeout_continuations = 0;
    loop {
        let process_ok = run.exit_code == Some(0) && !run.timed_out && !run.stalled;
        if !process_ok {
            // A timeout is a process failure, but the session still holds the
            // work context. Resume the same session with a short continuation
            // prompt and a fresh run window.
            let retryable = run.timed_out;
            if retryable
                && timeout_continuations < MAX_TIMEOUT_CONTINUATIONS
                && continuation_allowed(state, &job, &project, task.as_ref()).await?
            {
                timeout_continuations += 1;
                let follow_up = prompts::continuation(&prompt_data);
                fs::write(
                    job_dir.join(format!("timeout-continuation-{timeout_continuations}.md")),
                    &follow_up,
                )
                .await?;
                append_log_separator(&job_dir).await?;
                progress.touch();
                spec.prompt = follow_up;
                spec.continuation = true;
                emit_job(
                    &state.db,
                    &state.bus,
                    Some(&project.id),
                    job.task_id.as_deref(),
                    Some(&job.id),
                    "job_continued",
                    format!(
                        "{}作业超时，已追加第 {} 轮继续提示（复用同一会话）",
                        role_label(&job.role),
                        timeout_continuations
                    ),
                )
                .await;
                run_timeout = state.cfg.job_timeout;
                job_deadline = Instant::now() + run_timeout;
                let next =
                    run_agent_job(state, &job, &job_dir, &agent, &spec, &progress, run_timeout)
                        .await?;
                combined_stdout.push_str("\n--- 继续执行 ---\n");
                combined_stdout.push_str(&next.stdout);
                combined_stderr.push_str("\n--- 继续执行 ---\n");
                combined_stderr.push_str(&next.stderr);
                let usage = merge_usage(run.usage.clone(), next.usage.clone());
                run = next;
                run.usage = usage;
                spec.session_id = run.session_id.clone();
                continue;
            }
            break;
        }
        let missing_result = fs::read_to_string(&result_path).await.ok().is_none();
        if !missing_result
            || continuations >= MAX_CONTINUATIONS
            || !continuation_allowed(state, &job, &project, task.as_ref()).await?
        {
            break;
        }
        let remaining = job_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let follow_up = prompts::continuation(&prompt_data);
        fs::write(
            job_dir.join(format!("continuation-{}.md", continuations + 1)),
            &follow_up,
        )
        .await?;
        append_log_separator(&job_dir).await?;
        // The previous run may have ended after a long silent stretch; the
        // continuation starts a fresh stall window.
        progress.touch();
        spec.prompt = follow_up;
        spec.continuation = true;
        emit_job(
            &state.db,
            &state.bus,
            Some(&project.id),
            job.task_id.as_deref(),
            Some(&job.id),
            "job_continued",
            format!(
                "{}作业正常结束但没有写结果文件，已追加第 {} 轮继续提示",
                role_label(&job.role),
                continuations + 1
            ),
        )
        .await;
        let next =
            run_agent_job(state, &job, &job_dir, &agent, &spec, &progress, remaining).await?;
        combined_stdout.push_str("\n--- 继续执行 ---\n");
        combined_stdout.push_str(&next.stdout);
        combined_stderr.push_str("\n--- 继续执行 ---\n");
        combined_stderr.push_str(&next.stderr);
        let usage = merge_usage(run.usage.clone(), next.usage.clone());
        run = next;
        run.usage = usage;
        spec.session_id = run.session_id.clone();
        continuations += 1;
    }
    run.stdout = combined_stdout;
    run.stderr = combined_stderr;
    if !state.is_shutting_down() {
        // The watchdog kills the job process tree on stall or timeout; retry
        // here when the recorded process is still alive.
        agents::kill_job_dir(&job_dir);
    }
    let usage = agents::finish_job(agents::FinishContext {
        agent: &agent,
        job_id: &job.id,
        session_id: spec.session_id.as_deref(),
        process_failed: run.process_failure(),
        shutting_down: state.is_shutting_down(),
    })
    .await;
    run.usage = agents::apply_usage(run.usage, usage);
    state.unregister_job(&job_dir);

    let raw_result = fs::read_to_string(&result_path).await.ok();
    if job.role == JOB_ROLE_VERIFIER && project.is_git() {
        // The verifier workspace is disposable and must not leak into later work.
        let _guard = state.git_lock.lock().await;
        let _ = git::remove_worktree(&paths::project_repo(&workspace), &cwd).await;
    }
    if project.is_git()
        && matches!(
            job.role.as_str(),
            JOB_ROLE_ANALYSIS | JOB_ROLE_ACCEPTANCE | JOB_ROLE_SUMMARY
        )
    {
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
    if job.role == JOB_ROLE_ANALYSIS
        && fresh_project.goal_locked_at.is_some()
        && (fresh_project.goal != project.goal
            || fresh_project.replan_generation != job.replan_generation
            || (job.replan_generation > 0 && fresh_project.replan_active == 0))
    {
        mark_canceled_job(state, &job, "重规划代际已变化，丢弃过期分析结果").await?;
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
                Some(if result_is_failure(&job.role, raw) {
                    parse_error
                } else {
                    format!("结果协议不合法: {parse_error}")
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
            let commit = if project.is_git() {
                let _guard = state.git_lock.lock().await;
                git::commit_all(&cwd, &message).await
            } else {
                Ok(false)
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
                let commit = if project.is_git() {
                    let _guard = state.git_lock.lock().await;
                    git::commit_all(&cwd, &message).await
                } else {
                    Ok(false)
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
                let commit = if project.is_git() {
                    let _guard = state.git_lock.lock().await;
                    git::commit_all(&cwd, &message).await
                } else {
                    Ok(false)
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
        (
            JOB_ROLE_ACCEPTANCE,
            Some(ParsedResult::ProjectAcceptance {
                summary,
                criteria,
                replan_reason,
            }),
        ) => handle_project_acceptance(state, &project, summary, criteria, replan_reason).await,
        (JOB_ROLE_ACCEPTANCE, _) => {
            let process_failure = run.process_failure();
            handle_acceptance_failure(
                state,
                &project,
                error.as_deref().unwrap_or("项目验收失败"),
                Some(&agent),
                process_failure,
            )
            .await
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
