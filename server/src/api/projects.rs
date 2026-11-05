//! 项目、项目 Agent 池与任务 Agent 池相关的 HTTP handler。

use axum::extract::{Path, State};
use axum::Json;

use crate::domain::{self, now, short_id};
use crate::events::emit;
use crate::git;
use crate::models::*;
use crate::paths;
use crate::replan;
use crate::state::AppState;

use super::agents::validate_role;
use super::detail::load_detail;
use super::helpers::{load_project_task, plan_role_reorder, require_project, role_label};
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
        let merged_into_base = row.is_git()
            && (row.merged_at.is_some() || super::detail::project_merged_into_base(&row).await);
        projects.push(ProjectSummary {
            attention_items: crate::attention::items(&state.db, &row, merged_into_base).await?,
            id: row.id,
            name: row.name,
            goal: row.goal,
            repo_path: row.repo_path,
            base_branch: row.base_branch,
            branch: row.branch,
            workspace_type: row.workspace_type,
            stage: row.stage,
            status: row.status,
            blocked_reason: row.blocked_reason,
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
    let workspace_type = match request
        .workspace_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None | Some(WORKSPACE_GIT) => WORKSPACE_GIT,
        Some(WORKSPACE_PLAIN) => WORKSPACE_PLAIN,
        Some(other) => {
            return Err(ApiError::bad(format!(
                "未知工作区类型: {other}（可选 git 或 plain）"
            )))
        }
    };
    let confirm = match request
        .confirm_mode
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None | Some(CONFIRM_MANUAL) => CONFIRM_MANUAL,
        Some(CONFIRM_AUTO) => CONFIRM_AUTO,
        Some(other) => {
            return Err(ApiError::bad(format!(
                "未知方案确认方式: {other}（可选 manual 或 auto）"
            )))
        }
    };
    let goal = request
        .goal
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    let repo_input = crate::platform::expand_tilde(request.repo_path.trim());
    if request.repo_path.trim().is_empty() {
        return Err(ApiError::bad("工作区目录不能为空"));
    }
    // 创建与派发共用锁，目录检查和占用登记不能被另一创建请求穿插。
    let _guard = state.schedule_lock.lock().await;
    let project_id = domain::new_id("prj");
    let workspace = paths::project_workspace(&state.cfg.workspace_root, &project_id);
    let (repo, base, branch, repo_worktree) = if workspace_type == WORKSPACE_PLAIN {
        // 普通工作区直接在用户选择的本地目录中推进，不要求、也不初始化 Git。
        if repo_input.exists() {
            if !repo_input.is_dir() {
                return Err(ApiError::bad("本地目录不是一个目录"));
            }
        } else {
            tokio::fs::create_dir_all(&repo_input)
                .await
                .map_err(|error| ApiError::bad(format!("创建本地目录失败: {error:#}")))?;
        }
        let repo = repo_input
            .canonicalize()
            .unwrap_or_else(|_| repo_input.clone());
        let occupied: Vec<String> = sqlx::query_scalar(
            "SELECT repo_path FROM projects WHERE workspace_type = 'plain' \
             AND COALESCE(status, '') != 'archived'",
        )
        .fetch_all(&state.db)
        .await?;
        for path in occupied {
            let other = std::path::PathBuf::from(path);
            let other = other.canonicalize().unwrap_or(other);
            if repo.starts_with(&other) || other.starts_with(&repo) {
                return Err(ApiError::conflict(format!(
                    "目录与未归档普通工作区 {} 重叠，请先归档原项目或选择独立目录",
                    other.display()
                )));
            }
        }
        (repo, String::new(), String::new(), None)
    } else {
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
        let branch = format!("domo/p-{}", short_id(&project_id));
        if git::has_branch(&repo, &branch).await {
            return Err(ApiError::bad(format!("分支 {branch} 已存在")));
        }
        let repo_worktree = paths::project_repo(&workspace);
        {
            let _guard = state.git_lock.lock().await;
            git::create_project_worktree(&repo, &base, &repo_worktree, &branch)
                .await
                .map_err(|error| ApiError::bad(format!("创建项目 worktree 失败: {error:#}")))?;
        }
        (repo, base, branch, Some(repo_worktree))
    };
    if let Err(error) = tokio::fs::create_dir_all(&workspace).await {
        if let Some(worktree) = repo_worktree.as_deref() {
            let _ = git::remove_worktree(&repo, worktree).await;
        }
        return Err(ApiError::bad(format!("创建项目目录失败: {error:#}")));
    }
    let timestamp = now();
    let status = if goal.is_empty() {
        STATUS_BLOCKED
    } else {
        STATUS_RUNNING
    };
    let blocked_reason = goal.is_empty().then_some("等待输入项目目标");
    let result = sqlx::query(
        "INSERT INTO projects (id, name, goal, repo_path, base_branch, branch, workspace, \
         workspace_type, stage, status, blocked_reason, confirm_mode, completion_mode, \
         created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&project_id)
    .bind(name)
    .bind(&goal)
    .bind(repo.to_string_lossy().to_string())
    .bind(&base)
    .bind(&branch)
    .bind(workspace.to_string_lossy().to_string())
    .bind(workspace_type)
    .bind(STAGE_ANALYSIS)
    .bind(status)
    .bind(blocked_reason)
    .bind(confirm)
    .bind(COMPLETION_TASKS)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&state.db)
    .await;
    if let Err(error) = result {
        if let Some(worktree) = repo_worktree.as_deref() {
            let _ = git::remove_worktree(&repo, worktree).await;
        }
        let _ = crate::platform::remove_dir_all(&workspace).await;
        return Err(error.into());
    }
    if !goal.is_empty() {
        sqlx::query(
            "INSERT INTO messages (id, project_id, role, content, created_at) \
             VALUES (?, ?, 'user', ?, ?)",
        )
        .bind(domain::new_id("msg"))
        .bind(&project_id)
        .bind(&goal)
        .bind(&timestamp)
        .execute(&state.db)
        .await?;
    }
    crate::db::ensure_project_agents(&state.db, &project_id).await?;
    let created = require_project(&state, &project_id).await?;
    crate::resources::shared_snapshot(&state, &created, None, None).await?;
    emit(
        &state.db,
        &state.bus,
        Some(&project_id),
        None,
        "project_created",
        if workspace_type == WORKSPACE_PLAIN {
            format!("项目创建，普通工作区 {}", repo.display())
        } else if goal.is_empty() {
            format!("项目创建，等待输入目标（base 分支 {base}）")
        } else {
            format!("项目创建（base 分支 {base}）")
        },
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
    let _guard = state.schedule_lock.lock().await;
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
            let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
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
    crate::delivery::wake_configuration(&state.db, Some(&project_id)).await?;
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
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
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
            let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
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
    crate::delivery::wake_configuration(&state.db, Some(&project_id)).await?;
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
