//! 任务创建、队列顺序与项目分析阶段相关的 HTTP handler。

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use crate::domain::{self, now};
use crate::events::emit;
use crate::models::*;
use crate::paths;
use crate::state::AppState;
use crate::transitions::{ProjectAction, TaskAction, Transitions};
use crate::usage;

use super::detail::load_detail;
use super::helpers::{confirm_mode, load_project_task};
use super::ApiError;

pub(super) async fn pause_task(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    body: Option<Json<TaskPauseRequest>>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let request = body.map(|Json(value)| value).unwrap_or_default();
    let force = request.forced();
    {
        let transition = Transitions::lock(&state).await;
        let (project, task) = transition
            .task(&project_id, &task_id, TaskAction::Pause)
            .await?;
        let workspace = PathBuf::from(&project.workspace);
        if force {
            let running = sqlx::query_as::<_, JobRow>(
                "SELECT * FROM jobs WHERE task_id = ? AND status = 'running'",
            )
            .bind(&task.id)
            .fetch_all(&state.db)
            .await?;
            for job in &running {
                crate::agents::kill_job_dir(&paths::job_dir(&workspace, &job.id));
                crate::continuation::interrupted(
                    &state,
                    job,
                    crate::agents::StopReason::UserStopped,
                )
                .await?;
                let finished_at = now();
                let duration_ms = job
                    .started_at
                    .as_deref()
                    .and_then(|started| usage::elapsed_ms(Some(started), &finished_at));
                sqlx::query(
                    "UPDATE jobs SET status = ?, error = ?, duration_ms = COALESCE(duration_ms, ?), \
                     finished_at = ?, failure_kind = 'user_stopped' WHERE id = ? AND status = 'running'",
                )
                .bind(JOB_STATUS_CANCELED)
                .bind("任务被强制暂停，作业已终止")
                .bind(duration_ms)
                .bind(&finished_at)
                .bind(&job.id)
                .execute(&state.db)
                .await?;
            }
            sqlx::query(
                "UPDATE tasks SET status = ?, agent_id = NULL, waiting_reason = NULL, updated_at = ? \
                 WHERE id = ? AND stage != ?",
            )
            .bind(STATUS_PAUSED)
            .bind(now())
            .bind(&task.id)
            .bind(TASK_STAGE_COMPLETED)
            .execute(&state.db)
            .await?;
        } else {
            let running: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM jobs WHERE task_id = ? AND status = 'running'",
            )
            .bind(&task.id)
            .fetch_one(&state.db)
            .await?;
            let status = if running > 0 {
                STATUS_PAUSING
            } else {
                STATUS_PAUSED
            };
            sqlx::query(
                "UPDATE tasks SET status = ?, waiting_reason = NULL, updated_at = ? \
                 WHERE id = ? AND stage != ?",
            )
            .bind(status)
            .bind(now())
            .bind(&task.id)
            .bind(TASK_STAGE_COMPLETED)
            .execute(&state.db)
            .await?;
        }
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        if force {
            "task_paused_forced"
        } else {
            "task_pausing"
        },
        if force {
            format!("任务 {} 已强制暂停，在途作业已终止", task.key)
        } else {
            format!("任务 {} 暂停请求已提交", task.key)
        },
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

pub(super) async fn resume_task(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> Result<Json<ProjectDetail>, ApiError> {
    {
        let transition = Transitions::lock(&state).await;
        let (project, task) = transition
            .task(&project_id, &task_id, TaskAction::Resume)
            .await?;
        sqlx::query(
            "UPDATE tasks SET status = ?, waiting_reason = NULL, updated_at = ? \
             WHERE id = ? AND status IN (?, ?)",
        )
        .bind(
            if crate::continuation::pending_manual(&state, &project, Some(&task.id)).await? {
                STATUS_BLOCKED
            } else {
                STATUS_QUEUED
            },
        )
        .bind(now())
        .bind(&task.id)
        .bind(STATUS_PAUSING)
        .bind(STATUS_PAUSED)
        .execute(&state.db)
        .await?;
        if !matches!(
            project.status.as_deref(),
            Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
        ) {
            let human_gate: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND id != ? AND stage != ? \
                 AND status = ? AND human_gate IS NOT NULL",
            )
            .bind(&project_id)
            .bind(&task.id)
            .bind(TASK_STAGE_COMPLETED)
            .bind(STATUS_BLOCKED)
            .fetch_one(&state.db)
            .await?;
            if human_gate == 0 {
                sqlx::query(
                    "UPDATE projects SET status = ?, blocked_reason = NULL, updated_at = ? \
                     WHERE id = ? AND status = ?",
                )
                .bind(STATUS_RUNNING)
                .bind(now())
                .bind(&project_id)
                .bind(STATUS_BLOCKED)
                .execute(&state.db)
                .await?;
            }
        }
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "task_resumed",
        format!(
            "任务 {} 已恢复，{}",
            task.key,
            if task.status.as_deref() == Some(STATUS_BLOCKED) {
                "等待选择继续原会话或新会话执行"
            } else {
                "回到原阶段重新排队"
            }
        ),
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

pub(super) async fn reorder_task_queue(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<TaskQueueRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let direction = request.direction.as_deref().unwrap_or("").trim();
    if request.position.is_none() && !matches!(direction, "up" | "down") {
        return Err(ApiError::bad("direction 必须是 up 或 down"));
    }
    let transition = Transitions::lock(&state).await;
    let (_, task) = transition
        .task(&project_id, &task_id, TaskAction::Reorder)
        .await?;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let mut queue = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM tasks WHERE project_id = ? AND stage IN ('pending', 'executing') AND attempts = 0 \
          AND rejections = 0 AND COALESCE(status, '') != ? \
          AND (status IS NULL OR status IN ('queued', 'paused')) \
          AND NOT EXISTS(SELECT 1 FROM jobs WHERE task_id = tasks.id) ORDER BY queue_order ASC, seq ASC",
    )
    .bind(&project_id)
    .bind(STATUS_CANCELED)
    .fetch_all(&mut *tx)
    .await?;
    let Some(index) = queue.iter().position(|row| row.id == task.id) else {
        return Err(ApiError::bad("任务当前不在可调整的队列中"));
    };
    let target = if let Some(position) = request.position {
        let target = (position.max(1) as usize - 1).min(queue.len() - 1);
        if target == index {
            return Err(ApiError::bad("任务排队位置没有变化"));
        }
        let row = queue.remove(index);
        queue.insert(target, row);
        target
    } else {
        let target = match direction {
            "up" if index > 0 => index - 1,
            "down" if index + 1 < queue.len() => index + 1,
            _ => return Err(ApiError::bad("任务已经在队列边界")),
        };
        queue.swap(index, target);
        target
    };
    let timestamp = now();
    for (position, row) in queue.iter().enumerate() {
        sqlx::query(
            "UPDATE tasks SET queue_order = ?, updated_at = ? \
             WHERE id = ? AND stage IN ('pending', 'executing') AND attempts = 0 AND COALESCE(status, '') != ?",
        )
        .bind((position + 1) as i64)
        .bind(&timestamp)
        .bind(&row.id)
        .bind(STATUS_CANCELED)
        .execute(&mut *tx)
        .await?;
    }
    let queued: Vec<String> = sqlx::query_scalar("SELECT q.id FROM dispatch_queue q JOIN tasks t ON t.id = q.task_id \
        WHERE q.project_id = ? AND q.role = 'executor' AND q.state = 'waiting' ORDER BY t.queue_order, q.position")
        .bind(&project_id).fetch_all(&mut *tx).await?;
    let positions: Vec<i64> = sqlx::query_scalar("SELECT position FROM dispatch_queue WHERE project_id = ? AND role = 'executor' AND state = 'waiting' ORDER BY position")
        .bind(&project_id).fetch_all(&mut *tx).await?;
    for (id, position) in queued.iter().zip(positions) {
        sqlx::query("UPDATE dispatch_queue SET position = ? WHERE id = ?")
            .bind(position)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "task_queue_reordered",
        format!(
            "任务 {} 排队顺序已调整到第 {} 位{}",
            task.key,
            target + 1,
            match direction {
                "up" => "（向上）",
                "down" => "（向下）",
                _ => "",
            }
        ),
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

pub(super) async fn running_project_job(
    state: &AppState,
    project_id: &str,
    role: &str,
) -> Result<bool, ApiError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE project_id = ? AND role = ? AND status = 'running'",
    )
    .bind(project_id)
    .bind(role)
    .fetch_one(&state.db)
    .await?;
    Ok(count > 0)
}

pub(super) async fn post_analysis_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<AnalysisMessageRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let message = request.message.trim();
    if message.is_empty() {
        return Err(ApiError::bad("消息内容不能为空"));
    }
    let mode = request
        .confirm_mode
        .as_deref()
        .map(confirm_mode)
        .transpose()?;
    {
        let transition = Transitions::lock(&state).await;
        transition.project(&id, ProjectAction::Analysis).await?;
        if running_project_job(&state, &id, JOB_ROLE_ANALYSIS).await? {
            return Err(ApiError::bad("规划者正在处理上一条消息"));
        }
        sqlx::query(
            "INSERT INTO messages (id, project_id, role, content, created_at) \
             VALUES (?, ?, 'user', ?, ?)",
        )
        .bind(domain::new_id("msg"))
        .bind(&id)
        .bind(message)
        .bind(now())
        .execute(&state.db)
        .await?;
        // 项目目标只在首次输入时写入；后续消息属于分析对话或方案修订，
        // 目标由方案中的 goal 字段在确认时更新。
        sqlx::query(
            "UPDATE projects SET goal = CASE WHEN goal_locked_at IS NULL AND TRIM(goal) = '' THEN ? ELSE goal END, \
             confirm_mode = COALESCE(?, confirm_mode), \
             proposal = CASE WHEN replan_active = 0 THEN NULL ELSE proposal END, \
             confirmed_at = CASE WHEN replan_active = 0 THEN NULL ELSE confirmed_at END, \
             replan_proposal = NULL, status = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(message)
        .bind(mode)
        .bind(STATUS_RUNNING)
        .bind(now())
        .bind(&id)
        .execute(&state.db)
        .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        "analysis_message",
        "收到用户消息，规划者开始项目分析",
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &id).await.map(Json)
}

pub(super) async fn update_analysis_policy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<AnalysisPolicyRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let mode = confirm_mode(&request.mode)?;
    {
        let transition = Transitions::lock(&state).await;
        transition.project(&id, ProjectAction::Analysis).await?;
        sqlx::query("UPDATE projects SET confirm_mode = ?, updated_at = ? WHERE id = ?")
            .bind(mode)
            .bind(now())
            .bind(&id)
            .execute(&state.db)
            .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        "analysis_policy_updated",
        format!(
            "确认方式已设为{}",
            if mode == CONFIRM_AUTO {
                "自动确认"
            } else {
                "人工确认"
            }
        ),
    )
    .await;
    load_detail(&state, &id).await.map(Json)
}

pub(super) async fn confirm_analysis(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ConfirmAnalysisRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let transition = Transitions::lock(&state).await;
    let project = transition.project(&id, ProjectAction::Analysis).await?;
    if running_project_job(&state, &id, JOB_ROLE_ANALYSIS).await? {
        return Err(ApiError::bad("规划者正在更新项目方案，请稍后再确认"));
    }
    let proposal_raw = if project.replan_active != 0 {
        project.replan_proposal.as_deref()
    } else {
        project.proposal.as_deref()
    }
    .ok_or_else(|| ApiError::bad("规划者还没有给出可确认的项目方案"))?;
    let proposal: AnalysisProposal =
        serde_json::from_str(proposal_raw).map_err(|_| ApiError::bad("项目方案格式不合法"))?;
    if let Some(expected) = request.proposal {
        if expected
            != serde_json::from_str::<serde_json::Value>(proposal_raw)
                .map_err(|_| ApiError::bad("项目方案格式不合法"))?
        {
            return Err(ApiError::conflict("方案已变化，请审查最新草稿后重新确认"));
        }
    }
    if proposal.kind != "plan" {
        return Err(ApiError::bad("当前项目方案不能确认"));
    }
    let goal = proposal
        .goal
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(project.goal.trim())
        .to_string();
    if goal.is_empty() {
        return Err(ApiError::bad("目标内容为空"));
    }
    let tasks = domain::validate_plan(
        proposal
            .tasks
            .iter()
            .map(|task| domain::PlanTask {
                key: task.key.clone(),
                title: task.title.clone(),
                description: task.description.clone(),
                acceptance: task.acceptance.clone(),
                deps: task.deps.clone(),
            })
            .collect(),
    )
    .map_err(ApiError::bad)?;
    crate::runner::apply_analysis_plan_locked(
        &state,
        &project,
        &goal,
        &tasks,
        &proposal.summary,
        proposal_raw,
    )
    .await
    .map_err(replan_apply_error)?;
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        "analysis_confirmed",
        "方案已确认，开始执行任务",
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &id).await.map(Json)
}

fn replan_apply_error(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<crate::replan::StaleReplan>().is_some() {
        ApiError::conflict(error.to_string())
    } else {
        ApiError::internal(error)
    }
}

pub(super) async fn pause_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<ProjectPauseRequest>>,
) -> Result<Json<Value>, ApiError> {
    let request = body.map(|Json(value)| value).unwrap_or_default();
    let force = request.forced();
    {
        let transition = Transitions::lock(&state).await;
        let project = transition.project(&id, ProjectAction::Pause).await?;
        let workspace = PathBuf::from(&project.workspace);
        if force {
            let running = sqlx::query_as::<_, JobRow>(
                "SELECT * FROM jobs WHERE project_id = ? AND status = 'running'",
            )
            .bind(&id)
            .fetch_all(&state.db)
            .await?;
            for job in &running {
                crate::agents::kill_job_dir(&paths::job_dir(&workspace, &job.id));
                crate::continuation::interrupted(
                    &state,
                    job,
                    crate::agents::StopReason::UserStopped,
                )
                .await?;
                let finished_at = now();
                let duration_ms = job
                    .started_at
                    .as_deref()
                    .and_then(|started| usage::elapsed_ms(Some(started), &finished_at));
                sqlx::query(
                    "UPDATE jobs SET status = ?, error = ?, duration_ms = COALESCE(duration_ms, ?), \
                     finished_at = ?, failure_kind = 'user_stopped' WHERE id = ? AND status = 'running'",
                )
                .bind(JOB_STATUS_CANCELED)
                .bind("项目被强制暂停，作业已终止")
                .bind(duration_ms)
                .bind(&finished_at)
                .bind(&job.id)
                .execute(&state.db)
                .await?;
            }
            sqlx::query(
                "UPDATE tasks SET status = 'queued', agent_id = NULL, waiting_reason = NULL, updated_at = ? \
                 WHERE project_id = ? AND stage != ? AND status = 'running'",
            )
            .bind(now())
            .bind(&id)
            .bind(TASK_STAGE_COMPLETED)
            .execute(&state.db)
            .await?;
            for job in &running {
                if crate::continuation::pending_manual(&state, &project, job.task_id.as_deref())
                    .await?
                {
                    crate::continuation::block_interrupted(
                        &state,
                        job,
                        "作业被强制停止，请选择继续或新会话执行",
                    )
                    .await?;
                }
            }
            sqlx::query(
                "UPDATE projects SET status = ?, blocked_reason = NULL, updated_at = ? WHERE id = ?",
            )
            .bind(STATUS_PAUSED)
            .bind(now())
            .bind(&id)
            .execute(&state.db)
            .await?;
        } else {
            sqlx::query("UPDATE projects SET status = ?, updated_at = ? WHERE id = ?")
                .bind(STATUS_PAUSING)
                .bind(now())
                .bind(&id)
                .execute(&state.db)
                .await?;
        }
    }
    if force {
        emit(
            &state.db,
            &state.bus,
            Some(&id),
            None,
            "project_paused_forced",
            "项目已强制暂停，所有在途作业已终止",
        )
        .await;
    } else {
        emit(
            &state.db,
            &state.bus,
            Some(&id),
            None,
            "project_pausing",
            "项目暂停请求已提交，等待在途作业结束",
        )
        .await;
    }
    state.notify_scheduler();
    Ok(Json(json!({"ok": true, "forced": force})))
}

pub(super) async fn resume_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let transition = Transitions::lock(&state).await;
    let project = transition.project(&id, ProjectAction::Resume).await?;
    let analysis_pending = project.stage == STAGE_ANALYSIS
        && crate::runner::analysis_needs_dispatch(&state.db, &project).await?;
    let waiting_human = (project.stage == STAGE_ANALYSIS && !analysis_pending)
        || (project.stage == STAGE_SUMMARIZING && project.report.is_some())
        || crate::continuation::pending_manual(&state, &project, None).await?;
    {
        // 项目恢复只解除项目级派发限制，任务自己的暂停状态保持不变。
        sqlx::query(
            "UPDATE projects SET status = ?, blocked_reason = CASE WHEN ? THEN blocked_reason \
             ELSE NULL END, updated_at = ? WHERE id = ?",
        )
        .bind(if waiting_human {
            STATUS_BLOCKED
        } else {
            STATUS_RUNNING
        })
        .bind(waiting_human)
        .bind(now())
        .bind(&id)
        .execute(&state.db)
        .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        "project_resumed",
        "项目已恢复",
    )
    .await;
    state.notify_scheduler();
    Ok(Json(json!({"ok": true})))
}
