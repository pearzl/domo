//! Project consumption authorization. Limits only change through explicit user input.
use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};

use super::{helpers::require_project, ApiError};
use crate::{delivery::Limits, domain::now, state::AppState};

pub(super) async fn set_limits(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(limits): Json<Limits>,
) -> Result<Json<Value>, ApiError> {
    limits.validate().map_err(ApiError::bad)?;
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some("archived") {
        return Err(ApiError::bad("归档项目只读"));
    }
    let timestamp = now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("INSERT INTO project_limits(project_id, cost_budget, task_attempt_limit, replan_limit, updated_at) \
        VALUES(?, ?, ?, ?, ?) ON CONFLICT(project_id) DO UPDATE SET cost_budget = excluded.cost_budget, \
        task_attempt_limit = excluded.task_attempt_limit, replan_limit = excluded.replan_limit, updated_at = excluded.updated_at")
        .bind(&id).bind(limits.cost_budget).bind(limits.task_attempt_limit).bind(limits.replan_limit)
        .bind(&timestamp).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE projects SET status = 'running', blocked_reason = NULL, updated_at = ? \
        WHERE id = ? AND status = 'blocked' AND blocked_reason LIKE '项目费用预算已达到%'",
    )
    .bind(&timestamp)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE tasks SET status = 'queued', last_error = NULL, updated_at = ? \
        WHERE project_id = ? AND status = 'blocked' AND last_error LIKE '任务自动执行尝试已达到%'",
    )
    .bind(&timestamp)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    // A task-only stop may have propagated into the project status.
    sqlx::query(
        "UPDATE projects SET status = 'running', blocked_reason = NULL, updated_at = ? \
        WHERE id = ? AND status = 'blocked' AND blocked_reason LIKE '任务自动执行尝试已达到%'",
    )
    .bind(&timestamp)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO events(project_id, kind, message, payload, created_at) VALUES(?, 'project_limits_set', ?, ?, ?)")
        .bind(&id).bind("用户设置项目消耗限制")
        .bind(json!({"version": 1, "limits": limits}).to_string()).bind(&timestamp)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    state.notify_scheduler();
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn job_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<(i64, String, Option<f64>, Option<String>)> = sqlx::query_as(
        "SELECT id, snapshot, cost, finished_at FROM job_runs WHERE job_id = ? ORDER BY id",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    let workspace: Option<String> = sqlx::query_scalar(
        "SELECT p.workspace FROM projects p JOIN jobs j ON j.project_id = p.id WHERE j.id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?;
    let mut diagnostics = Vec::new();
    if let Some(workspace) = workspace {
        let dir = crate::paths::job_dir(&std::path::PathBuf::from(workspace), &id);
        if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| ApiError::internal(e.into()))?
            {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("diagnostic-") && name.ends_with(".json") {
                    if let Ok(bytes) = tokio::fs::read(entry.path()).await {
                        if let Ok(record) =
                            serde_json::from_slice::<crate::continuation::Checkpoint>(&bytes)
                        {
                            diagnostics.push(record);
                        }
                    }
                }
            }
        }
    }
    diagnostics.sort_by_key(|r| (r.turns, r.protocol_rounds));
    Ok(Json(
        json!({"diagnostics": diagnostics, "runs": rows.into_iter().map(|(id, snapshot, cost, at)| {
        json!({"id": id, "snapshot": serde_json::from_str::<Value>(&snapshot).unwrap_or(Value::Null),
            "cost": cost, "finished_at": at})
    }).collect::<Vec<_>>()}),
    ))
}
