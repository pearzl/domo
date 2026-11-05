//! API 模块内共享的项目/任务查询、标签与排序工具。

use crate::models::*;
use crate::state::AppState;

use super::ApiError;

/// 通用落位计划：校验 position 越界与 Agent 归属，返回移动后的 agent 顺序。
/// `members` 必须已经按目标角色当前顺序排列。
pub(super) fn plan_role_reorder(
    members: Vec<(String,)>,
    scope: &str,
    agent_id: &str,
    position: i64,
) -> Result<Vec<String>, ApiError> {
    if position < 0 || position as usize >= members.len() {
        return Err(ApiError::bad(format!(
            "{scope}该角色的 position 超出条目范围"
        )));
    }
    let Some(index) = members.iter().position(|(id,)| id == agent_id) else {
        return Err(ApiError::bad(format!("{scope}该角色下没有该 Agent")));
    };
    let mut reordered: Vec<String> = members.into_iter().map(|(id,)| id).collect();
    let moved = reordered.remove(index);
    reordered.insert(position as usize, moved);
    Ok(reordered)
}

pub(super) fn confirm_mode(value: &str) -> Result<&'static str, ApiError> {
    match value.trim() {
        CONFIRM_MANUAL => Ok(CONFIRM_MANUAL),
        CONFIRM_AUTO => Ok(CONFIRM_AUTO),
        _ => Err(ApiError::bad("确认方式必须是 manual 或 auto")),
    }
}

pub(super) async fn load_project_task(
    state: &AppState,
    project_id: &str,
    task_id: &str,
) -> Result<TaskRow, ApiError> {
    sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE id = ? AND project_id = ?")
        .bind(task_id)
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("任务不存在"))
}

pub(super) fn role_label(role: &str) -> &'static str {
    match role {
        JOB_ROLE_ANALYSIS => "项目分析",
        JOB_ROLE_DIRECT => "直接完成",
        ROLE_EXECUTOR => "执行",
        ROLE_VERIFIER => "验收",
        JOB_ROLE_ACCEPTANCE => "项目验收",
        JOB_ROLE_SUMMARY => "总结",
        JOB_ROLE_REPLAN_REVIEW => "重规划决策",
        ROLE_PLANNER => "规划",
        _ => "未知",
    }
}

pub(super) fn stage_label(stage: &str) -> &'static str {
    match stage {
        STAGE_ANALYSIS => "项目分析",
        STAGE_EXECUTING => "执行",
        STAGE_SUMMARIZING => "总结",
        STAGE_COMPLETED => "完成",
        _ => "未知",
    }
}

pub(super) fn parse_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

pub(super) async fn require_project(state: &AppState, id: &str) -> Result<ProjectRow, ApiError> {
    sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("项目不存在"))
}
