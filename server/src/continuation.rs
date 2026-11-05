//! Continuation is an attempt to resume work, not restore an immutable snapshot.
//! Diagnostics authorize automation only with reliable evidence; humans can
//! attempt recovery after unknown exits or after repairing external conditions.
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::{agents, domain, models::*, paths, state::AppState};

pub const MAX_CONTINUES: u64 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub session: String,
    pub agent_kind: String,
    pub turns: u64,
    pub protocol_rounds: usize,
    pub eligible: bool,
    pub diagnostic: agents::Diagnosis,
}

pub async fn load(dir: &Path) -> Result<Checkpoint> {
    Ok(serde_json::from_slice(
        &tokio::fs::read(dir.join("continuation.json")).await?,
    )?)
}

pub async fn save(dir: &Path, record: &Checkpoint) -> Result<()> {
    // Readers fail closed on an incomplete write. Keep the diagnostic and
    // session durable even when the process ends outside the normal runner.
    let bytes = serde_json::to_vec(record)?;
    tokio::fs::write(dir.join("continuation.json"), &bytes).await?;
    if record.diagnostic.stop != agents::StopReason::Completed
        && record.diagnostic.stop != agents::StopReason::ServiceInterrupted
    {
        tokio::fs::write(
            dir.join(format!(
                "diagnostic-{}-{}.json",
                record.turns, record.protocol_rounds
            )),
            &bytes,
        )
        .await?;
    }
    Ok(())
}

pub fn valid_outcome(role: &str, raw: Option<&str>) -> bool {
    raw.is_some_and(|raw| {
        let complete = match domain::parse_result(role, raw) {
            Ok(
                domain::ParsedResult::ExecutorDone { .. }
                | domain::ParsedResult::ExecutorDerive { .. }
                | domain::ParsedResult::DirectDone { .. },
            ) => domain::commit_files(raw).is_ok(),
            Ok(domain::ParsedResult::Verdict { .. }) => {
                domain::extract_json(raw).is_some_and(|v| v["evidence"].is_object())
            }
            Ok(_) => true,
            Err(_) => false,
        };
        complete
            || domain::extract_json(raw).is_some_and(|v| {
                v["kind"] == "failed" && v["reason"].as_str().is_some_and(|s| !s.trim().is_empty())
            })
    })
}

/// Record every completed process round, including non-resumable diagnostics.
/// Only interruptions without a valid outcome or exhausted protocol correction
/// remain eligible; file/config equality is not a condition.
pub async fn checkpoint(
    agent: &AgentRow,
    job: &JobRow,
    dir: &Path,
    run: &agents::AgentRun,
    raw: Option<&str>,
    turns: u64,
    protocol_rounds: usize,
) -> Result<Option<Checkpoint>> {
    let previous = load(dir).await.ok();
    let session = agents::recover_session(
        agent,
        &job.id,
        &run.stdout,
        run.session_id
            .as_deref()
            .or_else(|| previous.as_ref().map(|r| r.session.as_str())),
    )
    .unwrap_or_default();
    let diagnostic = agents::diagnose(agent, run);
    let eligible = !session.is_empty()
        && agents::supports_resume(agent)
        && run.process_failure()
        && !valid_outcome(&job.role, raw)
        && protocol_rounds < 5
        && !diagnostic.session_unavailable();
    let record = Checkpoint {
        session,
        agent_kind: agent.kind.clone(),
        turns,
        protocol_rounds,
        eligible,
        diagnostic,
    };
    save(dir, &record).await?;
    tokio::fs::write(
        dir.join(format!("diagnostic-{}-{}.json", turns, protocol_rounds)),
        serde_json::to_vec(&record)?,
    )
    .await?;
    Ok(Some(record))
}

/// Establish a durable session intent before launch. Built-in adapters recover
/// their actual id from output; this also preserves a previous resumed session.
pub async fn starting(
    agent: &AgentRow,
    job: &JobRow,
    dir: &Path,
    session: Option<&str>,
    turns: u64,
    protocol_rounds: usize,
) -> Result<()> {
    if let Some(session) = agents::recover_session(agent, &job.id, "", session) {
        save(
            dir,
            &Checkpoint {
                session,
                agent_kind: agent.kind.clone(),
                turns,
                protocol_rounds,
                eligible: true,
                diagnostic: agents::Diagnosis::stopped(agents::StopReason::ServiceInterrupted),
            },
        )
        .await?;
    }
    Ok(())
}

/// Recover process-loss evidence from durable logs, including CLI-assigned ids.
/// Called after service restart, explicit stop and infrastructure launch errors.
pub async fn interrupted(state: &AppState, job: &JobRow, stop: agents::StopReason) -> Result<bool> {
    let Some(agent) =
        crate::db::load_agent(&state.db, job.agent_id.as_deref().unwrap_or("")).await?
    else {
        return Ok(false);
    };
    let workspace: String = sqlx::query_scalar("SELECT workspace FROM projects WHERE id = ?")
        .bind(&job.project_id)
        .fetch_one(&state.db)
        .await?;
    let dir = paths::job_dir(&PathBuf::from(workspace), &job.id);
    let previous = load(&dir).await.ok();
    let stdout = tokio::fs::read_to_string(dir.join("stdout.log"))
        .await
        .unwrap_or_default();
    let Some(session) = agents::recover_session(
        &agent,
        &job.id,
        &stdout,
        previous.as_ref().map(|r| r.session.as_str()),
    ) else {
        return Ok(false);
    };
    let raw = tokio::fs::read_to_string(dir.join("result.json"))
        .await
        .ok();
    let record = Checkpoint {
        session,
        agent_kind: agent.kind,
        turns: previous.as_ref().map_or(0, |r| r.turns),
        protocol_rounds: previous.as_ref().map_or(0, |r| r.protocol_rounds),
        eligible: !valid_outcome(&job.role, raw.as_deref())
            && previous.as_ref().is_none_or(|r| r.protocol_rounds < 5),
        diagnostic: agents::Diagnosis::stopped(stop),
    };
    save(&dir, &record).await?;
    tokio::fs::write(
        dir.join(format!(
            "diagnostic-{}-{}.json",
            record.turns, record.protocol_rounds
        )),
        serde_json::to_vec(&record)?,
    )
    .await?;
    Ok(record.eligible)
}

/// Only lifecycle and dispatch authorization are checked here. The stopped
/// work may have different files, configuration or inputs when it resumes.
pub async fn validate(state: &AppState, job: &JobRow) -> Result<Checkpoint> {
    if job.status != JOB_STATUS_FAILED
        && !(job.status == JOB_STATUS_CANCELED
            && job.failure_kind.as_deref() == Some("user_stopped"))
        && !(job.status == JOB_STATUS_RUNNING && job.resume != 0)
    {
        bail!("只有尚未被替代的中断作业可以继续");
    }
    let project = crate::replan::load_project(&state.db, &job.project_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("项目不存在"))?;
    if matches!(
        project.status.as_deref(),
        Some(STATUS_ARCHIVED | STATUS_PAUSED | STATUS_PAUSING)
    ) || project.stage == STAGE_COMPLETED
        || project.replan_active != 0
        || project.replan_generation != job.replan_generation
        || project.contract_id != job.contract_id
    {
        bail!("项目已停止或作业不再属于当前有效计划");
    }
    if matches!(job.role.as_str(), JOB_ROLE_REPLAN_REVIEW | "merge_conflict") {
        bail!("控制作业使用独立处置流程");
    }
    let dir = paths::job_dir(&PathBuf::from(&project.workspace), &job.id);
    if job.status != JOB_STATUS_RUNNING && state.job_active(&dir) {
        bail!("上一轮输出与费用仍在结算，请稍后继续");
    }
    let record = load(&dir).await?;
    if !record.eligible || record.diagnostic.session_unavailable() || record.protocol_rounds >= 5 {
        bail!("上次结果需要新会话或业务处置，不能继续");
    }
    if record.turns >= MAX_CONTINUES {
        bail!("会话继续额度耗尽");
    }
    if agents::job_process_alive(&dir) {
        bail!("原作业进程尚未停止");
    }
    let agent = crate::db::load_agent(&state.db, job.agent_id.as_deref().unwrap_or(""))
        .await?
        .ok_or_else(|| anyhow::anyhow!("Agent 不存在"))?;
    if record.agent_kind != agent.kind || !agents::supports_resume(&agent) {
        bail!("当前适配器无法恢复原会话，请新会话执行");
    }
    let role = if matches!(job.role.as_str(), ROLE_EXECUTOR | ROLE_VERIFIER) {
        job.role.as_str()
    } else {
        ROLE_PLANNER
    };
    if agent.enabled == 0 || !parse_string_list(&agent.roles).iter().any(|r| r == role) {
        bail!("Agent 尚未启用或不具备所需角色");
    }
    let latest: Option<String> = sqlx::query_scalar("SELECT id FROM jobs WHERE project_id = ? AND task_id IS ? AND role = ? ORDER BY rowid DESC LIMIT 1")
        .bind(&job.project_id).bind(&job.task_id).bind(&job.role).fetch_optional(&state.db).await?;
    if latest.as_deref() != Some(&job.id) {
        bail!("作业已有后续尝试");
    }
    let running: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE project_id = ? AND (task_id IS ? OR ? = 0) AND id != ? AND status = 'running'")
        .bind(&job.project_id).bind(&job.task_id).bind(i64::from(project.is_git())).bind(&job.id).fetch_one(&state.db).await?;
    if running > 0 {
        bail!("已有其他作业运行");
    }
    if let Some(id) = &job.task_id {
        let task = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
        if matches!(
            task.status.as_deref(),
            Some(STATUS_CANCELED | STATUS_PAUSED | STATUS_PAUSING)
        ) || task.stage == TASK_STAGE_COMPLETED
            || task.human_gate.is_some()
            || (task.stage == TASK_STAGE_VERIFYING) != (job.role == ROLE_VERIFIER)
            || crate::replan_review::active(&state.db, id).await?.is_some()
        {
            bail!("任务状态不允许继续");
        }
        let member: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_agents t WHERE t.task_id = ? AND t.role = ? AND t.agent_id = ? AND t.enabled = 1 AND NOT EXISTS (SELECT 1 FROM project_agents p WHERE p.project_id = ? AND p.role = t.role AND p.agent_id = t.agent_id AND p.enabled = 0)")
            .bind(id).bind(role).bind(&agent.id).bind(&project.id).fetch_one(&state.db).await?;
        if member == 0 {
            bail!("原 Agent 不在有效任务池中");
        }
    } else {
        let stage = match job.role.as_str() {
            JOB_ROLE_ANALYSIS => STAGE_ANALYSIS,
            JOB_ROLE_DIRECT => STAGE_EXECUTING,
            JOB_ROLE_ACCEPTANCE | JOB_ROLE_SUMMARY => STAGE_SUMMARIZING,
            _ => "",
        };
        if project.stage != stage
            || project.report.is_some()
            || (job.role == JOB_ROLE_DIRECT && project.completion_mode != COMPLETION_DIRECT)
            || (job.role == JOB_ROLE_ACCEPTANCE && project.acceptance_passed != 0)
            || (job.role == JOB_ROLE_SUMMARY
                && project.acceptance_passed == 0
                && project.completion_mode != COMPLETION_DIRECT)
        {
            bail!("项目阶段不允许继续");
        }
        let member: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_agents WHERE project_id = ? AND role = ? AND agent_id = ? AND enabled = 1")
            .bind(&project.id).bind(ROLE_PLANNER).bind(&agent.id).fetch_one(&state.db).await?;
        if member == 0 {
            bail!("原 Agent 不在有效项目池中");
        }
    }
    // A valid outcome awaits collection/settlement, not another model turn.
    let raw = tokio::fs::read_to_string(dir.join("result.json"))
        .await
        .ok();
    if valid_outcome(&job.role, raw.as_deref()) {
        bail!("已有业务结果，请处理结果或结算，不重复执行模型");
    }
    let reserved = job.status == JOB_STATUS_RUNNING && job.resume != 0;
    if let Some(reason) = crate::delivery::gate_with_reserved_attempt(
        &state.db,
        &project.id,
        job.task_id.as_deref().filter(|_| job.role == ROLE_EXECUTOR),
        reserved,
    )
    .await?
    {
        bail!("{reason}");
    }
    Ok(record)
}

pub async fn can_continue(state: &AppState, job: &JobRow) -> bool {
    if job.status == JOB_STATUS_RUNNING {
        return false;
    }
    let pending: i64 = sqlx::query_scalar("SELECT (SELECT COUNT(*) FROM retry_requests WHERE job_id = ?) + (SELECT COUNT(*) FROM project_continuations WHERE job_id = ?)")
        .bind(&job.id).bind(&job.id).fetch_one(&state.db).await.unwrap_or(1);
    pending == 0 && validate(state, job).await.is_ok()
}

pub async fn diagnostic(state: &AppState, job: &JobRow) -> Option<agents::Diagnosis> {
    let workspace: String = sqlx::query_scalar("SELECT workspace FROM projects WHERE id = ?")
        .bind(&job.project_id)
        .fetch_one(&state.db)
        .await
        .ok()?;
    load(&paths::job_dir(&PathBuf::from(workspace), &job.id))
        .await
        .ok()
        .map(|r| r.diagnostic)
}

/// Pausing does not choose how interrupted work resumes. Keep a resumable
/// stopped job available for explicit human choice after lifting the pause.
pub async fn pending_manual(
    state: &AppState,
    project: &ProjectRow,
    task_id: Option<&str>,
) -> Result<bool> {
    let job = sqlx::query_as::<_, JobRow>(
        "SELECT * FROM jobs WHERE project_id = ? AND task_id IS ? ORDER BY rowid DESC LIMIT 1",
    )
    .bind(&project.id)
    .bind(task_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(job) = job else {
        return Ok(false);
    };
    if job.status != JOB_STATUS_FAILED
        && !(job.status == JOB_STATUS_CANCELED
            && job.failure_kind.as_deref() == Some("user_stopped"))
    {
        return Ok(false);
    }
    Ok(
        load(&paths::job_dir(&PathBuf::from(&project.workspace), &job.id))
            .await
            .is_ok_and(|r| r.eligible),
    )
}

/// A failed continuation remains visible for human choice instead of silently
/// starting a fresh session. Preserve pauses and terminal task states.
pub async fn block_interrupted(state: &AppState, job: &JobRow, reason: &str) -> Result<()> {
    if let Some(id) = &job.task_id {
        sqlx::query("UPDATE tasks SET status = 'blocked', last_error = ?, waiting_reason = '等待人工选择继续或新会话执行', updated_at = ? WHERE id = ? AND COALESCE(status, '') NOT IN ('canceled', 'paused', 'pausing') AND stage != 'completed'")
            .bind(reason).bind(domain::now()).bind(id).execute(&state.db).await?;
    } else {
        sqlx::query("UPDATE projects SET status = 'blocked', blocked_reason = ?, blocked_kind = 'continuation', updated_at = ? WHERE id = ? AND status NOT IN ('archived', 'paused', 'pausing') AND stage != 'completed'")
            .bind(reason).bind(domain::now()).bind(&job.project_id).execute(&state.db).await?;
    }
    Ok(())
}

pub async fn reject_started(state: &AppState, job: &JobRow, reason: &str) -> Result<()> {
    let reason = format!("继续未启动，请检查条件或选择新会话：{reason}");
    sqlx::query("UPDATE jobs SET status = 'failed', error = ?, failure_kind = 'continuation_invalid', finished_at = ?, resume = 0 WHERE id = ? AND status = 'running'")
        .bind(&reason).bind(domain::now()).bind(&job.id).execute(&state.db).await?;
    block_interrupted(state, job, &reason).await?;
    crate::events::emit_job(
        &state.db,
        &state.bus,
        Some(&job.project_id),
        job.task_id.as_deref(),
        Some(&job.id),
        "job_continuation_invalid",
        reason,
    )
    .await;
    Ok(())
}

/// Session ledgers are cumulative; archived runs already count toward budgets.
pub async fn usage_delta(
    state: &AppState,
    id: &str,
    current: Option<crate::usage::UsageRecord>,
) -> Result<Option<crate::usage::UsageRecord>> {
    let Some(mut current) = current else {
        return Ok(None);
    };
    type UsageRow = (
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<f64>,
        Option<i64>,
    );
    let rows: Vec<UsageRow> = sqlx::query_as("SELECT input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, cost, duration_ms FROM job_runs WHERE job_id = ?")
        .bind(id).fetch_all(&state.db).await?;
    fn subtract(left: &mut Option<i64>, right: Option<i64>) {
        if let (Some(left), Some(right)) = (left.as_mut(), right) {
            *left = left.saturating_sub(right).max(0);
        }
    }
    for (input, output, read, write, reasoning, cost, duration) in rows {
        subtract(&mut current.input_tokens, input);
        subtract(&mut current.output_tokens, output);
        subtract(&mut current.cache_read_tokens, read);
        subtract(&mut current.cache_write_tokens, write);
        subtract(&mut current.reasoning_tokens, reasoning);
        subtract(&mut current.duration_ms, duration);
        if let (Some(left), Some(right)) = (current.cost.as_mut(), cost) {
            *left = (*left - right).max(0.0);
        }
    }
    Ok(Some(current))
}
