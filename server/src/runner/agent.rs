use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::agents::{self, AgentSpec};
use crate::events::emit_job;
use crate::models::*;
use crate::state::AppState;

use super::common::{load_job, load_project, load_task, role_label};

/// A continuation is allowed only while the job is still running and neither
/// the project nor the task has been paused or archived.
pub(super) async fn continuation_allowed(
    state: &AppState,
    job: &JobRow,
    project: &ProjectRow,
    task: Option<&TaskRow>,
) -> Result<bool> {
    if state.is_shutting_down() {
        return Ok(false);
    }
    let Some(current_job) = load_job(&state.db, &job.id).await? else {
        return Ok(false);
    };
    if current_job.status != JOB_STATUS_RUNNING {
        return Ok(false);
    }
    let Some(current_project) = load_project(&state.db, &project.id).await? else {
        return Ok(false);
    };
    if matches!(
        current_project.status.as_deref(),
        Some(STATUS_PAUSING | STATUS_PAUSED | STATUS_ARCHIVED)
    ) {
        return Ok(false);
    }
    if let Some(task) = task {
        if let Some(current_task) = load_task(&state.db, &task.id).await? {
            if matches!(
                current_task.status.as_deref(),
                Some(STATUS_PAUSING | STATUS_PAUSED | STATUS_CANCELED)
            ) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Sums usage from the first run and the continuation for agents whose usage
/// comes from process output. Agents that report a session ledger replace this
/// total after the final run.
pub(super) fn merge_usage(
    mut base: Option<crate::usage::UsageRecord>,
    extra: Option<crate::usage::UsageRecord>,
) -> Option<crate::usage::UsageRecord> {
    let Some(extra) = extra else {
        return base;
    };
    let Some(base) = base.as_mut() else {
        return Some(extra);
    };
    fn add(left: Option<i64>, right: Option<i64>) -> Option<i64> {
        match (left, right) {
            (Some(left), Some(right)) => Some(left.saturating_add(right)),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        }
    }
    base.input_tokens = add(base.input_tokens, extra.input_tokens);
    base.output_tokens = add(base.output_tokens, extra.output_tokens);
    base.cache_read_tokens = add(base.cache_read_tokens, extra.cache_read_tokens);
    base.cache_write_tokens = add(base.cache_write_tokens, extra.cache_write_tokens);
    base.reasoning_tokens = add(base.reasoning_tokens, extra.reasoning_tokens);
    base.cost = match (base.cost, extra.cost) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    base.duration_ms = add(base.duration_ms, extra.duration_ms);
    Some(base.clone())
}

pub(super) async fn append_log_separator(job_dir: &Path) -> Result<()> {
    for name in ["stdout.log", "stderr.log"] {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(job_dir.join(name))
            .await?;
        file.write_all("\n--- 继续执行 ---\n".as_bytes()).await?;
        file.flush().await?;
    }
    Ok(())
}

/// Runs one agent process under two watchdogs. `timeout` bounds this run; the
/// stall watchdog ends a run whose stdout/stderr stopped growing for
/// `stall_timeout`. Both kill only the recorded process group, leaving other
/// jobs of the same agent untouched, and let the normal failure path apply.
pub(super) async fn run_agent_job(
    state: &AppState,
    job: &JobRow,
    job_dir: &Path,
    agent: &AgentRow,
    spec: &AgentSpec,
    progress: &agents::JobProgress,
    timeout: Duration,
) -> Result<agents::AgentRun> {
    let execution = agents::execute(agent, spec, timeout);
    tokio::pin!(execution);
    let deadline = tokio::time::sleep(timeout);
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
                    "Agent 作业卡死：连续 {idle_secs} 秒无 stdout/stderr 输出\
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
        run.stderr = format!("作业超时（{} 秒），进程已终止", timeout.as_secs());
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
pub(super) async fn wait_for_stall(progress: &agents::JobProgress, threshold: std::time::Duration) {
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
