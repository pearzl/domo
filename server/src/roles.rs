//! 角色隔离策略（DESIGN §6「裁判不下场」）。
//!
//! 角色本身是**标签**：一个 agent 实例可以同时持有多个角色标签。
//! 但**决策者（decision_maker）有额外约束**——它代表人类在团队中做裁决，
//! 若同时在同一项目里担任 executor / acceptor，等于「裁判既下场又吹哨」，
//! 战术自决（豁免裁定 / 协商失败裁决 / 任务级变更复核）将失去可信度。
//!
//! 因此系统在这几处**拒绝**决策者与执行/验收角色的重叠：
//! - 登记 agent（`POST /api/agents`）与编辑角色（`PATCH /api/agents/:id`）；
//! - 把决策者指派为任务的 executor / acceptor（`POST|PATCH /api/tasks...`）；
//! - 把决策者放进项目 agent 池的 executor / acceptor 优先级列表；
//! - 动态选人（agent 池解析）时跳过决策者实例（历史数据兜底）。
//!
//! 例外：BYO 池过小时，用户可显式打开例外——环境变量 `DOMO_DM_ROLE_OVERLAP=1`
//! （`true`/`yes`/`on` 同义）。打开后该实例可被人工显式指派为执行/验收，
//! 也可参与动态选人——即用户自担「自审自裁」风险。
//!
//! 隔离只作用于**执行/验收**；决策者与 planner 可以共存（规划者只提案，无决定权）。

/// 决策者角色名。
pub const DECISION_MAKER: &str = "decision_maker";
/// 执行者角色名。
pub const EXECUTOR: &str = "executor";
/// 验收者角色名。
pub const ACCEPTOR: &str = "acceptor";

/// 用户是否显式打开了「决策者兼任执行/验收」的例外（BYO 池过小时的兜底）。
pub fn overlap_allowed() -> bool {
    std::env::var("DOMO_DM_ROLE_OVERLAP")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// 角色集合里是否含决策者。
pub fn is_decision_maker(roles: &[String]) -> bool {
    roles.iter().any(|r| r == DECISION_MAKER)
}

/// 角色集合里是否含执行者或验收者。
pub fn is_execution_side(roles: &[String]) -> bool {
    roles.iter().any(|r| r == EXECUTOR || r == ACCEPTOR)
}

/// 角色集合是否违反隔离（决策者 + 执行/验收）。
pub fn has_dm_overlap(roles: &[String]) -> bool {
    is_decision_maker(roles) && is_execution_side(roles)
}

/// 角色集合校验：违反隔离且未打开例外时返回错误信息。
pub fn check_roles(roles: &[String]) -> Result<(), String> {
    check_roles_with(roles, overlap_allowed())
}

/// 同 `check_roles`，但例外开关显式给出（便于单测与策略预演）。
pub fn check_roles_with(roles: &[String], allowed: bool) -> Result<(), String> {
    if has_dm_overlap(roles) && !allowed {
        return Err(overlap_error());
    }
    Ok(())
}

/// 统一的隔离错误文案（前端直接展示）。
pub fn overlap_error() -> String {
    "角色隔离：决策者（decision_maker）不得同时担任 executor / acceptor（裁判不下场）；\
     请登记独立实例，或设置 DOMO_DM_ROLE_OVERLAP=1 显式打开例外"
        .to_string()
}

/// 某个 agent 实例是否因「决策者」身份而被禁止承担执行/验收。
pub fn agent_forbidden_for(agent_roles: &[String]) -> bool {
    agent_forbidden_with(agent_roles, overlap_allowed())
}

/// 同 `agent_forbidden_for`，但例外开关显式给出。
pub fn agent_forbidden_with(agent_roles: &[String], allowed: bool) -> bool {
    is_decision_maker(agent_roles) && !allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn dm_alone_is_fine() {
        assert!(!has_dm_overlap(&rs(&["decision_maker"])));
        assert!(check_roles(&rs(&["decision_maker"])).is_ok());
    }

    #[test]
    fn dm_with_executor_or_acceptor_is_overlap() {
        assert!(has_dm_overlap(&rs(&["decision_maker", "executor"])));
        assert!(has_dm_overlap(&rs(&["decision_maker", "acceptor"])));
        assert!(has_dm_overlap(&rs(&["decision_maker", "executor", "acceptor", "planner"])));
        assert!(!has_dm_overlap(&rs(&["executor", "acceptor"])));
        assert!(!has_dm_overlap(&rs(&["planner", "executor"])));
    }

    #[test]
    fn planner_and_dm_may_coexist() {
        assert!(check_roles(&rs(&["planner", "decision_maker"])).is_ok());
    }

    #[test]
    fn overlap_is_rejected_by_default() {
        // 例外未打开 → 拒绝
        assert!(check_roles_with(&rs(&["decision_maker", "executor"]), false).is_err());
        assert!(agent_forbidden_with(&rs(&["decision_maker", "executor"]), false));
        assert!(!agent_forbidden_with(&rs(&["executor"]), false));
        // 例外显式打开 → 允许（BYO 池过小时的兜底）
        assert!(check_roles_with(&rs(&["decision_maker", "executor"]), true).is_ok());
        assert!(!agent_forbidden_with(&rs(&["decision_maker", "executor"]), true));
    }
}
