//! Job lifecycle. Long-running agent processes never hold the scheduler lock;
//! state transitions are applied atomically after the process exits.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Result};
use tokio::fs;

use crate::agents::{self, AgentSpec};
use crate::domain::{self, now, parse_result, short_id, ParsedResult};
use crate::events::emit_job;
use crate::git;
use crate::models::*;
use crate::paths;
use crate::prompts;
use crate::state::AppState;

mod agent;
mod analysis;
mod common;
mod direct;
mod executor;
mod failure;
mod lifecycle;
mod merge;
mod project;
mod prompt;
mod verifier;
mod worktree;

use self::agent::{append_log_separator, continuation_allowed, merge_usage, run_agent_job};
pub(crate) use self::analysis::apply_analysis_plan;
pub(crate) use self::analysis::apply_analysis_plan_locked;
use self::analysis::{
    handle_analysis_direct, handle_analysis_failure, handle_analysis_plan,
    handle_analysis_questions,
};
pub(crate) use self::common::{analysis_needs_dispatch, insert_message};
use self::common::{
    load_job, load_project, load_task, parsed_error, result_is_failure, role_label,
};
use self::direct::{handle_direct_failure, handle_direct_success};
pub(crate) use self::executor::cleanup_task_artifacts;
use self::executor::{
    handle_executor_derive, handle_executor_failure, handle_executor_success,
    validate_derive_request,
};
use self::lifecycle::{fail_runtime, finish_job, mark_archived_job, mark_canceled_job};
use self::project::{
    handle_acceptance_failure, handle_project_acceptance, handle_report, handle_summary_failure,
};
use self::prompt::{build_context, build_prompt_data};
pub(crate) use self::verifier::handoff_merge_conflict_unlocked;
use self::verifier::{handle_verdict, handle_verifier_failure};
pub(crate) use self::worktree::task_merge_target;
use self::worktree::{ensure_worktree_root, prepare_task_worktree, prepare_verify_worktree};

/// Missing or invalid results get up to five correction rounds in the same job.
const MAX_CONTINUATIONS: usize = 5;
/// Automatic recovery and manual continuation share this additional-round cap.
/// Each automatic recovery starts a fresh run window; protocol correction has
/// its own bound and does not reset the continuation count.
const MAX_TIMEOUT_CONTINUATIONS: u64 = crate::continuation::MAX_CONTINUES;

pub async fn execute_job(state: AppState, job_id: String) {
    if let Err(error) = run_job(&state, &job_id).await {
        tracing::error!(job_id, error = %error, "作业执行失败");
        if let Err(inner) = fail_runtime(&state, &job_id, &format!("{error:#}")).await {
            tracing::error!(job_id, error = %inner, "记录作业失败状态时出错");
        }
    }
    if let Ok(Some(workspace)) = sqlx::query_scalar::<_, String>(
        "SELECT p.workspace FROM projects p JOIN jobs j ON j.project_id = p.id WHERE j.id = ?",
    )
    .bind(&job_id)
    .fetch_optional(&state.db)
    .await
    {
        state.unregister_job(&paths::job_dir(&PathBuf::from(workspace), &job_id));
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
    if job.role == "merge_conflict" {
        return merge::run(state, &job).await;
    }
    let mut project = load_project(&state.db, &job.project_id)
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
    let contract = crate::delivery::job_contract(&state.db, &job.id).await?;
    if job.role != JOB_ROLE_ANALYSIS {
        if let Some(goal) = contract["goal"].as_str() {
            project.goal = goal.to_string();
        }
        if !contract["proposal"].is_null() {
            project.proposal = Some(contract["proposal"].to_string());
        }
    }

    let job_dir = paths::job_dir(&PathBuf::from(&project.workspace), &job.id);
    let collect = job.resume == 2;
    let resumed = if job.resume == 1 {
        match crate::continuation::validate(state, &job).await {
            Ok(record) => Some(record),
            Err(error) => {
                let _ = fs::remove_file(job_dir.join("continuation.json")).await;
                crate::continuation::reject_started(state, &job, &error.to_string()).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    let mut timeout_continuations = resumed.as_ref().map_or(0, |r| r.turns + 1);
    fs::create_dir_all(&job_dir).await?;
    state.register_job(job_dir.clone());
    let result_path = job_dir.join("result.json");
    if !collect {
        let _ = fs::remove_file(&result_path).await;
    }
    if resumed.is_none() && !collect {
        let _ = fs::write(job_dir.join("stdout.log"), b"").await;
        let _ = fs::write(job_dir.join("stderr.log"), b"").await;
    } else if !collect {
        append_log_separator(&job_dir).await?;
    }

    let workspace = PathBuf::from(&project.workspace);
    let review = job.role == JOB_ROLE_REPLAN_REVIEW;
    let cwd = if resumed.is_some() || collect {
        let context: serde_json::Value =
            serde_json::from_slice(&fs::read(job_dir.join("context.json")).await?)?;
        PathBuf::from(
            context["cwd"]
                .as_str()
                .ok_or_else(|| anyhow!("继续作业缺少工作区"))?,
        )
    } else if review {
        job_dir.clone()
    } else if project.is_git() {
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
    ensure_worktree_root(&cwd, project.is_git() && !review).await?;
    if let Err(error) = if review {
        Ok(())
    } else if resumed.is_none() && !collect {
        crate::resources::workspace_preflight(state, &project, &job).await
    } else {
        // The original writer may resume its unfinished files. Keep the pending
        // write marker; settlement still enforces ownership and write rights.
        Ok(())
    } {
        let reason = format!("工作区检查未通过：{error:#}");
        crate::resources::block_workspace(state, &project, &job, &reason).await?;
        return Ok(());
    }

    let prompt_data = build_prompt_data(state, &project, task.as_ref(), &job, &cwd).await?;
    let artifact_version = if collect {
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(job_dir.join("context.json")).await?)?;
        original["artifact_version"].clone()
    } else if project.is_git() && !review {
        serde_json::json!(git::commit_of(&cwd, "HEAD").await?)
    } else {
        serde_json::Value::Null
    };
    let shared =
        crate::resources::shared_snapshot(state, &project, Some(&job), task.as_ref()).await?;
    let artifacts = crate::resources::artifact_snapshot(state, &project).await?;
    fs::write(
        job_dir.join("artifacts.json"),
        serde_json::to_vec(&artifacts)?,
    )
    .await?;
    let review_context = if review {
        Some(crate::replan_review::context(state, &job).await?)
    } else {
        None
    };
    let full_prompt = prompts::render_with_inputs(
        &prompt_data,
        Some(&prompts::JobInputs {
            contract: &contract,
            artifact_version: &artifact_version,
            shared: &shared,
            artifacts: &artifacts,
            review: review_context.as_ref(),
        }),
    );
    let resume = job.resume == 1;
    let prompt = if resume {
        prompts::continuation_with_prompt(&full_prompt)
    } else {
        full_prompt.clone()
    };
    fs::write(job_dir.join("prompt.md"), &full_prompt).await?;
    if resume {
        fs::write(job_dir.join("resume.md"), &prompt).await?;
    }
    let mut context = build_context(
        &project,
        task.as_ref(),
        &job,
        &cwd,
        &job_dir,
        &prompt_data,
        &agent,
    );
    context["contract"] = contract.clone();
    context["artifact_version"] = artifact_version.clone();
    context["shared"] = shared;
    context["artifacts"] = artifacts;
    if review {
        context["replan_review"] = crate::replan_review::context(state, &job).await?;
    }
    fs::write(
        job_dir.join("context.json"),
        serde_json::to_vec_pretty(&context)?,
    )
    .await?;

    sqlx::query(
        "UPDATE jobs SET prompt = ?, started_at = ?, error = NULL, resume = 0 WHERE id = ?",
    )
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
        session_id: resumed
            .as_ref()
            .map(|r| r.session.clone())
            .or_else(|| agents::session_for_job(&agent, &job.id)),
        continuation: resume,
        progress: progress.clone(),
    };
    let mut continuations = resumed.as_ref().map_or(0, |r| r.protocol_rounds);
    if !collect {
        crate::continuation::starting(
            &agent,
            &job,
            &job_dir,
            resumed.as_ref().map(|r| r.session.as_str()),
            timeout_continuations,
            continuations,
        )
        .await?;
    }
    let mut run = if collect {
        let stdout = fs::read_to_string(job_dir.join("stdout.log"))
            .await
            .unwrap_or_default();
        let session_id = agents::recover_session(&agent, &job.id, &stdout, None);
        agents::AgentRun {
            stdout,
            stderr: fs::read_to_string(job_dir.join("stderr.log"))
                .await
                .unwrap_or_default(),
            exit_code: Some(0),
            timed_out: false,
            stalled: false,
            session_id,
            usage: None,
        }
    } else {
        run_agent_job(state, &job, &job_dir, &agent, &spec, &progress, run_timeout).await?
    };
    spec.session_id = run.session_id.clone();
    let mut combined_stdout = if resume {
        format!(
            "{}\n--- 继续执行 ---\n{}",
            job.stdout.as_deref().unwrap_or_default(),
            run.stdout
        )
    } else {
        run.stdout.clone()
    };
    let mut combined_stderr = if resume {
        format!(
            "{}\n--- 继续执行 ---\n{}",
            job.stderr.as_deref().unwrap_or_default(),
            run.stderr
        )
    } else {
        run.stderr.clone()
    };
    loop {
        if collect {
            break;
        }
        let process_ok = run.exit_code == Some(0) && !run.timed_out && !run.stalled;
        if !process_ok {
            // Only supervisor timeouts or normalized structured temporary
            // provider failures authorize automatic resumption. Unknown,
            // stalled, account and configuration errors remain human choices.
            let raw = fs::read_to_string(&result_path).await.ok();
            let record = crate::continuation::checkpoint(
                &agent,
                &job,
                &job_dir,
                &run,
                raw.as_deref(),
                timeout_continuations,
                continuations,
            )
            .await?;
            let retryable = record.as_ref().is_some_and(|r| {
                r.eligible
                    && r.diagnostic.automatic()
                    && r.diagnostic.retry_after_ms.unwrap_or(0) <= 0
            }) && crate::delivery::gate(&state.db, &project.id, None)
                .await?
                .is_none();
            if retryable
                && continuations < MAX_CONTINUATIONS
                && timeout_continuations < MAX_TIMEOUT_CONTINUATIONS
                && continuation_allowed(state, &job, &project, task.as_ref()).await?
            {
                timeout_continuations += 1;
                let follow_up = format!(
                    "上一轮异常中断。\n{}",
                    prompts::continuation_with_prompt(&spec.original_prompt)
                );
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
                        "{}作业临时中断，已追加第 {} 轮自动继续提示（复用同一会话）",
                        role_label(&job.role),
                        timeout_continuations
                    ),
                )
                .await;
                run_timeout = state.cfg.job_timeout;
                job_deadline = Instant::now() + run_timeout;
                crate::continuation::starting(
                    &agent,
                    &job,
                    &job_dir,
                    spec.session_id.as_deref(),
                    timeout_continuations,
                    continuations,
                )
                .await?;
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
        if run.reported_error.is_some() {
            // An execution failure reported in the result event is not an
            // incomplete result protocol, so it goes through failure policy
            // instead of consuming the continuation budget.
            break;
        }
        let current_result = fs::read_to_string(&result_path).await.ok();
        let mut protocol_error = match current_result.as_deref() {
            None => Some("Agent 没有写 result.json".to_string()),
            Some(raw) if result_is_failure(&job.role, raw) => None,
            Some(raw) => match parse_result(&job.role, raw) {
                Err(error) => Some(error),
                Ok(ParsedResult::ExecutorDerive { tasks, .. }) => {
                    validate_derive_request(state, &project, task.as_ref(), &tasks).await?
                }
                Ok(_) if review => crate::replan_review::validate_result(&state.db, &job, raw)
                    .await
                    .err(),
                Ok(_) if job.role == JOB_ROLE_VERIFIER => crate::delivery::validate_evidence(
                    raw,
                    &contract,
                    &prompt_data
                        .task
                        .as_ref()
                        .map(|task| task.acceptance.clone())
                        .unwrap_or_default(),
                    &artifact_version,
                )
                .err(),
                Ok(_) if job.role == JOB_ROLE_ACCEPTANCE => {
                    crate::delivery::validate_project_evidence(
                        raw,
                        &contract,
                        &prompt_data.acceptance_criteria,
                        &artifact_version,
                    )
                    .err()
                }
                Ok(_) => None,
            },
        };
        if protocol_error.is_none() && !review {
            if let Some(raw) = current_result.as_deref() {
                if matches!(
                    parse_result(&job.role, raw),
                    Ok(ParsedResult::ExecutorDone { .. }
                        | ParsedResult::ExecutorDerive { .. }
                        | ParsedResult::DirectDone { .. })
                ) {
                    protocol_error = match crate::domain::commit_files(raw) {
                        Err(error) => Some(error),
                        Ok(files) if project.is_git() => {
                            let _guard = state.git_lock.lock().await;
                            git::validate_commit_files(&cwd, &files)
                                .await
                                .err()
                                .map(|e| e.to_string())
                        }
                        Ok(_) => None,
                    };
                }
            }
        }
        if protocol_error.is_none() && !review {
            if let Some(raw) = current_result.as_deref() {
                if let Some(value) =
                    crate::domain::extract_json(raw).and_then(|v| v.get("shared_updates").cloned())
                {
                    match serde_json::from_value::<Vec<crate::resources::SharedInput>>(value) {
                        Ok(records) => {
                            if let Err(error) = crate::resources::publish(
                                state,
                                &project,
                                job.task_id.as_deref(),
                                Some(&job.id),
                                &records,
                            )
                            .await
                            {
                                protocol_error = Some(format!(
                                    "共享记录提交冲突，请提交者修正后重试：{error:#}"
                                ));
                            }
                        }
                        Err(error) => {
                            protocol_error = Some(format!("shared_updates 格式错误：{error}"))
                        }
                    }
                }
            }
        }
        if protocol_error.is_none()
            || continuations >= MAX_CONTINUATIONS
            || !continuation_allowed(state, &job, &project, task.as_ref()).await?
        {
            break;
        }
        let remaining = job_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let protocol_error = protocol_error.unwrap_or_default();
        let follow_up = if current_result.is_none() {
            prompts::missing_result_continuation(&prompt_data, &spec.original_prompt)
        } else {
            prompts::invalid_result_continuation(
                &prompt_data,
                &protocol_error,
                &spec.original_prompt,
            )
        };
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
                "{}作业结果协议校验失败（{}），已追加第 {} 轮继续提示",
                role_label(&job.role),
                protocol_error,
                continuations + 1
            ),
        )
        .await;
        continuations += 1;
        crate::continuation::starting(
            &agent,
            &job,
            &job_dir,
            spec.session_id.as_deref(),
            timeout_continuations,
            continuations,
        )
        .await?;
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
    }
    let interruption_run = run.clone();
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
    let authoritative = matches!(&usage, agents::JobUsage::Authoritative(_));
    run.usage = agents::apply_usage(run.usage, usage);
    if resume && authoritative {
        run.usage = crate::continuation::usage_delta(state, &job.id, run.usage).await?;
    }

    let raw_result = fs::read_to_string(&result_path).await.ok();
    let current = load_job(&state.db, &job.id).await?;
    let mut record = if current
        .as_ref()
        .is_some_and(|j| j.status == JOB_STATUS_RUNNING)
    {
        crate::continuation::checkpoint(
            &agent,
            &job,
            &job_dir,
            &interruption_run,
            raw_result.as_deref(),
            timeout_continuations,
            continuations,
        )
        .await?
    } else {
        crate::continuation::load(&job_dir).await.ok()
    };
    if state.is_shutting_down()
        && current
            .as_ref()
            .is_some_and(|j| j.status == JOB_STATUS_RUNNING)
    {
        crate::continuation::interrupted(state, &job, agents::StopReason::ServiceInterrupted)
            .await?;
        record = crate::continuation::load(&job_dir).await.ok();
    }
    let has_checkpoint = record.as_ref().is_some_and(|r| r.eligible);
    if job.role == JOB_ROLE_VERIFIER && project.is_git() && !has_checkpoint {
        // The verifier workspace is disposable and must not leak into later work.
        let _guard = state.git_lock.lock().await;
        let _ = git::remove_worktree(&paths::project_repo(&workspace), &cwd).await;
    }
    if project.is_git()
        && !has_checkpoint
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
        if current_job.status == JOB_STATUS_CANCELED
            && current_job.failure_kind.as_deref() == Some("user_stopped")
        {
            // A forced stop remains canceled until the human chooses an action,
            // but its completed output/usage settlement must not disappear.
            let usage = run.usage.as_ref();
            sqlx::query("UPDATE jobs SET stdout = ?, stderr = ?, input_tokens = ?, output_tokens = ?, cache_read_tokens = ?, cache_write_tokens = ?, reasoning_tokens = ?, cost = ?, usage_known = ? WHERE id = ? AND status = 'canceled' AND failure_kind = 'user_stopped'")
                .bind(&run.stdout).bind(&run.stderr).bind(usage.and_then(|u| u.input_tokens)).bind(usage.and_then(|u| u.output_tokens))
                .bind(usage.and_then(|u| u.cache_read_tokens)).bind(usage.and_then(|u| u.cache_write_tokens)).bind(usage.and_then(|u| u.reasoning_tokens))
                .bind(usage.and_then(|u| u.cost)).bind(i64::from(usage.is_some_and(|u| u.known()))).bind(&job.id).execute(&state.db).await?;
        }
        return Ok(());
    }
    if let Some(task_id) = &job.task_id {
        if let Some(current_task) = load_task(&state.db, task_id).await? {
            if current_task.status.as_deref() == Some(STATUS_CANCELED) {
                return Ok(());
            }
        }
    }

    // An already produced valid outcome is applied/validated rather than
    // asking the model to repeat work after an exit/collection interruption.
    let process_ok = (run.exit_code == Some(0) && !run.timed_out && !run.stalled)
        || crate::continuation::valid_outcome(&job.role, raw_result.as_deref());
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
    } else if let Some(reported) = run.reported_error.clone() {
        (JOB_STATUS_FAILED, raw_result.clone(), Some(reported))
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
                    format!(
                        "结果协议不合法{}: {parse_error}",
                        if continuations >= MAX_CONTINUATIONS {
                            "，协议继续次数用尽"
                        } else {
                            ""
                        }
                    )
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
    if matches!(
        parsed,
        Some(
            ParsedResult::ExecutorDone { .. }
                | ParsedResult::ExecutorDerive { .. }
                | ParsedResult::DirectDone { .. }
        )
    ) {
        let validation =
            match crate::domain::commit_files(result_text.as_deref().unwrap_or_default()) {
                Ok(files) if project.is_git() => git::validate_commit_files(&cwd, &files)
                    .await
                    .map_err(|e| e.to_string()),
                Ok(_) => Ok(()),
                Err(error) => Err(error),
            };
        if let Err(reason) = validation {
            status = JOB_STATUS_FAILED;
            error = Some(format!(
                "结果协议不合法{}：{reason}",
                if continuations >= MAX_CONTINUATIONS {
                    "，协议继续次数用尽"
                } else {
                    ""
                }
            ));
            parsed = None;
        }
    }
    if status == JOB_STATUS_SUCCEEDED && !review {
        if let Some(value) = result_text
            .as_deref()
            .and_then(crate::domain::extract_json)
            .and_then(|v| v.get("shared_updates").cloned())
        {
            let outcome = match serde_json::from_value::<Vec<crate::resources::SharedInput>>(value)
            {
                Ok(records) => {
                    crate::resources::publish(
                        state,
                        &project,
                        job.task_id.as_deref(),
                        Some(&job.id),
                        &records,
                    )
                    .await
                }
                Err(error) => Err(error.into()),
            };
            if let Err(reason) = outcome {
                status = JOB_STATUS_FAILED;
                parsed = None;
                error = Some(format!(
                    "结果协议不合法，协议继续次数用尽：共享记录提交失败 {reason:#}"
                ));
            }
        }
    }
    if status == JOB_STATUS_SUCCEEDED && job.role == JOB_ROLE_VERIFIER {
        if let Some(raw) = result_text.as_deref() {
            if let Err(reason) = crate::delivery::validate_evidence(
                raw,
                &contract,
                &prompt_data
                    .task
                    .as_ref()
                    .map(|task| task.acceptance.clone())
                    .unwrap_or_default(),
                &artifact_version,
            ) {
                status = JOB_STATUS_FAILED;
                parsed = None;
                error = Some(format!("结果协议不合法，协议继续次数用尽：{reason}"));
            }
        }
    }
    if status == JOB_STATUS_SUCCEEDED && job.role == JOB_ROLE_ACCEPTANCE {
        if let Some(raw) = result_text.as_deref() {
            if let Err(reason) = crate::delivery::validate_project_evidence(
                raw,
                &contract,
                &prompt_data.acceptance_criteria,
                &artifact_version,
            ) {
                status = JOB_STATUS_FAILED;
                parsed = None;
                error = Some(format!("结果协议不合法，协议继续次数用尽：{reason}"));
            }
        }
    }
    if review {
        if status == JOB_STATUS_SUCCEEDED {
            if let Err(reason) = crate::replan_review::validate_result(
                &state.db,
                &job,
                result_text.as_deref().unwrap_or_default(),
            )
            .await
            {
                status = JOB_STATUS_FAILED;
                parsed = None;
                error = Some(format!("重规划决策结果不合法：{reason}"));
            }
        }
        let _guard = state.schedule_lock.lock().await;
        if finish_job(
            state,
            &job,
            status,
            result_text,
            &run,
            error.as_deref(),
            parsed.clone(),
        )
        .await?
        {
            crate::replan_review::settle_locked(
                state,
                &job,
                parsed.as_ref(),
                error.as_deref(),
                false,
            )
            .await?;
        }
        return Ok(());
    }
    if status == JOB_STATUS_FAILED
        && run.process_failure()
        && !crate::continuation::valid_outcome(&job.role, result_text.as_deref())
        && record.as_ref().is_some_and(|r| !r.session.is_empty())
        && !review
    {
        let reason = error.as_deref().unwrap_or("Agent 异常中断");
        let kind = record
            .as_ref()
            .and_then(|r| r.diagnostic.failure_kind())
            .unwrap_or(crate::domain::FailureKind::Transient);
        self::failure::apply_agent_failure(state, &agent, reason, kind).await?;
        if let Some(delay) = record
            .as_ref()
            .and_then(|r| r.diagnostic.retry_after_ms)
            .filter(|ms| *ms > 0)
        {
            let until = chrono::Duration::try_milliseconds(delay)
                .and_then(|duration| chrono::Utc::now().checked_add_signed(duration))
                .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC)
                .to_rfc3339();
            sqlx::query("UPDATE agents SET cooldown_until = CASE WHEN cooldown_until IS NULL OR cooldown_until < ? THEN ? ELSE cooldown_until END WHERE id = ?")
                .bind(&until).bind(&until).bind(&agent.id).execute(&state.db).await?;
        }
        let _guard = state.schedule_lock.lock().await;
        if finish_job(
            state,
            &job,
            status,
            result_text.clone(),
            &run,
            error.as_deref(),
            None,
        )
        .await?
        {
            crate::continuation::block_interrupted(state, &job, reason).await?;
        }
        return Ok(());
    }
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
                git::commit_files(
                    &cwd,
                    &message,
                    &crate::domain::commit_files(result_text.as_deref().unwrap_or_default())
                        .map_err(anyhow::Error::msg)?,
                )
                .await
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
            if let Err(error) =
                crate::resources::workspace_settle(state, &project, &job, &cwd, &artifact_version)
                    .await
            {
                finish_job(
                    state,
                    &job,
                    JOB_STATUS_FAILED,
                    result_text,
                    &run,
                    Some(&format!("工作区交付检查未通过：{error:#}")),
                    None,
                )
                .await?;
                crate::resources::block_workspace(
                    state,
                    &project,
                    &job,
                    &format!("工作区交付检查未通过：{error:#}"),
                )
                .await?;
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
                // Persist the request before settling its source job so crash
                // recovery cannot lose an accepted request between callbacks.
                crate::replan_review::submit(state, &job, &reason).await?;
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
                    git::commit_files(
                        &cwd,
                        &message,
                        &crate::domain::commit_files(result_text.as_deref().unwrap_or_default())
                            .map_err(anyhow::Error::msg)?,
                    )
                    .await
                } else {
                    Ok(false)
                };
                if let Err(commit_error) = commit {
                    let error_text = format!("提交执行结果失败: {commit_error:#}");
                    let transition = handle_executor_failure(
                        state,
                        &project,
                        task,
                        &error_text,
                        &agent,
                        domain::FailureKind::Task,
                    )
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
                if let Err(error) = crate::resources::workspace_settle(
                    state,
                    &project,
                    &job,
                    &cwd,
                    &artifact_version,
                )
                .await
                {
                    finish_job(
                        state,
                        &job,
                        JOB_STATUS_FAILED,
                        result_text,
                        &run,
                        Some(&format!("工作区交付检查未通过：{error:#}")),
                        None,
                    )
                    .await?;
                    crate::resources::block_workspace(
                        state,
                        &project,
                        &job,
                        &format!("工作区交付检查未通过：{error:#}"),
                    )
                    .await?;
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
                    git::commit_files(
                        &cwd,
                        &message,
                        &crate::domain::commit_files(result_text.as_deref().unwrap_or_default())
                            .map_err(anyhow::Error::msg)?,
                    )
                    .await
                } else {
                    Ok(false)
                };
                if let Err(commit_error) = commit {
                    let error_text = format!("提交执行结果失败: {commit_error:#}");
                    let transition = handle_executor_failure(
                        state,
                        &project,
                        task,
                        &error_text,
                        &agent,
                        domain::FailureKind::Task,
                    )
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
                if let Err(error) = crate::resources::workspace_settle(
                    state,
                    &project,
                    &job,
                    &cwd,
                    &artifact_version,
                )
                .await
                {
                    finish_job(
                        state,
                        &job,
                        JOB_STATUS_FAILED,
                        result_text,
                        &run,
                        Some(&format!("工作区交付检查未通过：{error:#}")),
                        None,
                    )
                    .await?;
                    crate::resources::block_workspace(
                        state,
                        &project,
                        &job,
                        &format!("工作区交付检查未通过：{error:#}"),
                    )
                    .await?;
                    return Ok(());
                }
                let transition =
                    handle_executor_success(state, &project, task, &summary, &agent).await;
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
                let kind = domain::classify_job_failure(
                    &job.role,
                    result_text.as_deref(),
                    reason,
                    run.process_failure(),
                );
                handle_executor_failure(state, &project, task, reason, &agent, kind).await
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
