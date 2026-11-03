//! 项目、项目 Agent 池与任务 Agent 池相关的 HTTP handler。

use axum::extract::{Path, State};
use axum::Json;

use crate::attention::{project_attention, ProjectAttentionState};
use crate::domain::{self, now, short_id};
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::paths;
use crate::replan;
use crate::state::AppState;

use super::agents::validate_role;
use super::detail::load_detail;
use super::helpers::{
    load_project_task, parse_list, plan_role_reorder, require_project, role_label,
};
use super::ApiError;

pub(super) async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProjectSummary>>, ApiError> {
    let rows = sqlx::query_as::<_, ProjectRow>(
        "SELECT * FROM projects ORDER BY updated_at DESC, created_at DESC",
    )
    .fetch_all(&state.db)
    .await?;
    let mut projects = Vec::new();
    for row in rows {
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND COALESCE(status, '') != ?",
        )
        .bind(&row.id)
        .bind(STATUS_CANCELED)
        .fetch_one(&state.db)
        .await?;
        let completed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND stage = 'completed'",
        )
        .bind(&row.id)
        .fetch_one(&state.db)
        .await?;
        let latest_project_job_status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM jobs WHERE project_id = ? AND task_id IS NULL \
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(&row.id)
        .fetch_optional(&state.db)
        .await?;
        let analysis_running: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM jobs WHERE project_id = ? AND task_id IS NULL \
             AND role = 'analysis' AND status = 'running'",
        )
        .bind(&row.id)
        .fetch_one(&state.db)
        .await?;
        let merged_into_base =
            row.merged_at.is_some() || super::detail::project_merged_into_base(&row).await;
        let proposal_raw = if row.replan_active != 0 {
            row.replan_proposal.as_deref()
        } else {
            row.proposal.as_deref()
        };
        let attention = project_attention(ProjectAttentionState {
            stage: &row.stage,
            status: row.status.as_deref(),
            goal: &row.goal,
            blocked_reason: row.blocked_reason.as_deref(),
            proposal_pending: proposal_raw
                .and_then(|raw| serde_json::from_str::<AnalysisProposal>(raw).ok())
                .is_some(),
            report_ready: row
                .report
                .as_deref()
                .is_some_and(|report| !report.is_empty()),
            merged_into_base,
            latest_project_job_failed: latest_project_job_status.as_deref()
                == Some(JOB_STATUS_FAILED),
            analysis_running: analysis_running > 0,
        });
        projects.push(ProjectSummary {
            id: row.id,
            name: row.name,
            goal: row.goal,
            repo_path: row.repo_path,
            base_branch: row.base_branch,
            branch: row.branch,
            stage: row.stage,
            status: row.status,
            blocked_reason: row.blocked_reason,
            attention,
            task_total: total,
            task_completed: completed,
            replan_reason: row.replan_reason,
            replan_source: row.replan_source,
            confirm_mode: row.confirm_mode,
            completion_mode: row.completion_mode,
            merged_at: row.merged_at,
            merged_commit: row.merged_commit,
            created_at: row.created_at,
            updated_at: row.updated_at,
        });
    }
    Ok(Json(projects))
}

pub(super) async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad("项目名称不能为空"));
    }
    let repo_input = crate::platform::expand_tilde(request.repo_path.trim());
    if !repo_input.is_dir() {
        return Err(ApiError::bad("仓库路径不存在"));
    }
    let repo = git::repo_root(&repo_input)
        .await
        .map_err(|error| ApiError::bad(format!("{error:#}")))?;
    let base = match request.base_branch.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => git::current_branch(&repo)
            .await
            .map_err(|error| ApiError::bad(format!("{error:#}")))?,
    };
    git::validate_base(&repo, &base)
        .await
        .map_err(|error| ApiError::bad(format!("base 分支不可用: {error:#}")))?;
    let project_id = domain::new_id("prj");
    let branch = format!("domo/p-{}", short_id(&project_id));
    if git::has_branch(&repo, &branch).await {
        return Err(ApiError::bad(format!("分支 {branch} 已存在")));
    }
    let workspace = paths::project_workspace(&state.cfg.workspace_root, &project_id);
    let repo_worktree = paths::project_repo(&workspace);
    {
        let _guard = state.git_lock.lock().await;
        git::create_project_worktree(&repo, &base, &repo_worktree, &branch)
            .await
            .map_err(|error| ApiError::bad(format!("创建项目 worktree 失败: {error:#}")))?;
    }
    let timestamp = now();
    let result = sqlx::query(
        "INSERT INTO projects (id, name, goal, repo_path, base_branch, branch, workspace, \
         stage, status, blocked_reason, confirm_mode, completion_mode, created_at, updated_at) \
         VALUES (?, ?, '', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&project_id)
    .bind(name)
    .bind(repo.to_string_lossy().to_string())
    .bind(&base)
    .bind(&branch)
    .bind(workspace.to_string_lossy().to_string())
    .bind(STAGE_ANALYSIS)
    .bind(STATUS_BLOCKED)
    .bind("等待输入项目目标")
    .bind(CONFIRM_MANUAL)
    .bind(COMPLETION_TASKS)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&state.db)
    .await;
    if let Err(error) = result {
        let _ = git::remove_worktree(&repo, &repo_worktree).await;
        let _ = crate::platform::remove_dir_all(&workspace).await;
        return Err(error.into());
    }
    crate::db::ensure_project_agents(&state.db, &project_id).await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        None,
        "project_created",
        format!("项目创建，等待输入目标（base 分支 {base}）"),
    )
    .await;
    state.notify_scheduler();
    load_detail(&state, &project_id).await.map(Json)
}

pub(super) async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectDetail>, ApiError> {
    load_detail(&state, &id).await.map(Json)
}

pub(super) async fn get_project_agents(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectAgentPool>, ApiError> {
    require_project(&state, &id).await?;
    let pool = crate::db::load_project_agent_pool(&state.db, &id).await?;
    Ok(Json(pool))
}

pub(super) async fn update_project_agent(
    State(state): State<AppState>,
    Path((project_id, role, agent_id)): Path<(String, String, String)>,
    Json(request): Json<ProjectAgentUpdateRequest>,
) -> Result<Json<ProjectAgentPool>, ApiError> {
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    let role = validate_role(role.trim())?;
    crate::db::ensure_project_agents(&state.db, &project_id).await?;
    let lookup = sqlx::query_as::<_, ProjectAgentRow>(
        "SELECT * FROM project_agents WHERE project_id = ? AND role = ? AND agent_id = ?",
    )
    .bind(&project_id)
    .bind(&role)
    .bind(&agent_id)
    .fetch_optional(&state.db)
    .await?;
    let current = match lookup {
        Some(row) => row,
        // position 调整要求条目必须在该角色下，此时按参数错误处理。
        None if request.position.is_some() => {
            return Err(ApiError::bad("项目 Agent 池中没有该 Agent"))
        }
        None => return Err(ApiError::not_found("项目 Agent 池中没有该 Agent")),
    };
    if let Some(position) = request.position {
        move_project_agent_to(&state, &project_id, &role, &current.agent_id, position).await?;
    }
    if let Some(direction) = request.direction.as_deref().map(str::trim) {
        if !matches!(direction, "up" | "down") {
            return Err(ApiError::bad("direction 必须是 up 或 down"));
        }
        let target = if direction == "up" {
            sqlx::query_as::<_, ProjectAgentRow>(
                "SELECT * FROM project_agents WHERE project_id = ? AND role = ? \
                 AND priority < ? ORDER BY priority DESC LIMIT 1",
            )
            .bind(&project_id)
            .bind(&role)
            .bind(current.priority)
            .fetch_optional(&state.db)
            .await?
        } else {
            sqlx::query_as::<_, ProjectAgentRow>(
                "SELECT * FROM project_agents WHERE project_id = ? AND role = ? \
                 AND priority > ? ORDER BY priority ASC LIMIT 1",
            )
            .bind(&project_id)
            .bind(&role)
            .bind(current.priority)
            .fetch_optional(&state.db)
            .await?
        };
        if let Some(target) = target {
            let timestamp = now();
            let mut tx = state.db.begin().await?;
            sqlx::query(
                "UPDATE project_agents SET priority = ?, updated_at = ? \
                 WHERE project_id = ? AND role = ? AND agent_id = ?",
            )
            .bind(target.priority)
            .bind(&timestamp)
            .bind(&project_id)
            .bind(&role)
            .bind(&current.agent_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE project_agents SET priority = ?, updated_at = ? \
                 WHERE project_id = ? AND role = ? AND agent_id = ?",
            )
            .bind(current.priority)
            .bind(&timestamp)
            .bind(&project_id)
            .bind(&role)
            .bind(&target.agent_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
    }
    if let Some(enabled) = request.enabled {
        sqlx::query(
            "UPDATE project_agents SET enabled = ?, updated_at = ? \
             WHERE project_id = ? AND role = ? AND agent_id = ?",
        )
        .bind(if enabled { 1 } else { 0 })
        .bind(now())
        .bind(&project_id)
        .bind(&role)
        .bind(&current.agent_id)
        .execute(&state.db)
        .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        None,
        "project_agents_updated",
        format!(
            "项目 Agent 池更新{}角色配置（{}）",
            role_label(&role),
            current.agent_id
        ),
    )
    .await;
    state.notify_scheduler();
    let pool = crate::db::load_project_agent_pool(&state.db, &project_id).await?;
    Ok(Json(pool))
}

/// 把项目池角色内指定 Agent 移动到 0 基目标位置，并在事务内把该角色优先级重排为 0..n-1。
/// 其余角色的条目不参与本次重排。
pub(super) async fn move_project_agent_to(
    state: &AppState,
    project_id: &str,
    role: &str,
    agent_id: &str,
    position: i64,
) -> Result<(), ApiError> {
    let members: Vec<(String,)> = sqlx::query_as(
        "SELECT agent_id FROM project_agents WHERE project_id = ? AND role = ? \
         ORDER BY priority ASC, agent_id ASC",
    )
    .bind(project_id)
    .bind(role)
    .fetch_all(&state.db)
    .await?;
    let reordered = plan_role_reorder(members, "项目 Agent 池", agent_id, position)?;
    let timestamp = now();
    let mut tx = state.db.begin().await?;
    for (priority, id) in reordered.iter().enumerate() {
        sqlx::query(
            "UPDATE project_agents SET priority = ?, updated_at = ? \
             WHERE project_id = ? AND role = ? AND agent_id = ?",
        )
        .bind(priority as i64)
        .bind(&timestamp)
        .bind(project_id)
        .bind(role)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 任务池只允许执行者与验收者两个角色；规划者始终使用项目池。
pub(super) fn validate_task_pool_role(role: &str) -> Result<String, ApiError> {
    match role {
        ROLE_EXECUTOR | ROLE_VERIFIER => Ok(role.to_string()),
        ROLE_PLANNER => Err(ApiError::bad("任务 Agent 池只有执行者与验收者两个角色")),
        other => Err(ApiError::bad(format!("未知角色: {other}"))),
    }
}

pub(super) fn ensure_task_pool_mutable(task: &TaskRow) -> Result<(), ApiError> {
    if task.stage == TASK_STAGE_COMPLETED {
        return Err(ApiError::bad("任务已完成，任务 Agent 池只读"));
    }
    if task.status.as_deref() == Some(STATUS_CANCELED) {
        return Err(ApiError::bad("任务已取消，任务 Agent 池只读"));
    }
    Ok(())
}

pub(super) async fn get_task_agent_pool(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> Result<Json<TaskAgentPool>, ApiError> {
    let task = load_project_task(&state, &project_id, &task_id).await?;
    let pool = crate::db::load_task_agent_pool(&state.db, &project_id, &task.id).await?;
    Ok(Json(pool))
}

pub(super) async fn update_task_agent(
    State(state): State<AppState>,
    Path((project_id, task_id, role, agent_id)): Path<(String, String, String, String)>,
    Json(request): Json<ProjectAgentUpdateRequest>,
) -> Result<Json<TaskAgentPool>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档，任务 Agent 池只读"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    ensure_task_pool_mutable(&task)?;
    let role = validate_task_pool_role(role.trim())?;
    crate::db::ensure_task_agents(&state.db, &project_id, &task.id).await?;
    let current = sqlx::query_as::<_, TaskAgentRow>(
        "SELECT * FROM task_agents WHERE task_id = ? AND role = ? AND agent_id = ?",
    )
    .bind(&task.id)
    .bind(&role)
    .bind(&agent_id)
    .fetch_optional(&state.db)
    .await?;
    let current = match current {
        Some(row) => row,
        // position 调整要求条目必须在该角色下，此时按参数错误处理。
        None if request.position.is_some() => {
            return Err(ApiError::bad("任务 Agent 池中没有该 Agent"))
        }
        None => return Err(ApiError::not_found("任务 Agent 池中没有该 Agent")),
    };
    if let Some(position) = request.position {
        move_task_agent_to(&state, &task.id, &role, &current.agent_id, position).await?;
    }
    if let Some(direction) = request.direction.as_deref().map(str::trim) {
        if !matches!(direction, "up" | "down") {
            return Err(ApiError::bad("direction 必须是 up 或 down"));
        }
        let target = if direction == "up" {
            sqlx::query_as::<_, TaskAgentRow>(
                "SELECT * FROM task_agents WHERE task_id = ? AND role = ? \
                 AND priority < ? ORDER BY priority DESC LIMIT 1",
            )
            .bind(&task.id)
            .bind(&role)
            .bind(current.priority)
            .fetch_optional(&state.db)
            .await?
        } else {
            sqlx::query_as::<_, TaskAgentRow>(
                "SELECT * FROM task_agents WHERE task_id = ? AND role = ? \
                 AND priority > ? ORDER BY priority ASC LIMIT 1",
            )
            .bind(&task.id)
            .bind(&role)
            .bind(current.priority)
            .fetch_optional(&state.db)
            .await?
        };
        if let Some(target) = target {
            let timestamp = now();
            let mut tx = state.db.begin().await?;
            sqlx::query(
                "UPDATE task_agents SET priority = ?, updated_at = ? \
                 WHERE task_id = ? AND role = ? AND agent_id = ?",
            )
            .bind(target.priority)
            .bind(&timestamp)
            .bind(&task.id)
            .bind(&role)
            .bind(&current.agent_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE task_agents SET priority = ?, updated_at = ? \
                 WHERE task_id = ? AND role = ? AND agent_id = ?",
            )
            .bind(current.priority)
            .bind(&timestamp)
            .bind(&task.id)
            .bind(&role)
            .bind(&target.agent_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
    }
    if let Some(enabled) = request.enabled {
        sqlx::query(
            "UPDATE task_agents SET enabled = ?, updated_at = ? \
             WHERE task_id = ? AND role = ? AND agent_id = ?",
        )
        .bind(if enabled { 1 } else { 0 })
        .bind(now())
        .bind(&task.id)
        .bind(&role)
        .bind(&current.agent_id)
        .execute(&state.db)
        .await?;
    }
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        Some(&task.id),
        "task_agents_updated",
        format!(
            "任务 {} 更新{}角色配置（{}）",
            task.key,
            role_label(&role),
            current.agent_id
        ),
    )
    .await;
    state.notify_scheduler();
    let pool = crate::db::load_task_agent_pool(&state.db, &project_id, &task.id).await?;
    Ok(Json(pool))
}

/// 把任务池角色内指定 Agent 移动到 0 基目标位置，并在事务内把该角色优先级
/// 重排为 0..n-1，其他角色不受影响。
pub(super) async fn move_task_agent_to(
    state: &AppState,
    task_id: &str,
    role: &str,
    agent_id: &str,
    position: i64,
) -> Result<(), ApiError> {
    let members: Vec<(String,)> = sqlx::query_as(
        "SELECT agent_id FROM task_agents WHERE task_id = ? AND role = ? \
         ORDER BY priority ASC, agent_id ASC",
    )
    .bind(task_id)
    .bind(role)
    .fetch_all(&state.db)
    .await?;
    let reordered = plan_role_reorder(members, "任务 Agent 池", agent_id, position)?;
    let timestamp = now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    for (priority, id) in reordered.iter().enumerate() {
        sqlx::query(
            "UPDATE task_agents SET priority = ?, updated_at = ? \
             WHERE task_id = ? AND role = ? AND agent_id = ?",
        )
        .bind(priority as i64)
        .bind(&timestamp)
        .bind(task_id)
        .bind(role)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 旧的任务 Agent 覆盖接口，作为兼容入口保留：指定 Agent 映射为“只含该
/// Agent 的单 Agent 池”，清除覆盖映射为“重置为项目池快照”。旧字段只是
/// 只读兼容展示，调度只看任务池。
pub(super) async fn set_task_agents(
    State(state): State<AppState>,
    Path((project_id, task_id)): Path<(String, String)>,
    Json(request): Json<TaskAgentsRequest>,
) -> Result<Json<TaskView>, ApiError> {
    let _guard = state.schedule_lock.lock().await;
    let project = require_project(&state, &project_id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    let task = load_project_task(&state, &project_id, &task_id).await?;
    // 兼容入口同样先完成两角色的懒初始化，避免只写入一个角色。
    crate::db::ensure_task_agents(&state.db, &project_id, &task.id).await?;
    let mut changes: Vec<(&str, &str)> = Vec::new();
    if let Some(value) = &request.executor_agent_id {
        changes.push((ROLE_EXECUTOR, value.as_deref().unwrap_or("")));
    }
    if let Some(value) = &request.verifier_agent_id {
        changes.push((ROLE_VERIFIER, value.as_deref().unwrap_or("")));
    }
    for (role, raw) in &changes {
        let mut snapshot = String::new();
        if !raw.is_empty() {
            snapshot = resolve_task_agent_from_single(&state, raw, role).await?;
        }
        if snapshot.is_empty() {
            crate::db::reset_task_agent_role(&state.db, &project_id, &task.id, role).await?;
        } else {
            crate::db::set_single_task_agent(&state.db, &task.id, role, &snapshot).await?;
        }
    }
    if !changes.is_empty() {
        let executor = if changes.iter().any(|(role, _)| role == &ROLE_EXECUTOR) {
            request
                .executor_agent_id
                .as_ref()
                .and_then(|value| value.as_ref())
                .cloned()
        } else {
            task.executor_agent_id.clone()
        };
        let verifier = if changes.iter().any(|(role, _)| role == &ROLE_VERIFIER) {
            request
                .verifier_agent_id
                .as_ref()
                .and_then(|value| value.as_ref())
                .cloned()
        } else {
            task.verifier_agent_id.clone()
        };
        sqlx::query(
            "UPDATE tasks SET executor_agent_id = ?, verifier_agent_id = ?, updated_at = ? \
             WHERE id = ?",
        )
        .bind(&executor)
        .bind(&verifier)
        .bind(now())
        .bind(&task.id)
        .execute(&state.db)
        .await?;
        emit(
            &state.db,
            &state.bus,
            Some(&project_id),
            Some(&task.id),
            "task_agents_updated",
            format!(
                "任务 {} 的 Agent 覆盖已更新（执行：{}，验收：{}）",
                task.key,
                executor.as_deref().unwrap_or("跟随项目池"),
                verifier.as_deref().unwrap_or("跟随项目池")
            ),
        )
        .await;
    }
    state.notify_scheduler();
    let row = load_project_task(&state, &project_id, &task.id).await?;
    Ok(Json(TaskView::from_row(row)))
}

pub(super) async fn resolve_task_agent_from_single(
    state: &AppState,
    agent_id: &str,
    role: &str,
) -> Result<String, ApiError> {
    let Some(agent) = crate::db::load_agent(&state.db, agent_id).await? else {
        return Err(ApiError::not_found(format!("Agent {agent_id} 不存在")));
    };
    if !parse_list(&agent.roles).iter().any(|item| item == role) {
        return Err(ApiError::bad(format!(
            "Agent {} 不能承担{}角色",
            agent.name,
            role_label(role)
        )));
    }
    Ok(agent.id)
}

pub(super) async fn update_project_goal(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateGoalRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成，不能修改目标"));
    }
    if project.goal_locked_at.is_none() {
        return Err(ApiError::bad("首次方案确认前请在项目分析中输入目标"));
    }
    let goal = request.goal.trim();
    if goal.is_empty() {
        return Err(ApiError::bad("项目目标不能为空"));
    }
    if goal == project.goal.trim() {
        return Err(ApiError::bad("新项目目标与当前目标相同"));
    }
    if replan::change_goal(&state, &id, goal).await?.is_none() {
        return Err(ApiError::conflict("项目状态已变化，请刷新后重试"));
    }
    state.notify_scheduler();
    load_detail(&state, &id).await.map(Json)
}

pub(super) async fn abandon_replan(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.replan_active == 0 {
        return Err(ApiError::bad("当前没有进行中的重规划"));
    }
    if crate::replan::cancel(&state, &id).await?.is_none() {
        return Err(ApiError::conflict(
            "规划者仍在分析或项目状态已变化，请稍后重试",
        ));
    }
    state.notify_scheduler();
    load_detail(&state, &id).await.map(Json)
}

pub(super) async fn replan_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ReplanRequest>,
) -> Result<Json<ProjectDetail>, ApiError> {
    let project = require_project(&state, &id).await?;
    if project.status.as_deref() == Some(STATUS_ARCHIVED) {
        return Err(ApiError::bad("项目已归档"));
    }
    if project.stage == STAGE_COMPLETED {
        return Err(ApiError::bad("项目已完成"));
    }
    if !matches!(project.stage.as_str(), STAGE_EXECUTING | STAGE_SUMMARIZING) {
        return Err(ApiError::bad("只有执行或总结阶段可以重新规划"));
    }
    let reason = request
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad("重新规划必须填写原因"))?;
    let outcome = replan::request(&state, &id, reason, REPLAN_SOURCE_HUMAN).await?;
    let Some(summary) = outcome else {
        return Err(ApiError::bad("项目状态已变化，未执行重新规划"));
    };
    tracing::info!(
        project_id = %id,
        canceled_tasks = summary.canceled_tasks,
        retained_tasks = summary.retained_tasks,
        canceled_jobs = summary.canceled_jobs,
        "项目已请求重规划，等待方案确认"
    );
    state.notify_scheduler();
    load_detail(&state, &id).await.map(Json)
}
