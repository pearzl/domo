//! 任务人工里程碑验收与人工指导。

use axum::extract::{Path, State};
use axum::Json;

use crate::domain::now;
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::replan;
use crate::state::AppState;
use crate::transitions::{TaskAction, Transitions};

use super::detail::load_detail;
use super::helpers::{load_project_task, require_project};
use super::ApiError;

pub(super) async fn save_task_guidance(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<TaskGuidanceRequest>,
) -> Result<Json<TaskView>, ApiError> {
    let guidance = request
        .guidance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad("人工指导不能为空"))?
        .to_string();
    let transition = Transitions::lock(&state).await;
    let (_, task) = transition
        .task(&project_id, &task_id, TaskAction::Guide)
        .await?;
    let timestamp = now();
    sqlx::query("UPDATE tasks SET guidance = ?, updated_at = ? WHERE id = ?")
        .bind(&guidance)
        .bind(&timestamp)
        .bind(&task.id)
        .execute(&state.db)
        .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "guidance",
        format!("任务 {} 收到人工指导：{guidance}", task.key),
    )
    .await;
    let row = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ?")
        .bind(&task.id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(TaskView::from_row(row)))
}

pub(super) async fn set_task_milestone(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<MilestoneRequest>,
) -> Result<Json<TaskView>, ApiError> {
    let milestone = match (request.milestone, request.action.as_deref()) {
        (Some(value), _) => value,
        (None, Some("set" | "enable")) => true,
        (None, Some("clear" | "cancel" | "disable")) => false,
        _ => return Err(ApiError::bad("必须指定 milestone 布尔值或 set/clear 动作")),
    };
    let transition = Transitions::lock(&state).await;
    let (_, task) = transition
        .task(&project_id, &task_id, TaskAction::Milestone)
        .await?;
    if task.human_gate.as_deref() == Some(HUMAN_GATE_MILESTONE) {
        return Err(ApiError::bad("任务正在等待人工验收，不能修改里程碑标记"));
    }
    sqlx::query("UPDATE tasks SET milestone = ?, updated_at = ? WHERE id = ?")
        .bind(if milestone { 1 } else { 0 })
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        if milestone {
            "milestone_set"
        } else {
            "milestone_cleared"
        },
        if milestone {
            format!("任务 {} 已标记为里程碑，验收通过后需要人工确认", task.key)
        } else {
            format!("任务 {} 已取消里程碑标记", task.key)
        },
    )
    .await;
    let row = load_project_task(&state, &project_id, &task.id).await?;
    Ok(Json(TaskView::from_row(row)))
}

pub(super) async fn accept_milestone(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<MilestoneAcceptRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let note = request
        .reason
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let _guard = Transitions::lock(&state).await;
    let (project, task) = {
        let (project, task) = _guard
            .task(&project_id, &task_id, TaskAction::Milestone)
            .await?;
        if project.replan_active != 0 {
            return Err(ApiError::conflict(
                "重规划期间不能合入旧里程碑，请先确认或放弃草稿",
            ));
        }
        if task.status.as_deref() != Some(STATUS_BLOCKED)
            || task.human_gate.as_deref() != Some(HUMAN_GATE_MILESTONE)
        {
            return Err(ApiError::bad("任务不在等待人工验收状态"));
        }
        sqlx::query(
            "UPDATE tasks SET human_gate = 'milestone_accepting', updated_at = ? WHERE id = ?",
        )
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
        (project, task)
    };

    if project.is_git() {
        let Some(branch) = task.branch.clone() else {
            restore_milestone_gate_unlocked(&state, &task.id, "任务缺少执行分支，无法完成人工验收")
                .await?;
            return Err(ApiError::bad("任务缺少执行分支，无法完成人工验收"));
        };
        let target = match crate::runner::task_merge_target(&state, &project, &task).await {
            Ok(target) => target,
            Err(error) => {
                restore_milestone_gate_unlocked(&state, &task.id, &error.to_string()).await?;
                return Err(ApiError::internal(error));
            }
        };
        let message = format!("domo(task-{}): {}", task.key, task.title);
        if let Some(delay) = std::env::var("DOMO_MOCK_MILESTONE_MERGE_DELAY_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        let merge = {
            let _guard = state.git_lock.lock().await;
            git::squash_merge(&target.worktree, &branch, &message).await
        };
        if let Err(error) = merge {
            emit(
                &state.db,
                &state.bus,
                Some(&project_id),
                Some(&task.id),
                "milestone_merge_conflict",
                format!(
                    "任务 {} 人工验收通过{}，合并失败：{error:#}",
                    task.key,
                    if note.is_empty() {
                        String::new()
                    } else {
                        format!("：{note}")
                    }
                ),
            )
            .await;
            crate::runner::handoff_merge_conflict_unlocked(
                &state,
                &project,
                &task,
                &error,
                task.agent_id.as_deref(),
            )
            .await?;
            sqlx::query("UPDATE projects SET status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
                THEN status ELSE 'running' END, blocked_reason = NULL, updated_at = ? WHERE id = ?")
                .bind(now()).bind(&project_id).execute(&state.db).await?;
            drop(_guard);
            state.notify_scheduler();
            return load_detail(&state, &project_id).await.map(Json);
        }
    }

    {
        let fresh_project = require_project(&state, &project_id).await?;
        sqlx::query(
            "UPDATE tasks SET stage = ?, status = NULL, human_gate = NULL, last_error = NULL, \
              forced_agent_id = NULL, forced_agent_role = NULL, verifier_issues = '[]', updated_at = ? WHERE id = ?",
        )
        .bind(TASK_STAGE_COMPLETED)
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
        sqlx::query(
            "UPDATE projects SET status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
             THEN status ELSE ? END, blocked_reason = CASE WHEN status IN ('pausing', 'paused') \
             THEN ? ELSE NULL END, updated_at = ? WHERE id = ?",
        )
        .bind(STATUS_RUNNING)
        .bind("人工验收已通过，等待项目恢复后继续")
        .bind(now())
        .bind(&project_id)
        .execute(&state.db)
        .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project_id),
            Some(&task.id),
            "milestone_accepted",
            format!(
                "任务 {} 人工验收通过{}{}",
                task.key,
                if note.is_empty() {
                    String::new()
                } else {
                    format!("：{note}")
                },
                if project.is_git() {
                    if task.parent_id.is_some() {
                        "，已合并到父任务分支"
                    } else {
                        "，已合并到项目交付分支"
                    }
                } else {
                    "，普通工作区交付完成"
                }
            ),
        )
        .await;
        emit(
            &state.db,
            &state.bus,
            Some(&project_id),
            Some(&task.id),
            "task_completed",
            if project.is_git() {
                format!(
                    "任务 {} 人工验收通过并合并到{}",
                    task.key,
                    if task.parent_id.is_some() {
                        "父任务分支"
                    } else {
                        "项目交付分支"
                    }
                )
            } else {
                format!("任务 {} 人工验收通过，交付完成", task.key)
            },
        )
        .await;
        if fresh_project.status.as_deref() == Some(STATUS_ARCHIVED) {
            tracing::warn!(project_id = %project_id, "项目在人工验收合并期间被归档");
        }
    }
    drop(_guard);
    crate::runner::cleanup_task_artifacts(&state, &project, &task).await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

pub(super) async fn restore_milestone_gate(
    state: &AppState,
    task_id: &str,
    reason: &str,
) -> Result<(), ApiError> {
    let _guard = state.schedule_lock.lock().await;
    restore_milestone_gate_unlocked(state, task_id, reason).await
}

async fn restore_milestone_gate_unlocked(
    state: &AppState,
    task_id: &str,
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query(
         "UPDATE tasks SET human_gate = ?, status = ?, last_error = ?, updated_at = ? WHERE id = ? \
          AND human_gate IN ('milestone_replanning', 'milestone_accepting') AND stage != 'completed' AND COALESCE(status, '') != 'canceled' \
          AND EXISTS (SELECT 1 FROM projects WHERE id = tasks.project_id AND COALESCE(status, '') != 'archived')",
    )
    .bind(HUMAN_GATE_MILESTONE)
    .bind(STATUS_BLOCKED)
    .bind(reason)
    .bind(now())
    .bind(task_id)
    .execute(&state.db)
    .await?;
    Ok(())
}

pub(super) async fn reject_milestone(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<MilestoneRejectRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let reason = request.reason.trim().to_string();
    if reason.is_empty() {
        return Err(ApiError::bad("拒绝人工验收时必须填写理由"));
    }
    let guidance = request
        .guidance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let disposition = milestone_reject_disposition(request.action.as_deref())?;
    let _guard = Transitions::lock(&state).await;
    let (project, task) = _guard
        .task(&project_id, &task_id, TaskAction::Milestone)
        .await?;
    if task.status.as_deref() != Some(STATUS_BLOCKED)
        || task.human_gate.as_deref() != Some(HUMAN_GATE_MILESTONE)
    {
        return Err(ApiError::bad("任务不在等待人工验收状态"));
    }
    if disposition == MILESTONE_DISPOSITION_REPLAN {
        sqlx::query("UPDATE tasks SET human_gate = ?, rejections = ?, updated_at = ? WHERE id = ?")
            .bind(HUMAN_GATE_MILESTONE_REPLANNING)
            .bind(task.rejections + 1)
            .bind(now())
            .bind(&task.id)
            .execute(&state.db)
            .await?;
        drop(_guard);
        return replan_rejected_milestone(&state, project, task, &reason, guidance).await;
    }
    let issues = serde_json::json!([reason]).to_string();
    let guidance = guidance.unwrap_or_else(|| task.guidance.clone());
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, human_gate = NULL, agent_id = NULL, \
         last_error = NULL, verifier_issues = ?, guidance = ?, forced_agent_id = NULL, \
         forced_agent_role = NULL, rejections = ?, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_EXECUTING)
    .bind(STATUS_QUEUED)
    .bind(&issues)
    .bind(&guidance)
    .bind(task.rejections + 1)
    .bind(now())
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    let paused = matches!(
        project.status.as_deref(),
        Some(STATUS_PAUSING) | Some(STATUS_PAUSED)
    );
    sqlx::query(
        "UPDATE projects SET status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
         THEN status ELSE ? END, blocked_reason = CASE WHEN ? THEN ? ELSE NULL END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(STATUS_RUNNING)
    .bind(paused)
    .bind("人工验收已拒绝，等待项目恢复后重新执行")
    .bind(now())
    .bind(&project_id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "milestone_rejected",
        format!(
            "任务 {} 人工验收被拒绝（第 {} 次验收失败），退回执行：{reason}",
            task.key,
            task.rejections + 1
        ),
    )
    .await;
    if task.rejections + 1 >= 3 {
        let reviewed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM replan_requests WHERE task_id = ? AND origin = 'rejections'",
        )
        .bind(&task.id)
        .fetch_one(&state.db)
        .await?;
        if reviewed == 0 {
            let source = sqlx::query_as::<_, JobRow>("SELECT * FROM jobs WHERE task_id = ? AND role = 'verifier' AND status = 'succeeded' ORDER BY rowid DESC LIMIT 1")
                .bind(&task.id).fetch_optional(&state.db).await?;
            if let Some(source) = source {
                crate::replan_review::submit_locked(
                    &state,
                    &source,
                    &format!("连续验收拒绝，最近人工意见：{reason}\n{guidance}"),
                    "rejections",
                    TASK_STAGE_EXECUTING,
                )
                .await
                .map_err(ApiError::internal)?;
            }
        } else {
            sqlx::query(
                "UPDATE tasks SET status = 'blocked', last_error = ?, updated_at = ? WHERE id = ?",
            )
            .bind(format!("上级指导后仍多次验收拒绝，等待人工裁决：{reason}"))
            .bind(now())
            .bind(&task.id)
            .execute(&state.db)
            .await?;
        }
    }
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

pub(super) fn milestone_reject_disposition(action: Option<&str>) -> Result<&'static str, ApiError> {
    let Some(action) = action.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(MILESTONE_DISPOSITION_RETRY);
    };
    match action.to_ascii_lowercase().as_str() {
        "retry" | "rework" | "reject" | "execute" | "execution" | "requeue" => {
            Ok(MILESTONE_DISPOSITION_RETRY)
        }
        "replan" | "replanning" | "replan_project" | "plan" => Ok(MILESTONE_DISPOSITION_REPLAN),
        _ => Err(ApiError::bad(
            "不支持的里程碑拒绝处置，请选择 retry 或 replan",
        )),
    }
}

pub(super) async fn replan_rejected_milestone(
    state: &AppState,
    project: ProjectRow,
    task: TaskRow,
    reason: &str,
    guidance: Option<String>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let mut replan_reason = format!("人工里程碑验收拒绝（任务 {}）：{reason}", task.key);
    if let Some(guidance) = guidance.as_deref() {
        replan_reason.push_str(&format!("；指导意见：{guidance}"));
    }
    let outcome = replan::request(state, &project.id, &replan_reason, REPLAN_SOURCE_HUMAN).await;
    let summary = match outcome {
        Ok(Some(summary)) => summary,
        Ok(None) => {
            restore_milestone_gate(state, &task.id, "项目状态已变化，未执行重新规划").await?;
            return Err(ApiError::bad("项目状态已变化，未执行重新规划"));
        }
        Err(error) => {
            let message = format!("重新规划失败：{error:#}");
            restore_milestone_gate(state, &task.id, &message).await?;
            return Err(ApiError::internal(error));
        }
    };
    sqlx::query(
        "UPDATE tasks SET human_gate = NULL, updated_at = ? WHERE id = ? AND human_gate = ?",
    )
    .bind(now())
    .bind(&task.id)
    .bind(HUMAN_GATE_MILESTONE_REPLANNING)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        "milestone_rejected",
        format!(
            "任务 {} 人工验收被拒绝，要求重新规划：{reason}{}",
            task.key,
            guidance
                .as_deref()
                .map(|value| format!("；指导意见：{value}"))
                .unwrap_or_default()
        ),
    )
    .await;
    tracing::info!(
        project_id = %project.id,
        task_key = %task.key,
        canceled_tasks = summary.canceled_tasks,
        retained_tasks = summary.retained_tasks,
        canceled_jobs = summary.canceled_jobs,
        "里程碑人工验收要求重新规划"
    );
    state.notify_scheduler();
    load_detail(state, &project.id).await.map(Json)
}
