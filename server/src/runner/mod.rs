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
mod merge;
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
use self::worktree::{ensure_worktree_root, prepare_task_worktree, prepare_verify_worktree};

/// Missing or invalid results get up to five correction rounds in the same job.
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
    if let Err(error) = crate::resources::workspace_preflight(state, &project, &job).await {
        let reason = format!("工作区检查未通过：{error:#}");
        crate::resources::block_workspace(state, &project, &job, &reason).await?;
        return Ok(());
    }
    if job.role == JOB_ROLE_DIRECT && project.is_git() {
        let _guard = state.git_lock.lock().await;
        git::reset_hard(&cwd).await?;
    }

    let prompt_data = build_prompt_data(state, &project, task.as_ref(), &job, &cwd).await?;
    let artifact_version = if project.is_git() {
        serde_json::json!(git::commit_of(&cwd, "HEAD").await?)
    } else {
        serde_json::Value::Null
    };
    let mut full_prompt = format!(
        "{}\n\n## 交付契约快照\n{}\n",
        prompts::render(&prompt_data),
        contract
    );
    let shared =
        crate::resources::shared_snapshot(state, &project, Some(&job), task.as_ref()).await?;
    full_prompt.push_str(&format!("\n## 项目共享信息（固定输入版本）\n{}\n只加载相关有效记录；运行中可通过给出的只读 API 按需读取其他主题或版本，不因新记录自动改变本轮输入。结果 JSON 可提交 shared_updates 数组，每项包含 topic、body、scope（任务 key 数组，空数组为全项目）、evidence（非空证据引用数组）、supersedes（被替代记录 ID）、conflicts（存在结论分歧的记录 ID）和 reason（替代/分歧原因及影响）。记录按主题追加到独立仓库，不能修改交付契约。不得静默覆盖结论；分歧保留双方依据，优先由相关任务核验，影响方案或契约时才请求规划者。记录提交无需规划者逐条审批。\n", shared));
    let artifacts = crate::resources::artifact_snapshot(state, &project).await?;
    full_prompt.push_str(&format!("\n## 输入与产物边界\n{}\n普通工作区必须保留原始输入，默认把新交付物写到 outputs/ 等独立位置。只能修改自己任务的产物以及 permissions 中明确授权给当前任务的路径，不能覆盖其他历史产物。需要未授权原地修改时报告失败并请求用户在产物区域登记修改对象及理由。若输入或产物外部变化或前次中断，必须先检查现场，不得把停止进程当作文件回滚。\n", artifacts));
    if job.role == JOB_ROLE_VERIFIER {
        full_prompt.push_str(&format!("\n## 必填验收证据\n结果必须含 evidence 对象：contract_id={}，artifact_version={}，artifacts 为已检查的产物路径数组，risks 和 human_checks 为未覆盖风险及人工检查项数组。criteria 按任务验收标准的原文与顺序逐项填写，每项包含 criterion、passed 布尔值和非空 checks 数组；每个检查包含 method（实际运行命令或检查方法）和 result（实际结果，不得编造）。无产物或风险时数组可为空。普通工作区不提供历史内容回溯，检查时间由服务端记录。\n", contract["id"], artifact_version));
    }
    if job.role == JOB_ROLE_ACCEPTANCE {
        full_prompt.push_str(&format!("\n项目验收结果必须包含 contract_id={} 和 artifact_version={}。criteria 必须按当前契约验收标准的原文与顺序逐项填写；判定 met 的条目必须附实际检查方法及结果，不得省略证据或只用任务已通过代替集成检查。\n", contract["id"], artifact_version));
    }
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
        if protocol_error.is_none() {
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
        let follow_up = format!(
            "{}\n请修正结果协议错误：{}\n请覆盖结果文件并提交完整有效结果。\n",
            prompts::continuation(&prompt_data),
            protocol_error.unwrap_or_default()
        );
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
                "{}作业结果协议不完整，已追加第 {} 轮继续提示",
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
    if status == JOB_STATUS_SUCCEEDED {
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
