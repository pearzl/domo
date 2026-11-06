//! Task owners decide and produce replacement plans in one read-only job.
//! A request alone never suspends unrelated execution. Only a complete plan
//! starts the affected scope's quiet handoff and eventual replacement.

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::domain::{new_id, now, parse_result, ParsedResult};
use crate::events::emit;
use crate::models::*;
use crate::state::AppState;

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct ReviewRequest {
    pub id: String,
    pub project_id: String,
    pub task_id: String,
    pub source_job_id: String,
    pub reviewer_task_id: Option<String>,
    pub generation: i64,
    pub reason: String,
    pub state: String,
    pub review_job_id: Option<String>,
    pub attempts: i64,
    pub guidance: String,
    pub origin: String,
    pub resume_stage: String,
}

pub async fn initialize(pool: &SqlitePool) -> crate::db::DbResult<()> {
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS replan_requests (
          id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
          task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
          source_job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id),
          reviewer_task_id TEXT REFERENCES tasks(id), generation INTEGER NOT NULL,
          reason TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending', review_job_id TEXT REFERENCES jobs(id),
           attempts INTEGER NOT NULL DEFAULT 0, guidance TEXT NOT NULL DEFAULT '',
           origin TEXT NOT NULL DEFAULT 'executor', resume_stage TEXT NOT NULL DEFAULT 'executing',
          created_at TEXT NOT NULL, updated_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_replan_request_active ON replan_requests(task_id)
          WHERE state IN ('pending', 'running', 'planned', 'ready', 'waiting');",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn active(pool: &SqlitePool, task_id: &str) -> Result<Option<ReviewRequest>> {
    Ok(sqlx::query_as("SELECT * FROM replan_requests WHERE task_id = ? AND state IN ('pending', 'running', 'planned', 'ready', 'waiting')")
        .bind(task_id).fetch_optional(pool).await?)
}

pub async fn submit(state: &AppState, job: &JobRow, reason: &str) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    submit_locked(state, job, reason, "executor", TASK_STAGE_EXECUTING).await
}

/// The verdict and its decision request share the scheduling lock.
pub async fn submit_locked(
    state: &AppState,
    job: &JobRow,
    reason: &str,
    origin: &str,
    resume_stage: &str,
) -> Result<()> {
    let Some(task_id) = job.task_id.as_deref() else {
        anyhow::bail!("重规划请求缺少任务")
    };
    let task = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
        .bind(task_id)
        .fetch_one(&state.db)
        .await?;
    let project = crate::replan::load_project(&state.db, &job.project_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("项目不存在"))?;
    if task.stage == TASK_STAGE_COMPLETED
        || task.status.as_deref() == Some(STATUS_CANCELED)
        || project.replan_active != 0
        || project.replan_generation != job.replan_generation
        || project.status.as_deref() == Some(STATUS_ARCHIVED)
    {
        return Ok(());
    }
    if active(&state.db, task_id).await?.is_some() {
        return Ok(());
    }
    let existing: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM replan_requests WHERE source_job_id = ?")
            .bind(&job.id)
            .fetch_one(&state.db)
            .await?;
    if existing > 0 {
        return Ok(());
    }
    let timestamp = now();
    let waiting = if task.parent_id.is_some() {
        "重规划审批：等待父任务执行者决策"
    } else {
        "重规划审批：等待项目规划者决策"
    };
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("INSERT INTO replan_requests (id, project_id, task_id, source_job_id, reviewer_task_id, generation, reason, created_at, updated_at, origin, resume_stage) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(new_id("rpr")).bind(&job.project_id).bind(task_id).bind(&job.id).bind(&task.parent_id)
        .bind(project.replan_generation).bind(reason).bind(&timestamp).bind(&timestamp).bind(origin).bind(resume_stage).execute(&mut *tx).await?;
    sqlx::query("UPDATE tasks SET status = CASE WHEN status IN ('paused', 'pausing') THEN status ELSE 'blocked' END, waiting_reason = ?, last_error = ?, updated_at = ? WHERE id = ?")
        .bind(waiting).bind(reason).bind(&timestamp).bind(task_id).execute(&mut *tx).await?;
    tx.commit().await?;
    emit(
        &state.db,
        &state.bus,
        Some(&job.project_id),
        Some(task_id),
        "replan_review_requested",
        format!("任务 {} 申请重规划，{}：{reason}", task.key, waiting),
    )
    .await;
    Ok(())
}

/// Scheduler holds the scheduling lock. A completed review is replayed after a
/// crash; interrupted reviews return to the queue without charging task retries.
pub async fn reconcile_locked(state: &AppState) -> Result<()> {
    sqlx::query("UPDATE replan_requests SET state = CASE WHEN state = 'ready' THEN 'applied' ELSE 'obsolete' END, updated_at = ? WHERE state IN ('pending', 'running', 'planned', 'ready', 'waiting') AND EXISTS (SELECT 1 FROM projects p JOIN tasks t ON t.project_id = p.id WHERE p.id = replan_requests.project_id AND t.id = replan_requests.task_id AND (p.status = 'archived' OR t.status = 'canceled' OR t.stage = 'completed'))")
        .bind(now()).execute(&state.db).await?;
    // A human may abandon a project replan. Its surviving tasks still need
    // their local decisions; rebase the requests, never reuse an old verdict.
    sqlx::query("UPDATE replan_requests SET generation = (SELECT replan_generation FROM projects WHERE id = project_id), state = 'pending', review_job_id = NULL, attempts = 0, updated_at = ? WHERE state IN ('pending', 'running', 'planned', 'ready', 'waiting') AND EXISTS (SELECT 1 FROM projects p WHERE p.id = replan_requests.project_id AND p.replan_active = 0 AND (p.replan_generation != replan_requests.generation OR replan_requests.state = 'ready'))")
        .bind(now()).execute(&state.db).await?;
    let finished = sqlx::query_as::<_, ReviewRequest>("SELECT r.* FROM replan_requests r JOIN jobs j ON j.id = r.review_job_id WHERE r.state = 'running' AND j.status != 'running'")
        .fetch_all(&state.db).await?;
    for request in finished {
        let job = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE id = ?")
            .bind(&request.review_job_id)
            .fetch_one(&state.db)
            .await?;
        let parsed = if job.status == JOB_STATUS_SUCCEEDED {
            job.result
                .as_deref()
                .and_then(|raw| parse_result(JOB_ROLE_REPLAN_REVIEW, raw).ok())
        } else {
            None
        };
        let interrupted = job.status == JOB_STATUS_CANCELED
            || job
                .error
                .as_deref()
                .is_some_and(|error| error.contains("服务重启") || error.contains("强制暂停"));
        settle_locked(
            state,
            &job,
            parsed.as_ref(),
            job.error.as_deref(),
            interrupted,
        )
        .await?;
    }
    sqlx::query("UPDATE projects SET status = 'running', blocked_reason = NULL, updated_at = ? WHERE status = 'blocked' AND stage = 'executing' AND replan_active = 0 AND EXISTS (SELECT 1 FROM replan_requests r WHERE r.project_id = projects.id AND r.state IN ('pending', 'planned'))")
        .bind(now()).execute(&state.db).await?;
    Ok(())
}

pub async fn pending(pool: &SqlitePool, project_id: &str) -> Result<Vec<ReviewRequest>> {
    Ok(sqlx::query_as("SELECT * FROM replan_requests WHERE project_id = ? AND state = 'pending' ORDER BY created_at, id")
        .bind(project_id).fetch_all(pool).await?)
}

/// A prepared parent plan holds only that family. A prepared project plan holds
/// all dispatch. In-flight jobs settle before replacement so their callbacks
/// cannot race cancellation or workspace removal.
pub async fn holds_task(pool: &SqlitePool, task: &TaskRow) -> Result<bool> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM replan_requests WHERE project_id = ? AND state = 'planned' AND (reviewer_task_id IS NULL OR reviewer_task_id = ? OR reviewer_task_id = ?)")
        .bind(&task.project_id).bind(&task.id).bind(&task.parent_id).fetch_one(pool).await?;
    Ok(count > 0)
}

/// Returns true when project dispatch belongs to a prepared project plan.
/// Local family replacements leave other project tasks eligible for dispatch.
pub async fn advance_plans_locked(state: &AppState, project: &ProjectRow) -> Result<bool> {
    let requests = sqlx::query_as::<_, ReviewRequest>("SELECT * FROM replan_requests WHERE project_id = ? AND state IN ('planned', 'ready') ORDER BY reviewer_task_id IS NOT NULL, created_at, id")
        .bind(&project.id).fetch_all(&state.db).await?;
    for request in requests {
        if request.reviewer_task_id.is_none() {
            if request.state == "ready" {
                // Replay only the interrupted automatic application. A manual
                // draft remains blocked and is handled by the confirmation API.
                let proposal = load_plan(&state.db, &request).await?;
                if project.replan_active != 0
                    && proposal.skip_confirmation
                    && project.confirm_mode == CONFIRM_AUTO
                {
                    apply_project_plan_locked(state, project, &request).await?;
                    return Ok(true);
                }
                continue;
            }
            if project.replan_active == 0 && project.stage != STAGE_EXECUTING {
                continue;
            }
            if project.replan_active != 0
                && project
                    .replan_reason
                    .as_deref()
                    .is_none_or(|reason| !reason.starts_with(&format!("任务请求 {}：", request.id)))
            {
                continue;
            }
            if crate::replan::running_non_analysis_jobs(&state.db, &project.id).await? > 0 {
                return Ok(true);
            }
            apply_project_plan_locked(state, project, &request).await?;
            return Ok(true);
        }
        if project.replan_active == 0 && request.generation == project.replan_generation {
            apply_family_plan_locked(state, project, &request).await?;
        }
    }
    Ok(false)
}

async fn load_plan(pool: &SqlitePool, request: &ReviewRequest) -> Result<AnalysisProposal> {
    let raw: String = sqlx::query_scalar("SELECT result FROM jobs WHERE id = ?")
        .bind(&request.review_job_id)
        .fetch_one(pool)
        .await?;
    let ParsedResult::AnalysisPlan {
        tasks,
        summary,
        goal,
        assumptions,
        acceptance,
        skip_confirmation,
    } = parse_result(JOB_ROLE_REPLAN_REVIEW, &raw).map_err(anyhow::Error::msg)?
    else {
        anyhow::bail!("重规划结果不是完整方案")
    };
    let tasks = tasks
        .into_iter()
        .map(|task| ProposalTask {
            key: task.key,
            title: task.title,
            description: task.description,
            acceptance: task.acceptance,
            deps: task.deps,
        })
        .collect();
    Ok(AnalysisProposal {
        kind: "plan".into(),
        summary,
        goal,
        assumptions,
        acceptance,
        tasks,
        plan: vec![],
        verification: vec![],
        skip_confirmation,
        replan_impact: None,
    })
}

/// Validate the decision against persisted scope before accepting the protocol.
/// The same check runs during recovery, not just inside the agent continuation.
pub async fn validate_result(pool: &SqlitePool, job: &JobRow, raw: &str) -> Result<(), String> {
    let parsed = parse_result(JOB_ROLE_REPLAN_REVIEW, raw)?;
    let ParsedResult::AnalysisPlan { goal, tasks, .. } = parsed else {
        return Ok(());
    };
    let request =
        sqlx::query_as::<_, ReviewRequest>("SELECT * FROM replan_requests WHERE review_job_id = ?")
            .bind(&job.id)
            .fetch_optional(pool)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "重规划请求已失效".to_string())?;
    let project = crate::replan::load_project(pool, &job.project_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "项目不存在".to_string())?;
    if goal
        .as_deref()
        .is_some_and(|goal| goal.trim() != project.goal.trim())
    {
        return Err("重规划不能修改当前项目目标".into());
    }
    if request.reviewer_task_id.is_some() {
        crate::domain::validate_derive_scope(false, &std::collections::HashSet::new(), &tasks)?;
    }
    if let Some(parent_id) = request.reviewer_task_id {
        let external: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks t JOIN tasks child ON child.project_id = t.project_id WHERE child.parent_id = ? AND child.stage != 'completed' AND COALESCE(child.status, '') != 'canceled' AND t.id != ? AND COALESCE(t.parent_id, '') != ? AND COALESCE(t.status, '') != 'canceled' AND EXISTS (SELECT 1 FROM json_each(t.deps) WHERE value = child.key)")
            .bind(&parent_id).bind(&parent_id).bind(&parent_id).fetch_one(pool).await.map_err(|error| error.to_string())?;
        if external > 0 {
            return Err(
                "子任务被父任务范围外的任务依赖，不能局部替换；请返回等待并说明需协调的依赖".into(),
            );
        }
    }
    Ok(())
}

async fn apply_project_plan_locked(
    state: &AppState,
    project: &ProjectRow,
    request: &ReviewRequest,
) -> Result<()> {
    let mut proposal = load_plan(&state.db, request).await?;
    let tasks = proposal
        .tasks
        .iter()
        .map(|task| crate::domain::NormalizedTask {
            key: task.key.clone(),
            title: task.title.clone(),
            description: task.description.clone(),
            acceptance: task.acceptance.clone(),
            deps: task.deps.clone(),
        })
        .collect::<Vec<_>>();
    if project.replan_active == 0 {
        let reason = format!("任务请求 {}：{}", request.id, request.reason);
        if crate::replan::request_locked(
            state,
            &project.id,
            &reason,
            REPLAN_SOURCE_EXECUTOR,
            None,
            None,
        )
        .await?
        .is_none()
        {
            sqlx::query(
                "UPDATE replan_requests SET state = 'waiting', updated_at = ? WHERE id = ?",
            )
            .bind(now())
            .bind(&request.id)
            .execute(&state.db)
            .await?;
            set_waiting(
                state,
                request,
                "替代方案已生成，但项目状态或消耗限制不允许应用；处理后重试",
            )
            .await?;
            return Ok(());
        }
    }
    let current = crate::replan::load_project(&state.db, &project.id)
        .await?
        .unwrap();
    proposal.goal = Some(current.goal.clone());
    if proposal.acceptance.is_empty() {
        proposal.acceptance = current
            .proposal
            .as_deref()
            .and_then(|raw| serde_json::from_str::<AnalysisProposal>(raw).ok())
            .map(|previous| previous.acceptance)
            .unwrap_or_default();
    }
    proposal.replan_impact = Some(
        crate::replan::impact(state, &current, "plan", &proposal.acceptance, &[], &tasks).await?,
    );
    let raw = serde_json::to_string(&proposal)?;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("UPDATE projects SET replan_proposal = ?, updated_at = ? WHERE id = ?")
        .bind(&raw)
        .bind(now())
        .bind(&current.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE replan_requests SET state = 'ready', generation = ?, updated_at = ? WHERE id = ?",
    )
    .bind(current.replan_generation)
    .bind(now())
    .bind(&request.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if proposal.skip_confirmation && current.confirm_mode == CONFIRM_AUTO {
        crate::runner::apply_analysis_plan_locked(
            state,
            &current,
            &current.goal,
            &tasks,
            &proposal.summary,
            &raw,
        )
        .await?;
        sqlx::query("UPDATE replan_requests SET state = 'applied', updated_at = ? WHERE id = ?")
            .bind(now())
            .bind(&request.id)
            .execute(&state.db)
            .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            None,
            "replan_applied",
            "规划者在决策作业中生成的替代方案已自动应用",
        )
        .await;
    } else {
        crate::runner::insert_message(&state.db, &project.id, "planner", &raw).await?;
        sqlx::query("UPDATE projects SET status = 'blocked', blocked_reason = NULL WHERE id = ?")
            .bind(&project.id)
            .execute(&state.db)
            .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            None,
            "analysis_ready",
            "重规划决策与替代方案已生成，等待人工确认",
        )
        .await;
    }
    Ok(())
}

async fn apply_family_plan_locked(
    state: &AppState,
    project: &ProjectRow,
    request: &ReviewRequest,
) -> Result<()> {
    let current: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM replan_requests r JOIN tasks t ON t.id = r.task_id WHERE r.id = ? AND r.state = 'planned' AND COALESCE(t.status, '') != 'canceled' AND t.stage != 'completed'")
        .bind(&request.id).fetch_one(&state.db).await?;
    if current == 0 {
        return Ok(());
    }
    let job = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE id = ?")
        .bind(&request.review_job_id)
        .fetch_one(&state.db)
        .await?;
    if let Err(reason) =
        validate_result(&state.db, &job, job.result.as_deref().unwrap_or_default()).await
    {
        sqlx::query("UPDATE replan_requests SET state = 'waiting', updated_at = ? WHERE id = ?")
            .bind(now())
            .bind(&request.id)
            .execute(&state.db)
            .await?;
        set_waiting(state, request, &reason).await?;
        return Ok(());
    }
    let parent_id = request.reviewer_task_id.as_deref().unwrap();
    let active_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs j JOIN tasks t ON t.id = j.task_id WHERE j.status = 'running' AND (t.id = ? OR t.parent_id = ?)")
        .bind(parent_id).bind(parent_id).fetch_one(&state.db).await?;
    if active_jobs > 0 {
        return Ok(());
    }
    let parent = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
        .bind(parent_id)
        .fetch_one(&state.db)
        .await?;
    if matches!(
        parent.status.as_deref(),
        Some(STATUS_PAUSED | STATUS_PAUSING)
    ) {
        return Ok(());
    }
    let proposal = load_plan(&state.db, request).await?;
    let rows =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? ORDER BY seq")
            .bind(&project.id)
            .fetch_all(&state.db)
            .await?;
    let canceled = rows
        .iter()
        .filter(|task| {
            task.parent_id.as_deref() == Some(parent_id)
                && task.stage != TASK_STAGE_COMPLETED
                && task.status.as_deref() != Some(STATUS_CANCELED)
        })
        .collect::<Vec<_>>();
    let canceled_keys = canceled
        .iter()
        .map(|task| task.key.clone())
        .collect::<std::collections::HashSet<_>>();
    let taken = rows
        .iter()
        .filter(|task| {
            task.status.as_deref() != Some(STATUS_CANCELED) && !canceled_keys.contains(&task.key)
        })
        .map(|task| task.key.clone())
        .collect();
    let tasks = proposal
        .tasks
        .into_iter()
        .map(|task| crate::domain::NormalizedTask {
            key: task.key,
            title: task.title,
            description: task.description,
            acceptance: task.acceptance,
            deps: task.deps,
        })
        .collect();
    let (tasks, _) = crate::domain::reserve_plan_keys(tasks, &taken);
    let mut deps = parse_string_list(&parent.deps)
        .into_iter()
        .filter(|key| !canceled_keys.contains(key))
        .collect::<Vec<_>>();
    deps.extend(tasks.iter().map(|task| task.key.clone()));
    let timestamp = now();
    let max_seq = rows.iter().map(|task| task.seq).max().unwrap_or(0);
    let max_queue = rows.iter().map(|task| task.queue_order).max().unwrap_or(0);
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    for task in &canceled {
        sqlx::query(
            "UPDATE tasks SET status = 'canceled', agent_id = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(&timestamp)
        .bind(&task.id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE replan_requests SET state = 'obsolete', updated_at = ? WHERE task_id = ? AND id != ? AND state IN ('pending', 'running', 'planned', 'waiting')")
            .bind(&timestamp).bind(&task.id).bind(&request.id).execute(&mut *tx).await?;
    }
    for (index, task) in tasks.iter().enumerate() {
        let id = new_id("tsk");
        sqlx::query("INSERT INTO tasks (id, project_id, seq, queue_order, key, title, description, acceptance, deps, parent_id, stage, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 'queued', ?, ?)")
            .bind(&id).bind(&project.id).bind(max_seq + index as i64 + 1).bind(max_queue + index as i64 + 1)
            .bind(&task.key).bind(&task.title).bind(&task.description).bind(string_list_json(&task.acceptance))
            .bind(string_list_json(&task.deps)).bind(parent_id).bind(&timestamp).bind(&timestamp).execute(&mut *tx).await?;
        crate::db::initialize_task_agents_tx(&mut tx, &id).await?;
    }
    sqlx::query("UPDATE tasks SET deps = ?, stage = 'pending', status = CASE WHEN status IN ('paused', 'pausing') THEN status ELSE 'queued' END, last_error = NULL, waiting_reason = NULL, guidance = guidance || char(10) || ?, updated_at = ? WHERE id = ?")
        .bind(string_list_json(&deps)).bind(&proposal.summary).bind(&timestamp).bind(parent_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE replan_requests SET state = 'applied', updated_at = ? WHERE id = ?")
        .bind(&timestamp)
        .bind(&request.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(parent_id),
        "task_replan_applied",
        format!(
            "父任务 {} 的替代方案已应用：取消 {} 个未完成子任务，创建 {} 个子任务；其他任务不变",
            parent.key,
            canceled.len(),
            tasks.len()
        ),
    )
    .await;
    for task in canceled {
        if let Err(error) = crate::artifacts::cleanup_task(state, project, task, true).await {
            tracing::warn!(task = %task.id, error = %error, "清理被局部重规划取消的子任务失败");
        }
    }
    Ok(())
}

pub async fn context(state: &AppState, job: &JobRow) -> Result<Value> {
    let request = sqlx::query_as::<_, ReviewRequest>(
        "SELECT * FROM replan_requests WHERE review_job_id = ? AND state = 'running'",
    )
    .bind(&job.id)
    .fetch_one(&state.db)
    .await?;
    let tasks =
        sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE project_id = ? AND COALESCE(status, '') != 'canceled' ORDER BY seq")
            .bind(&job.project_id)
            .fetch_all(&state.db)
            .await?;
    let source = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE id = ?")
        .bind(&request.source_job_id)
        .fetch_one(&state.db)
        .await?;
    let tasks = tasks.iter().map(|task| json!({"id": task.id, "key": task.key,
        "title": task.title, "description": task.description, "acceptance": parse_string_list(&task.acceptance),
        "deps": parse_string_list(&task.deps), "parent_id": task.parent_id,
        "stage": task.stage, "status": task.status, "worktree": task.worktree, "branch": task.branch,
        "rejections": task.rejections, "issues": parse_string_list(&task.verifier_issues),
        "summary": task.result_summary.as_deref().unwrap_or_default().chars().take(2000).collect::<String>(),
    })).collect::<Vec<_>>();
    let history: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, role, result, error FROM jobs WHERE task_id = ? AND status != 'running' ORDER BY rowid DESC LIMIT 6")
        .bind(&request.task_id).fetch_all(&state.db).await?;
    Ok(
        json!({"request": request, "reviewer": if request.reviewer_task_id.is_some() { "parent" } else { "planner" },
        "tasks": tasks, "source_job": {"id": source.id, "result": source.result, "error": source.error},
        "recent_jobs": history.into_iter().map(|(id, role, result, error)| json!({"id": id, "role": role, "result": result, "error": error})).collect::<Vec<_>>()}),
    )
}

/// Caller holds the scheduling lock. The request/job identity and generation
/// prevent stale decisions from reviving canceled tasks or superseded requests.
pub async fn settle_locked(
    state: &AppState,
    job: &JobRow,
    parsed: Option<&ParsedResult>,
    error: Option<&str>,
    interrupted: bool,
) -> Result<()> {
    let request = sqlx::query_as::<_, ReviewRequest>(
        "SELECT * FROM replan_requests WHERE review_job_id = ? AND state = 'running'",
    )
    .bind(&job.id)
    .fetch_optional(&state.db)
    .await?;
    let Some(request) = request else {
        return Ok(());
    };
    let project = crate::replan::load_project(&state.db, &job.project_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("项目不存在"))?;
    let task = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
        .bind(&request.task_id)
        .fetch_one(&state.db)
        .await?;
    let parent_canceled: i64 = if let Some(id) = &request.reviewer_task_id {
        sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE id = ? AND status = 'canceled'")
            .bind(id)
            .fetch_one(&state.db)
            .await?
    } else {
        0
    };
    if project.status.as_deref() == Some(STATUS_ARCHIVED)
        || task.status.as_deref() == Some(STATUS_CANCELED)
        || task.stage == TASK_STAGE_COMPLETED
        || parent_canceled > 0
    {
        sqlx::query("UPDATE replan_requests SET state = 'obsolete', updated_at = ? WHERE id = ?")
            .bind(now())
            .bind(&request.id)
            .execute(&state.db)
            .await?;
        return Ok(());
    }
    if request.generation != project.replan_generation || project.replan_active != 0 {
        sqlx::query("UPDATE replan_requests SET state = 'pending', review_job_id = NULL, updated_at = ? WHERE id = ?")
            .bind(now()).bind(&request.id).execute(&state.db).await?;
        return Ok(());
    }
    if let Some(ParsedResult::AnalysisPlan { goal, tasks, .. }) = parsed {
        let invalid = goal
            .as_deref()
            .is_some_and(|goal| goal.trim() != project.goal.trim())
            || (request.reviewer_task_id.is_some()
                && tasks.len() > crate::domain::MAX_DERIVED_TASKS);
        let raw = sqlx::query_scalar::<_, Option<String>>("SELECT result FROM jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(&state.db)
            .await?
            .unwrap_or_default();
        if !invalid && validate_result(&state.db, job, &raw).await.is_ok() {
            sqlx::query(
                "UPDATE replan_requests SET state = 'planned', updated_at = ? WHERE id = ?",
            )
            .bind(now())
            .bind(&request.id)
            .execute(&state.db)
            .await?;
            set_waiting(
                state,
                &request,
                "重规划替代方案已生成，等待受影响的在途作业结算后应用",
            )
            .await?;
            emit(
                &state.db,
                &state.bus,
                Some(&job.project_id),
                Some(&request.task_id),
                "replan_review_planned",
                "重规划决策与替代方案已在同一作业中生成",
            )
            .await;
            return Ok(());
        }
    }
    let Some(ParsedResult::ReplanReview {
        decision,
        reason,
        guidance,
        resume_stage,
        user_action,
    }) = parsed
    else {
        let retry = interrupted || request.attempts < 3;
        sqlx::query("UPDATE replan_requests SET state = ?, review_job_id = NULL, attempts = CASE WHEN ? THEN MAX(attempts - 1, 0) ELSE attempts END, updated_at = ? WHERE id = ?")
            .bind(if retry { "pending" } else { "waiting" }).bind(interrupted).bind(now()).bind(&request.id).execute(&state.db).await?;
        set_waiting(
            state,
            &request,
            &format!(
                "重规划审批{}：{}",
                if retry {
                    "等待重试"
                } else {
                    "失败，等待人工指导或重试"
                },
                error.unwrap_or("审批结果无效")
            ),
        )
        .await?;
        return Ok(());
    };
    let note =
        crate::user_action::review_note(request.reviewer_task_id.is_some(), reason, guidance);
    let waiting = user_action
        .as_ref()
        .map(|action| action.summary.as_str())
        .unwrap_or("等待外部条件，请查看处理说明");
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let next = if decision == "reject" {
        "rejected"
    } else {
        "waiting"
    };
    sqlx::query("UPDATE replan_requests SET state = ?, guidance = ?, updated_at = ? WHERE id = ? AND state = 'running'")
        .bind(next).bind(&note).bind(now()).bind(&request.id).execute(&mut *tx).await?;
    let stage = resume_stage.as_deref().unwrap_or(&request.resume_stage);
    sqlx::query("UPDATE tasks SET guidance = CASE WHEN guidance = '' THEN ? ELSE guidance || char(10) || ? END, status = CASE WHEN status IN ('paused', 'pausing') THEN status WHEN ? THEN 'queued' ELSE 'blocked' END, last_error = CASE WHEN ? THEN NULL ELSE ? END, waiting_reason = ?, stage = ?, updated_at = ? WHERE id = ?")
        .bind(&note).bind(&note).bind(decision == "reject").bind(decision == "reject").bind(&note)
        .bind(if next == "rejected" { None } else { Some(waiting) })
        .bind(stage).bind(now()).bind(&request.task_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE replan_requests SET resume_stage = ? WHERE id = ?")
        .bind(stage)
        .bind(&request.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    emit(
        &state.db,
        &state.bus,
        Some(&job.project_id),
        Some(&request.task_id),
        if next == "rejected" {
            "replan_review_rejected"
        } else {
            "replan_review_waiting"
        },
        note,
    )
    .await;
    Ok(())
}

pub async fn set_waiting(state: &AppState, request: &ReviewRequest, reason: &str) -> Result<()> {
    sqlx::query("UPDATE tasks SET waiting_reason = ?, last_error = ?, updated_at = ? WHERE id = ? AND status NOT IN ('canceled')")
        .bind(reason).bind(reason).bind(now()).bind(&request.task_id).execute(&state.db).await?;
    Ok(())
}

/// Human retry requeues the decision, not the execution that requested it.
/// Explicit human guidance may resolve the request and resume execution.
pub async fn retry_locked(
    state: &AppState,
    task: &TaskRow,
    guidance: Option<&str>,
) -> Result<bool> {
    let Some(request) = active(&state.db, &task.id).await? else {
        return Ok(false);
    };
    if request.state != "waiting" {
        anyhow::bail!("重规划请求正在审批，请等待审批结束")
    }
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("UPDATE replan_requests SET state = ?, attempts = 0, review_job_id = NULL, updated_at = ? WHERE id = ?")
        .bind(if guidance.is_some() { "resolved" } else { "pending" }).bind(now()).bind(&request.id).execute(&mut *tx).await?;
    sqlx::query("UPDATE tasks SET status = CASE WHEN status IN ('paused', 'pausing') THEN status ELSE ? END, stage = ?, guidance = CASE WHEN ? IS NULL THEN guidance ELSE guidance || char(10) || ? END, waiting_reason = ?, last_error = NULL, updated_at = ? WHERE id = ?")
        .bind(if guidance.is_some() { STATUS_QUEUED } else { STATUS_BLOCKED }).bind(&request.resume_stage).bind(guidance).bind(guidance)
        .bind(if guidance.is_some() { None } else { Some("重规划审批：等待重新审批") }).bind(now()).bind(&task.id).execute(&mut *tx).await?;
    sqlx::query("UPDATE projects SET status = CASE WHEN status IN ('paused', 'pausing', 'archived') THEN status ELSE 'running' END, blocked_reason = NULL WHERE id = ?")
        .bind(&task.project_id).execute(&mut *tx).await?;
    tx.commit().await?;
    emit(
        &state.db,
        &state.bus,
        Some(&task.project_id),
        Some(&task.id),
        "replan_review_retried",
        if guidance.is_some() {
            "人工指导已解决重规划请求，任务继续执行"
        } else {
            "重规划请求已重新排队审批"
        },
    )
    .await;
    Ok(true)
}
