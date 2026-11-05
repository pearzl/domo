use anyhow::Result;

use crate::agents;
use crate::domain::{self, now, ParsedResult};
use crate::events::{emit, emit_job};
use crate::models::*;
use crate::state::AppState;

use super::analysis::handle_analysis_failure;
use super::common::{load_job, load_project, load_task, role_label};
use super::direct::handle_direct_failure;
use super::executor::handle_executor_failure;
use super::project::{handle_acceptance_failure, handle_summary_failure};
use super::verifier::handle_verifier_failure;

pub(super) async fn finish_job(
    state: &AppState,
    job: &JobRow,
    status: &str,
    result: Option<String>,
    run: &agents::AgentRun,
    error: Option<&str>,
    _parsed: Option<ParsedResult>,
) -> Result<bool> {
    let diagnostic = crate::continuation::diagnostic(state, job).await;
    let failure_kind = if status == JOB_STATUS_FAILED {
        error.map(|reason| {
            let kind = if run.process_failure() {
                diagnostic.as_ref().and_then(|d| d.failure_kind())
            } else {
                None
            };
            kind.unwrap_or_else(|| domain::classify_failure(reason, run.process_failure()))
                .as_str()
        })
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

pub(super) async fn fail_runtime(state: &AppState, job_id: &str, reason: &str) -> Result<()> {
    let Some(job) = load_job(&state.db, job_id).await? else {
        return Ok(());
    };
    if job.status != JOB_STATUS_RUNNING {
        return Ok(());
    }
    if job.role == "merge_conflict" {
        sqlx::query("UPDATE merge_requests SET state = 'failed', error = ? WHERE job_id = ?")
            .bind(reason)
            .bind(job_id)
            .execute(&state.db)
            .await?;
    }
    let finished_at = now();
    let duration_ms = job
        .started_at
        .as_deref()
        .and_then(|started| crate::usage::elapsed_ms(Some(started), &finished_at));
    let project_id = job.project_id.clone();
    let resumable =
        crate::continuation::interrupted(state, &job, agents::StopReason::LaunchError).await?;
    let job_agent = match &job.agent_id {
        Some(agent_id) => crate::db::load_agent(&state.db, agent_id).await?,
        None => None,
    };
    if let Some(delay) = std::env::var("DOMO_MOCK_FAILURE_CALLBACK_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
    }
    let Some(current_job) = load_job(&state.db, job_id).await? else {
        return Ok(());
    };
    if current_job.status != JOB_STATUS_RUNNING {
        return Ok(());
    }
    if let Some(current_project) = load_project(&state.db, &project_id).await? {
        if current_project.goal_locked_at.is_some()
            && current_project.replan_generation != job.replan_generation
        {
            mark_canceled_job(state, &job, "项目代际已变化，取消过期运行时结算").await?;
            return Ok(());
        }
        // Only the analysis job belongs to the replan draft itself. Execution,
        // acceptance and summary jobs keep that generation after it is applied.
        if job.role == JOB_ROLE_ANALYSIS
            && job.replan_generation > 0
            && current_project.replan_active == 0
        {
            mark_canceled_job(state, &job, "重规划已结束，取消过期运行时结算").await?;
            return Ok(());
        }
    }
    let transition: Result<()> = async {
        if let Some(project) = load_project(&state.db, &project_id).await? {
            if let Some(task_id) = &job.task_id {
                if let Some(task) = load_task(&state.db, task_id).await? {
                    let agent = job_agent.as_ref();
                    match job.role.as_str() {
                        JOB_ROLE_EXECUTOR => {
                            if let Some(agent) = agent {
                                handle_executor_failure(
                                    state, &project, &task, reason, agent, true,
                                )
                                .await?;
                            } else {
                                block_task(state, &project, &task, reason).await?;
                            }
                        }
                        JOB_ROLE_VERIFIER => {
                            if let Some(agent) = agent {
                                handle_verifier_failure(
                                    state, &project, &task, reason, agent, true,
                                )
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
            } else if job.role == JOB_ROLE_ACCEPTANCE {
                handle_acceptance_failure(state, &project, reason, job_agent.as_ref(), true)
                    .await?;
            } else if job.role == JOB_ROLE_SUMMARY {
                handle_summary_failure(state, &project, reason, job_agent.as_ref(), true).await?;
            }
        }
        Ok(())
    }
    .await;
    if resumable {
        let _guard = state.schedule_lock.lock().await;
        crate::continuation::block_interrupted(state, &job, reason).await?;
    }
    // The running row keeps replan's quiet handoff closed until business callbacks settle.
    let updated = sqlx::query(
        "UPDATE jobs SET status = ?, error = ?, duration_ms = COALESCE(duration_ms, ?), \
         finished_at = ? WHERE id = ? AND status = 'running'",
    )
    .bind(JOB_STATUS_FAILED)
    .bind(reason)
    .bind(duration_ms)
    .bind(&finished_at)
    .bind(job_id)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Ok(());
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
    transition?;
    Ok(())
}

pub(super) async fn block_task(
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

pub(super) async fn mark_archived_job(state: &AppState, job: &JobRow) -> Result<()> {
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

pub(super) async fn mark_canceled_job(state: &AppState, job: &JobRow, reason: &str) -> Result<()> {
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
