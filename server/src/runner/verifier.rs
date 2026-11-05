use anyhow::{anyhow, Result};

use crate::domain::{self, now, FailureKind};
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::state::AppState;

use super::common::{load_task, models_list_json};
use super::executor::cleanup_task_artifacts;
use super::failure::{
    apply_agent_failure, failure_kind_label, has_usable_agent, task_pause_intent,
};
use super::worktree::{prepare_task_worktree, task_merge_target};

pub(super) async fn handle_verdict(
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
                  last_error = ?, verifier_issues = '[]', forced_agent_id = NULL, forced_agent_role = NULL, updated_at = ? \
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
            // 里程碑 gate 只阻塞当前任务；项目可推进性由调度器统一计算。
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
        if !project.is_git() {
            complete_verified_task(
                state,
                project,
                task,
                agent,
                format!("任务 {} 验收通过，普通工作区交付完成", task.key),
            )
            .await?;
            return Ok(());
        }
        let branch = task
            .branch
            .clone()
            .ok_or_else(|| anyhow!("任务缺少执行分支"))?;
        let target = task_merge_target(state, project, task).await?;
        let message = format!("domo(task-{}): {}", task.key, task.title);
        let merge = {
            let _guard = state.git_lock.lock().await;
            git::squash_merge(&target.worktree, &branch, &message).await
        };
        match merge {
            Ok(_) => {
                complete_verified_task(
                    state,
                    project,
                    task,
                    agent,
                    format!("任务 {} 验收通过并合并到{}", task.key, target.label),
                )
                .await?;
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
        // Each verdict is a complete current list; old verdicts remain on jobs.
        let current_issues = if issues.is_empty() {
            models_list_json(&[reason.to_string()])
        } else {
            models_list_json(issues)
        };
        sqlx::query(
            "UPDATE tasks SET stage = ?, status = ?, attempts = 0, rejections = ?, verifier_issues = ?, \
             last_error = ?, agent_id = ?, waiting_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(TASK_STAGE_EXECUTING)
        .bind(status)
        .bind(rejections)
        .bind(&current_issues)
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

/// Marks a task complete after its acceptance requirements are met, emits the
/// audit event and releases task artifacts when the workspace owns any.
async fn complete_verified_task(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    agent: &AgentRow,
    message: String,
) -> Result<()> {
    let guard = state.schedule_lock.lock().await;
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = NULL, agent_id = ?, last_error = NULL, attempts = 0, \
          human_gate = NULL, verifier_issues = '[]', forced_agent_id = NULL, forced_agent_role = NULL, \
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
        message,
    )
    .await;
    drop(guard);
    cleanup_task_artifacts(state, project, task).await;
    Ok(())
}

/// A rejected verdict returns the task to the executor with verifier issues and
/// counts against the rejection limit. A merge conflict uses the same handoff
/// path but keeps the rejection count intact: the executor only resolves the
/// conflict, then the verifier checks the task again.
pub(super) async fn handoff_merge_conflict(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    merge_error: &git::GitError,
    agent: &AgentRow,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    handoff_merge_conflict_unlocked(state, project, task, merge_error, Some(&agent.id)).await
}

/// Caller holds the scheduler lock through Git conflict preparation and the
/// task transition, including milestone acceptance initiated through the API.
pub(crate) async fn handoff_merge_conflict_unlocked(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    merge_error: &git::GitError,
    agent_id: Option<&str>,
) -> Result<()> {
    let reason = format!("验收通过但合并失败：{merge_error:#}");
    if matches!(merge_error, git::GitError::DirtyMergeTarget { .. }) {
        return block_task_after_merge_failure(state, project, task, &reason).await;
    }
    let target = task_merge_target(state, project, task).await?;
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
        git::merge_conflict_files(&worktree, &target.branch).await
    };
    let conflicts = match conflicts {
        Ok(files) if !files.is_empty() => files,
        Ok(_) => {
            return block_task_after_merge_failure(
                state,
                project,
                task,
                &format!("{reason}；任务分支与{}没有文件冲突", target.label),
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
        "验收通过后合并到{}时发生合并冲突，冲突文件：{}。工作区已处于合并冲突状态，\
          请直接编辑冲突文件消除冲突标记，保留任务需要的改动；DOMO 会负责提交并在解决后重新验收。",
        target.label,
        conflicts.join("、")
    );
    let pause = task_pause_intent(state, &task.id).await?;
    let issues = models_list_json(std::slice::from_ref(&issue));
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, verifier_issues = ?, last_error = ?, agent_id = ?, \
          human_gate = NULL, waiting_reason = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_EXECUTING)
    .bind(pause.unwrap_or(STATUS_QUEUED))
    .bind(&issues)
    .bind(&issue)
    .bind(agent_id)
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
            "任务 {} 合并冲突：合入{}失败，已退回执行者解决，之后重新验收：{}",
            task.key,
            target.label,
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
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, human_gate = NULL, last_error = ?, verifier_issues = ?, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_VERIFYING)
    .bind(STATUS_BLOCKED)
    .bind(reason)
    .bind(models_list_json(&[reason.to_string()]))
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

pub(super) async fn handle_verifier_failure(
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
    let attempts = domain::task_failure_attempts(task.attempts, kind);
    let mut blocked = kind == FailureKind::Protocol || (kind == FailureKind::Task && attempts >= 3);
    if kind.is_agent_failure()
        && effect.switch
        && kind == FailureKind::Permanent
        && !has_usable_agent(
            state,
            &project.id,
            ROLE_VERIFIER,
            Some(&agent.id),
            Some(task),
        )
        .await?
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
