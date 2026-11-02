use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::domain::{self, now, FailureKind};
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::state::AppState;

use super::common::{load_task, models_list_json, parse_json_list};
use super::failure::{
    apply_agent_failure, failure_kind_label, has_usable_agent, protocol_failure_blocks,
    task_pause_intent,
};
use super::worktree::prepare_task_worktree;

pub(super) async fn handle_executor_success(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    summary: &str,
    agent: &AgentRow,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    let pause = task_pause_intent(state, &task.id).await?;
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, agent_id = ?, last_agent_id = NULL, \
         last_error = NULL, waiting_reason = NULL, result_summary = ?, updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_VERIFYING)
    .bind(pause.unwrap_or(STATUS_QUEUED))
    .bind(&agent.id)
    .bind(summary)
    .bind(now())
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&task.id),
        "task_executed",
        format!(
            "任务 {} 执行完成，{}",
            task.key,
            if pause.is_some() {
                "任务已暂停，等待恢复后验收"
            } else {
                "等待验收"
            }
        ),
    )
    .await;
    Ok(())
}

pub(super) async fn validate_derive_request(
    state: &AppState,
    project: &ProjectRow,
    task: Option<&TaskRow>,
    tasks: &[domain::NormalizedTask],
) -> Result<Option<String>> {
    let Some(task) = task else {
        return Ok(Some("执行作业缺少任务，无法派生".into()));
    };
    let active_keys: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM tasks WHERE project_id = ? AND COALESCE(status, '') != ?",
    )
    .bind(&project.id)
    .bind(STATUS_CANCELED)
    .fetch_all(&state.db)
    .await?;
    let active_keys: HashSet<String> = active_keys.into_iter().collect();
    Ok(
        domain::validate_derive_scope(task.parent_id.is_some(), &active_keys, tasks)
            .err()
            .map(|reason| format!("派生请求不合法: {reason}")),
    )
}

pub(super) async fn handle_executor_derive(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    summary: &str,
    tasks: &[domain::NormalizedTask],
    agent: &AgentRow,
) -> Result<()> {
    let _guard = state.schedule_lock.lock().await;
    let Some(parent) = load_task(&state.db, &task.id).await? else {
        return Ok(());
    };
    if parent.stage == TASK_STAGE_COMPLETED || parent.status.as_deref() == Some(STATUS_CANCELED) {
        return Ok(());
    }
    if parent.parent_id.is_some() {
        return Err(anyhow!("派生任务不能再次派生"));
    }
    let pause = task_pause_intent(state, &parent.id).await?;

    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let active_keys: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM tasks WHERE project_id = ? AND COALESCE(status, '') != ?",
    )
    .bind(&project.id)
    .bind(STATUS_CANCELED)
    .fetch_all(&mut *tx)
    .await?;
    let active_keys: HashSet<String> = active_keys.into_iter().collect();
    domain::validate_derive_scope(false, &active_keys, tasks)
        .map_err(|reason| anyhow!("{reason}"))?;
    let max_seq: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM tasks WHERE project_id = ?")
            .bind(&project.id)
            .fetch_one(&mut *tx)
            .await?;
    let max_queue: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(queue_order), 0) FROM tasks WHERE project_id = ?")
            .bind(&project.id)
            .fetch_one(&mut *tx)
            .await?;
    let timestamp = now();
    let mut child_keys = Vec::new();
    for (index, child) in tasks.iter().enumerate() {
        let task_id = domain::new_id("tsk");
        sqlx::query(
            "INSERT INTO tasks (id, project_id, seq, queue_order, key, title, description, \
             acceptance, deps, parent_id, stage, status, attempts, rejections, verifier_issues, \
             created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, '[]', ?, ?)",
        )
        .bind(&task_id)
        .bind(&project.id)
        .bind(max_seq + (index + 1) as i64)
        .bind(max_queue + (index + 1) as i64)
        .bind(&child.key)
        .bind(&child.title)
        .bind(&child.description)
        .bind(models_list_json(&child.acceptance))
        .bind(models_list_json(&child.deps))
        .bind(&parent.id)
        .bind(TASK_STAGE_PENDING)
        .bind(STATUS_QUEUED)
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&mut *tx)
        .await?;
        crate::db::ensure_task_agents_tx(&mut tx, &task_id).await?;
        child_keys.push(child.key.clone());
    }
    let mut deps = parse_json_list(&parent.deps);
    for key in &child_keys {
        if !deps.contains(key) {
            deps.push(key.clone());
        }
    }
    sqlx::query(
        "UPDATE tasks SET deps = ?, stage = ?, status = ?, agent_id = ?, last_agent_id = NULL, \
         last_error = NULL, result_summary = ?, updated_at = ? WHERE id = ?",
    )
    .bind(models_list_json(&deps))
    .bind(TASK_STAGE_PENDING)
    .bind(pause.unwrap_or(STATUS_QUEUED))
    .bind(&agent.id)
    .bind(summary)
    .bind(&timestamp)
    .bind(&parent.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project.id),
        Some(&parent.id),
        "task_derived",
        format!(
            "任务 {} 派生 {} 个子任务：{}",
            parent.key,
            child_keys.len(),
            child_keys.join(", ")
        ),
    )
    .await;
    Ok(())
}

pub(super) async fn handle_derived_task_success(
    state: &AppState,
    project: &ProjectRow,
    task: &TaskRow,
    summary: &str,
    agent: &AgentRow,
) -> Result<()> {
    let parent_id = task
        .parent_id
        .clone()
        .ok_or_else(|| anyhow!("任务不是派生任务"))?;
    let parent = load_task(&state.db, &parent_id)
        .await?
        .ok_or_else(|| anyhow!("派生任务的父任务不存在"))?;
    if parent.status.as_deref() == Some(STATUS_CANCELED) {
        return Ok(());
    }
    let parent_worktree = match parent.worktree.as_deref() {
        Some(path) if Path::new(path).join(".git").exists() => PathBuf::from(path),
        _ => prepare_task_worktree(state, project, Some(&parent)).await?,
    };
    let branch = load_task(&state.db, &task.id)
        .await?
        .and_then(|current| current.branch)
        .ok_or_else(|| anyhow!("派生任务缺少执行分支"))?;
    let message = format!("domo(task-{}): {}", task.key, task.title);
    let merge = {
        let _guard = state.git_lock.lock().await;
        git::squash_merge(&parent_worktree, &branch, &message).await
    };
    match merge {
        Ok(_) => {
            let guard = state.schedule_lock.lock().await;
            if let Some(current) = load_task(&state.db, &task.id).await? {
                if current.status.as_deref() == Some(STATUS_CANCELED)
                    || current.stage == TASK_STAGE_COMPLETED
                {
                    return Ok(());
                }
            }
            sqlx::query(
                "UPDATE tasks SET stage = ?, status = NULL, agent_id = ?, last_error = NULL, \
                 result_summary = ?, updated_at = ? WHERE id = ?",
            )
            .bind(TASK_STAGE_COMPLETED)
            .bind(&agent.id)
            .bind(summary)
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
                format!(
                    "派生任务 {} 已跳过验收并合入父任务 {} 的分支",
                    task.key, parent.key
                ),
            )
            .await;
            drop(guard);
            cleanup_task_artifacts(state, project, task).await;
        }
        Err(error) => {
            let reason = format!("合入父任务分支失败：{error:#}");
            handle_executor_failure(state, project, task, &reason, agent, false).await?;
        }
    }
    Ok(())
}

pub(super) async fn cleanup_task_artifacts(state: &AppState, project: &ProjectRow, task: &TaskRow) {
    if let Err(error) = crate::artifacts::cleanup_task(state, project, task, false)
        .await
        .context("清理任务工作区失败")
    {
        tracing::warn!(
            task = %task.id,
            error = %format!("{error:#}"),
            "清理任务工作区失败"
        );
    }
    let Some(parent_id) = task.parent_id.as_deref() else {
        return;
    };
    let parent = match load_task(&state.db, parent_id).await {
        Ok(Some(parent)) => parent,
        _ => return,
    };
    if parent.stage != TASK_STAGE_COMPLETED {
        return;
    }
    if let Err(error) = crate::artifacts::cleanup_task(state, project, &parent, false)
        .await
        .context("清理父任务工作区失败")
    {
        tracing::warn!(
            task = %parent.id,
            error = %format!("{error:#}"),
            "清理父任务工作区失败"
        );
    }
}

pub(super) async fn handle_executor_failure(
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
        && !has_usable_agent(state, &project.id, ROLE_EXECUTOR, Some(&agent.id)).await?
    {
        blocked = true;
    }
    let status = match pause {
        Some(paused) => paused,
        None if blocked => STATUS_BLOCKED,
        None => STATUS_QUEUED,
    };
    sqlx::query(
        "UPDATE tasks SET stage = ?, status = ?, attempts = ?, last_agent_id = ?, \
         last_failure_kind = ?, last_error = ?, agent_id = NULL, waiting_reason = NULL, \
         avoid_agent_id = CASE WHEN ? THEN ? ELSE avoid_agent_id END, \
         avoid_agent_role = CASE WHEN ? THEN ? ELSE avoid_agent_role END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(TASK_STAGE_EXECUTING)
    .bind(status)
    .bind(attempts)
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
        Some(ROLE_EXECUTOR)
    } else {
        None
    })
    .bind(now())
    .bind(&task.id)
    .execute(&state.db)
    .await?;
    let kind_label = failure_kind_label(kind);
    if status == STATUS_BLOCKED {
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            Some(&task.id),
            "task_blocked",
            if kind == FailureKind::Protocol {
                format!("任务 {} 协议失败，已阻塞：{reason}", task.key)
            } else {
                format!(
                    "任务 {} 连续失败或没有可用 Agent，已阻塞（{}）：{reason}",
                    task.key, kind_label
                )
            },
        )
        .await;
    } else {
        emit(
            &state.db,
            &state.bus,
            Some(&project.id),
            Some(&task.id),
            "task_retry",
            format!(
                "任务 {} 执行失败（{}），{}：{reason}",
                task.key,
                kind_label,
                if effect.switch {
                    "已切换 Agent 重试"
                } else {
                    "将在原 Agent 上重试"
                }
            ),
        )
        .await;
    }
    Ok(())
}
