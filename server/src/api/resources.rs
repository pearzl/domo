//! Read-only versioned resources and explicit workspace inspection/permissions.
use super::{
    helpers::{load_project_task, require_project},
    ApiError,
};
use crate::{domain::now, git, models::*, resources, state::AppState};
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Default, Deserialize)]
pub(super) struct SharedQuery {
    version: Option<String>,
    topic: Option<String>,
}

pub(super) async fn get_shared(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<SharedQuery>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    let mut result = resources::shared_snapshot(&state, &project, None, None).await?;
    if let Some(version) = query.version {
        if version.len() != 40 || !version.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(ApiError::bad("共享版本必须是完整提交哈希"));
        }
        let repo = resources::shared_repo(&project);
        if !git::is_ancestor(&repo, &version, "HEAD").await? {
            return Err(ApiError::bad("版本不属于本项目共享仓库历史"));
        }
        let paths = git::run_git(
            &repo,
            &["ls-tree", "-r", "--name-only", &version, "records/"],
        )
        .await?;
        let mut records = Vec::new();
        for path in paths.lines() {
            let raw = git::run_git(&repo, &["show", &format!("{version}:{path}")]).await?;
            records.push(
                serde_json::from_str::<Value>(&raw).map_err(|e| ApiError::bad(e.to_string()))?,
            );
        }
        result["superseded_ids"] = json!(records
            .iter()
            .flat_map(|record| record["supersedes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str))
            .collect::<Vec<_>>());
        result["version"] = json!(version);
        result["records"] = json!(records);
    }
    if let Some(topic) = query.topic {
        result["records"] = json!(result["records"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter(|r| r["topic"].as_str().is_some_and(|s| s.contains(&topic)))
            .cloned()
            .collect::<Vec<_>>());
    }
    Ok(Json(result))
}

pub(super) async fn post_shared(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(records): Json<Vec<resources::SharedInput>>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("归档项目只读"));
    }
    resources::publish(&state, &project, None, None, &records)
        .await
        .map_err(|e| ApiError::bad(format!("{e:#}")))?;
    get_shared(State(state), Path(id), Query(SharedQuery::default())).await
}

pub(super) async fn list_artifacts(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    Ok(Json(resources::artifact_snapshot(&state, &project).await?))
}

#[derive(Deserialize)]
pub(super) struct ArtifactRequest {
    #[serde(flatten)]
    artifact: resources::ArtifactInput,
    task_id: Option<String>,
}

pub(super) async fn put_artifact(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ArtifactRequest>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("归档项目只读"));
    }
    if let Some(task) = &request.task_id {
        load_project_task(&state, &id, task).await?;
    }
    resources::register_artifact(
        &state,
        &project,
        request.task_id.as_deref(),
        None,
        &request.artifact,
    )
    .await
    .map_err(|e| ApiError::bad(format!("{e:#}")))?;
    list_artifacts(State(state), Path(id)).await
}

#[derive(Deserialize)]
pub(super) struct InspectRequest {
    reason: String,
}
pub(super) async fn inspect(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<InspectRequest>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("归档项目只读"));
    }
    resources::acknowledge(&state, &project, &request.reason)
        .await
        .map_err(|e| ApiError::bad(format!("{e:#}")))?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub(super) struct PermissionRequest {
    path: String,
    task_id: Option<String>,
    reason: String,
}
pub(super) async fn permit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<PermissionRequest>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) || request.reason.trim().is_empty() {
        return Err(ApiError::bad("项目只读或缺少原地修改的理由"));
    }
    resources::contained(&resources::delivery_root(&project), &request.path)
        .await
        .map_err(|e| ApiError::bad(e.to_string()))?;
    if let Some(task) = &request.task_id {
        load_project_task(&state, &id, task).await?;
    }
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("INSERT INTO file_permissions(project_id, task_id, path, reason) VALUES(?, ?, ?, ?) ON CONFLICT(project_id, task_id, path) DO UPDATE SET reason = excluded.reason")
        .bind(&id).bind(request.task_id.as_deref().unwrap_or("")).bind(&request.path).bind(&request.reason).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO events(project_id, task_id, kind, message, payload, created_at) VALUES(?, ?, 'file_write_authorized', ?, ?, ?)")
        .bind(&id).bind(&request.task_id).bind(&request.reason).bind(json!({"version": 1, "path": request.path, "task_id": request.task_id}).to_string()).bind(now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Default, Deserialize)]
pub(super) struct ArtifactQuery {
    commit: Option<String>,
}
pub(super) async fn preview(
    State(state): State<AppState>,
    Path((id, artifact)): Path<(String, String)>,
    Query(query): Query<ArtifactQuery>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    let (path, current): (String, Option<String>) =
        sqlx::query_as("SELECT path, commit_id FROM artifacts WHERE project_id = ? AND id = ?")
            .bind(&id)
            .bind(&artifact)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::not_found("产物不存在"))?;
    resources::safe_relative(&path).map_err(|e| ApiError::bad(e.to_string()))?;
    let versions: Vec<String> = sqlx::query_scalar(
        "SELECT commit_id FROM artifact_versions WHERE artifact_id = ? ORDER BY id DESC",
    )
    .bind(&artifact)
    .fetch_all(&state.db)
    .await?;
    let selected = query.commit.or(current);
    let text = if project.is_git() {
        let commit = selected
            .as_deref()
            .ok_or_else(|| ApiError::bad("产物尚未提交"))?;
        if !versions.iter().any(|v| v == commit) {
            return Err(ApiError::bad("只能查看本产物已登记的版本"));
        }
        let object = format!("{commit}:{path}");
        let size = git::run_git(
            &PathBuf::from(&project.repo_path),
            &["cat-file", "-s", &object],
        )
        .await?;
        if size.trim().parse::<u64>().unwrap_or(u64::MAX) > 1_048_576 {
            return Err(ApiError::bad("产物超过 1MiB，请打开所在目录查看"));
        }
        git::run_git(&PathBuf::from(&project.repo_path), &["show", &object]).await?
    } else {
        if selected.is_some() {
            return Err(ApiError::bad("普通工作区不提供历史产物回溯"));
        }
        let full = resources::contained(&resources::delivery_root(&project), &path)
            .await
            .map_err(|e| ApiError::bad(e.to_string()))?;
        if tokio::fs::metadata(&full)
            .await
            .map_err(|e| ApiError::bad(e.to_string()))?
            .len()
            > 1_048_576
        {
            return Err(ApiError::bad("产物超过 1MiB，请打开所在目录查看"));
        }
        String::from_utf8(
            tokio::fs::read(full)
                .await
                .map_err(|e| ApiError::bad(e.to_string()))?,
        )
        .map_err(|_| ApiError::bad("二进制产物请打开所在目录查看"))?
    };
    Ok(Json(
        json!({"path": path, "text": text, "commit": selected, "versions": versions}),
    ))
}

pub(super) async fn reveal(
    State(state): State<AppState>,
    Path((id, artifact)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let project = require_project(&state, &id).await?;
    let path: String =
        sqlx::query_scalar("SELECT path FROM artifacts WHERE project_id = ? AND id = ?")
            .bind(&id)
            .bind(&artifact)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::not_found("产物不存在"))?;
    let full = resources::contained(&resources::delivery_root(&project), &path)
        .await
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let directory = full
        .parent()
        .ok_or_else(|| ApiError::bad("产物没有父目录"))?;
    let command = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    tokio::process::Command::new(command)
        .arg(directory)
        .spawn()
        .map_err(|e| ApiError::bad(format!("无法打开目录 {}：{e}", directory.display())))?;
    Ok(Json(json!({"directory": directory})))
}

pub(super) async fn global_queue(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::queue::view(&state.db).await?))
}

#[derive(Deserialize)]
pub(super) struct QueuePosition {
    position: usize,
}
pub(super) async fn reorder(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<QueuePosition>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    crate::queue::reorder(&state.db, &id, request.position)
        .await
        .map_err(|e| ApiError::conflict(e.to_string()))?;
    state.notify_scheduler();
    Ok(Json(crate::queue::view(&state.db).await?))
}

#[derive(Deserialize)]
pub(super) struct MergeRequest {
    agent_id: String,
    risk_confirmed: bool,
}
pub(super) async fn request_merge(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<MergeRequest>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &id).await?;
    if !project.is_git()
        || project.stage != STAGE_COMPLETED
        || project.status.as_deref() == Some(STATUS_ARCHIVED)
    {
        return Err(ApiError::bad(
            "只有未归档且已完成的 Git 项目可以处理合入冲突",
        ));
    }
    if !request.risk_confirmed {
        return Err(ApiError::bad(
            "必须确认人工指定可能绕过角色、可用性及产生额外费用的风险",
        ));
    }
    let agent = crate::db::load_agent(&state.db, &request.agent_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Agent 不存在"))?;
    if let Some(reason) = crate::delivery::gate(&state.db, &id, None).await? {
        return Err(ApiError::conflict(reason));
    }
    let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM merge_requests WHERE project_id = ? AND state IN ('waiting', 'running')")
        .bind(&id).fetch_one(&state.db).await?;
    if active != 0 {
        return Err(ApiError::conflict("已有合入冲突处理作业，请等待其结算"));
    }
    let unresolved: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM merge_operations WHERE project_id = ? AND phase NOT IN ('completed', 'rolled_back')")
        .bind(&id).fetch_one(&state.db).await?;
    if unresolved != 0 {
        return Err(ApiError::conflict(
            "合入恢复尚未完成，不能启动 Agent 覆盖恢复现场",
        ));
    }
    let repo = PathBuf::from(&project.repo_path);
    let base = git::commit_of(&repo, &project.base_branch).await?;
    let tip = git::commit_of(&repo, &project.branch).await?;
    if git::is_ancestor(&repo, &tip, &base).await? {
        return Err(ApiError::bad("目标已包含项目提交，无需冲突处理"));
    }
    sqlx::query("INSERT INTO merge_requests(project_id, agent_id, state, base_commit, project_commit, requested_at) VALUES(?, ?, 'waiting', ?, ?, ?) \
        ON CONFLICT(project_id) DO UPDATE SET agent_id = excluded.agent_id, state = 'waiting', job_id = NULL, error = NULL, \
        base_commit = excluded.base_commit, project_commit = excluded.project_commit, requested_at = excluded.requested_at")
        .bind(&id).bind(&agent.id).bind(base).bind(tip).bind(now()).execute(&state.db).await?;
    crate::events::emit(
        &state.db,
        &state.bus,
        Some(&id),
        None,
        "merge_assistance_requested",
        format!("用户确认风险，由人工指定 {} 处理合入冲突", agent.name),
    )
    .await;
    state.notify_scheduler();
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn merge_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_project(&state, &id).await?;
    let result: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT agent_id, state, job_id, error FROM merge_requests WHERE project_id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?;
    Ok(Json(
        json!({"request": result.map(|(agent, state, job, error)| json!({"agent_id": agent, "state": state, "job_id": job, "error": error}))}),
    ))
}

pub(super) async fn cancel_merge(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &id).await?;
    let job: Option<String> = sqlx::query_scalar("SELECT job_id FROM merge_requests WHERE project_id = ? AND state IN ('waiting', 'running')")
        .bind(&id).fetch_optional(&state.db).await?.flatten();
    if let Some(job) = job {
        crate::agents::kill_job_dir(&crate::paths::job_dir(
            std::path::Path::new(&project.workspace),
            &job,
        ));
        sqlx::query("UPDATE jobs SET status = 'canceled', error = '用户停止合入冲突处理，保留文件现场', finished_at = ? WHERE id = ? AND status = 'running'")
            .bind(now()).bind(job).execute(&state.db).await?;
    }
    sqlx::query("UPDATE merge_requests SET state = 'canceled', error = '用户停止处理，保留现场' WHERE project_id = ? AND state IN ('waiting', 'running')")
        .bind(&id).execute(&state.db).await?;
    sqlx::query("UPDATE dispatch_queue SET state = 'obsolete' WHERE project_id = ? AND role = 'merge_conflict' AND state = 'waiting'")
        .bind(&id).execute(&state.db).await?;
    state.notify_scheduler();
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn recommendations(
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let agents = sqlx::query_as::<_, AgentRow>("SELECT * FROM agents ORDER BY created_at, id")
        .fetch_all(&state.db)
        .await?;
    let mut result = Vec::new();
    for agent in agents {
        let (calls, cost, unknown, duration): (i64, f64, i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), CAST(COALESCE(SUM(cost), 0) AS REAL), COALESCE(SUM(cost IS NULL), 0), COALESCE(SUM(duration_ms), 0) FROM accounting_jobs WHERE agent_id = ?",
        ).bind(&agent.id).fetch_one(&state.db).await?;
        let (tasks, completed): (i64, i64) = sqlx::query_as("SELECT COUNT(DISTINCT t.id), COUNT(DISTINCT CASE WHEN t.stage = 'completed' THEN t.id END) \
            FROM tasks t JOIN jobs j ON j.task_id = t.id AND j.role = 'executor' WHERE j.agent_id = ?")
            .bind(&agent.id).fetch_one(&state.db).await?;
        let rejections: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs v WHERE v.role = 'verifier' AND json_valid(v.result) \
            AND json_extract(v.result, '$.pass') = 0 AND EXISTS(SELECT 1 FROM jobs e WHERE e.task_id = v.task_id AND e.role = 'executor' AND e.agent_id = ?)")
            .bind(&agent.id).fetch_one(&state.db).await?;
        let failed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM jobs WHERE agent_id = ? AND status = 'failed'",
        )
        .bind(&agent.id)
        .fetch_one(&state.db)
        .await?;
        let replans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM replan_history h WHERE EXISTS(SELECT 1 FROM jobs j WHERE j.project_id = h.project_id AND j.agent_id = ?)")
            .bind(&agent.id).fetch_one(&state.db).await?;
        let mut advice = Vec::new();
        if tasks < 5 {
            advice.push("样本不足：少于 5 个执行任务，不能据此判断能力档位或性价比。".to_string());
        }
        if rejections > 0 {
            advice.push(format!("关联任务有 {rejections} 次验收拒绝；建议先检查拒绝证据、任务边界与验收者独立性，再由用户决定是否调整角色池或能力投入。"));
        }
        if failed > 0 {
            advice.push(format!("有 {failed} 个失败作业，费用仍计入累计投入；建议区分供应商故障、协议错误与任务失败，不以失败率直接推断模型能力。"));
        }
        if unknown > 0 {
            advice.push(format!(
                "{unknown} 次调用费用未知，已知费用不是完整交付成本，不能据此认定更便宜。"
            ));
        }
        if tasks >= 5 && rejections == 0 {
            advice.push("当前样本未记录验收拒绝；可考虑在相近复杂度任务继续观察，不代表适用于更高风险任务。".to_string());
        }
        result.push(json!({"agent_id": agent.id, "agent_name": agent.name, "calls": calls, "known_cost": cost,
            "unknown_cost_calls": unknown, "duration_ms": duration, "executed_tasks": tasks,
            "completed_tasks": completed, "rejections": rejections, "failed_jobs": failed, "associated_replans": replans,
            "known_cost_per_completed_task": if completed > 0 { Some(cost / completed as f64) } else { None }, "advice": advice}));
    }
    Ok(Json(
        json!({"scope": "global", "automatic_changes": false, "limitations": "任务难度、人工监督时间和供应商价格不完全可比；多人或多 Agent 参与的任务可能关联多个 Agent，不能相加视为唯一归因。费用包含规划、验收、失败和重试。", "items": result}),
    ))
}
