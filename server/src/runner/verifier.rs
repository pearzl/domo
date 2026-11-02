use std::path::PathBuf;

use anyhow::{anyhow, Result};

use crate::domain::{self, now, FailureKind};
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::paths;
use crate::state::AppState;

use super::common::{load_task, merge_issues};
use super::executor::cleanup_task_artifacts;
use super::failure::{
    apply_agent_failure, failure_kind_label, has_usable_agent, protocol_failure_blocks,
    task_pause_intent,
};
use super::worktree::prepare_task_worktree;

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
                 last_error = ?, forced_agent_id = NULL, forced_agent_role = NULL, updated_at = ? \
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
            sqlx::query(
                "UPDATE projects SET status = CASE WHEN status IN ('pausing', 'paused', 'archived') \
                 THEN status ELSE ? END, blocked_reason = ?, updated_at = ? WHERE id = ?",
            )
            .bind(STATUS_BLOCKED)
            .bind(&blocked_reason)
            .bind(now())
            .bind(&project.id)
            .execute(&state.db)
            .await?;
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
        let branch = task
            .branch
            .clone()
            .ok_or_else(|| anyhow!("任务缺少执行分支"))?;
        let workspace = PathBuf::from(&project.workspace);
        let message = format!("domo(task-{}): {}", task.key, task.title);
        let merge = {
            let _guard = state.git_lock.lock().await;
            git::squash_merge(&paths::project_repo(&workspace), &branch, &message).await
        };
        match merge {
            Ok(_) => {
                let guard = state.schedule_lock.lock().await;
                sqlx::query(
                    "UPDATE tasks SET stage = ?, status = NULL, agent_id = ?, last_error = NULL, \
                     human_gate = NULL, forced_agent_id = NULL, forced_agent_role = NULL, \
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
                    format!("任务 {} 验收通过并合并到项目分支", task.key),
                )
                .await;
                drop(guard);
                cleanup_task_artifacts(state, project, task).await;
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
        let merged_issues = merge_issues(&task.verifier_issues, issues);
        sqlx::query(
            "UPDATE tasks SET stage = ?, status = ?, rejections = ?, verifier_issues = ?, \
             last_error = ?, agent_id = ?, waiting_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(TASK_STAGE_EXECUTING)
        .bind(status)
        .bind(rejections)
        .bind(&merged_issues)
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
    let reason = format!("验收通过但合并失败：{merge_error:#}");
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
        git::merge_conflict_files(&worktree, &project.branch).await
    };
    let conflicts = match conflicts {
        Ok(files) if !files.is_empty() => files,
        Ok(_) => {
            return block_task_after_merge_failure(
                state,
                project,
                task,
                &format!("{reason}；任务分支与项目交付分支没有文件冲突"),
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
        "验收通过后合并到项目交付分支时发生合并冲突，冲突文件：{}。工作区已处于合并冲突状态，\
         请直接编辑冲突文件消除冲突标记，保留任务需要的改动；DOMO 会负责提交并在解决后重新验收。",
        conflicts.join("、")
    );
    let _guard = state.schedule_lock.lock().await;
    let pause = task_pause_intent(state, &task.id).await?;
    let issues = merge_issues(&task.verifier_issues, std::slice::from_ref(&issue));
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, verifier_issues = ?, last_error = ?, agent_id = ?, \
         waiting_reason = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_EXECUTING)
    .bind(pause.unwrap_or(STATUS_QUEUED))
    .bind(&issues)
    .bind(&issue)
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
        "merge_conflict",
        format!(
            "任务 {} 合并冲突：合入项目分支失败，已退回执行者解决，之后重新验收：{}",
            task.key,
            conflicts.join("、")
        ),
    )
    .await;
    Ok(())
}

pub(super) async fn block_task_after_merge_failure(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    reason: &str,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, last_error = ?, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_VERIFYING)
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
    let attempts = task.attempts + 1;
    // Missing results have already consumed the continuation budget, so they
    // block. A malformed result can be corrected on retry and uses the normal
    // attempt budget.
    let mut blocked = (kind == FailureKind::Protocol
        && (protocol_failure_blocks(reason) || attempts >= 3))
        || (!kind.is_agent_failure() && attempts >= 3);
    if kind.is_agent_failure()
        && effect.switch
        && !has_usable_agent(state, &project.id, ROLE_VERIFIER, Some(&agent.id)).await?
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
