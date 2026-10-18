//! M4.5 agent 池：角色优先级选人、排队语义、错误分类与熔断、失败换人。
//!
//! 规则（用户约定）：
//! - 任务**不**在规划时绑定 agent；执行时按「角色 + 项目池优先级」动态解析；
//!   人类显式指定 agent 的任务例外（`agent_pinned`，永不动态改派）。
//! - **空闲不是分配依据**：任务始终分配给该角色优先级最高的可用 agent 并在此排队
//!   （agent 忙 → 等待，不降级到低优先级 agent）。
//! - 失败容错：错误分类后优先换下一优先级 agent 重试（不计入失败预算）；
//!   池内候选耗尽才计入 failure_count（3 次挂起）。
//! - 限流/额度类错误 → 该 agent **全局临时熔断**（时长由接入层给出，缺省按 agent 配置）。

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{db, events, models::Task, state::App};

/// 接入层给出的错误分类（审计可见 + 决定处置策略）。
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorClass {
    /// 限流 / 额度不足：全局临时熔断该 agent
    RateLimit,
    /// 配置错误（如 model 无效）：标记不可用，需人工修复
    Config,
    /// 供应商 5xx / 网络 / 流中断：退避后换人
    Transient,
    /// 单次 run 超时：换人（并提示可能是任务过重或超时过短）
    Timeout,
    Unknown,
}

impl ErrorClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorClass::RateLimit => "rate_limit",
            ErrorClass::Config => "config_error",
            ErrorClass::Transient => "transient",
            ErrorClass::Timeout => "timeout",
            ErrorClass::Unknown => "unknown",
        }
    }
}

/// 错误信息 + 处置策略（接入层产出，调度器与审计共用）。
#[derive(Debug, Clone)]
pub struct AgentError {
    pub class: ErrorClass,
    #[allow(dead_code)]
    pub message: String,
    /// 熔断时长（毫秒）；由接入层解析供应商提示或按 agent 配置给出
    pub cooldown_ms: Option<u64>,
    pub disposition: String,
}

/// 接入层错误分类：不同 agent 的限流提示/冷却时长由其配置决定。
pub fn classify_error(agent: &crate::models::Agent, message: &str) -> AgentError {
    let low = message.to_lowercase();
    let cooldown_from_config = |agent: &crate::models::Agent| -> Option<u64> {
        agent
            .config
            .get("cooldown_ms")
            .or_else(|| agent.config.get("rate_limit_cooldown_ms"))
            .and_then(|v| v.as_u64())
            .filter(|v| *v > 0)
    };
    if low.contains("rate limit") || low.contains("quota") || low.contains("429") {
        let cooldown = cooldown_from_config(agent).unwrap_or(10 * 60 * 1000);
        return AgentError {
            class: ErrorClass::RateLimit,
            message: message.to_string(),
            cooldown_ms: Some(cooldown),
            disposition: format!("临时熔断该 agent {}s（接入层给出），期间所有项目跳过；换下一优先级 agent 重试", cooldown / 1000),
        };
    }
    if low.contains("invalid model reference") || low.contains("expected a string starting with") {
        let cooldown = cooldown_from_config(agent).unwrap_or(60 * 60 * 1000);
        return AgentError {
            class: ErrorClass::Config,
            message: message.to_string(),
            cooldown_ms: Some(cooldown),
            disposition: format!("agent 配置错误（model 无效等）→ 熔断 {}s 并提示人工修复；换下一优先级 agent 重试", cooldown / 1000),
        };
    }
    if low.contains("timed out") {
        return AgentError {
            class: ErrorClass::Timeout,
            message: message.to_string(),
            cooldown_ms: None,
            disposition: "单次 run 超时 → 换下一优先级 agent；如反复出现建议上调该 agent 的 timeout_ms 或拆分任务".into(),
        };
    }
    if low.contains("http 500") || low.contains("unavailable") || low.contains("streaming response failed") || low.contains("connection") {
        return AgentError {
            class: ErrorClass::Transient,
            message: message.to_string(),
            cooldown_ms: Some(cooldown_from_config(agent).unwrap_or(60 * 1000)),
            disposition: "供应商/网络暂时性故障 → 短期退避并换下一优先级 agent".into(),
        };
    }
    AgentError {
        class: ErrorClass::Unknown,
        message: message.to_string(),
        cooldown_ms: None,
        disposition: "未分类错误 → 换下一优先级 agent；仍失败则按失败预算处理".into(),
    }
}

// ---------- 熔断（全局，跨项目） ----------

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 该 agent 是否处于熔断窗口内。
pub fn circuit_open(state: &Arc<App>, agent_id: &str) -> bool {
    let c = state.circuits.lock().unwrap();
    c.get(agent_id).map(|(until, _)| *until > now_ms()).unwrap_or(false)
}

/// 打开熔断窗口（全局生效，事件留痕）。
pub fn circuit_open_for(state: &Arc<App>, conn: &Connection, agent: &crate::models::Agent, err: &AgentError) {
    let Some(ms) = err.cooldown_ms else { return };
    let until = now_ms() + ms as i64;
    state.circuits.lock().unwrap().insert(agent.id.clone(), (until, err.class.as_str().to_string()));
    events::push_event(state, conn, "-", Some(&agent.id), "agent.circuit_opened",
        json!({"agent_id": agent.id, "agent": agent.name, "class": err.class.as_str(),
               "cooldown_ms": ms, "until_ms": until, "disposition": err.disposition}));
    tracing::warn!(agent = %agent.name, class = err.class.as_str(), cooldown_ms = ms, "agent circuit opened");
}

/// 熔断快照（UI/审计展示用）。
pub fn circuit_snapshot(state: &Arc<App>) -> Value {
    let c = state.circuits.lock().unwrap();
    let now = now_ms();
    let list: Vec<Value> = c
        .iter()
        .filter(|(_, (until, _))| *until > now)
        .map(|(id, (until, reason))| json!({"agent_id": id, "until_ms": until, "class": reason, "remaining_ms": until - now}))
        .collect();
    Value::Array(list)
}

// ---------- 角色选人 ----------

/// 解析任务某角色应使用的 agent：
/// 1) 人类固定（agent_pinned）→ 直接用已绑定实例（缺失则报错）；
/// 2) 否则按项目池优先级取第一个可用候选（跳过已删除/熔断/已尝试）；
/// 3) 全部候选不可用 → None（等待，不降级）。
pub fn resolve_role(
    state: &Arc<App>,
    conn: &Connection,
    project_id: &str,
    task: &Task,
    role: &str,
) -> Option<String> {
    let bound = if role == "acceptor" {
        task.acceptor_agent_id.clone()
    } else {
        task.executor_agent_id.clone()
    };
    if task.agent_pinned {
        return bound.filter(|id| db::get_agent(conn, id).is_some());
    }
    let pool = db::project_pool(conn, project_id);
    let tried = db::task_tried(conn, &task.id);
    let tried_ids: Vec<String> = tried
        .get(role)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let candidates: Vec<String> = match pool.get(role) {
        Some(list) => list.clone(),
        // 池未配置该角色 → 缺省：所有具备该角色者按登记顺序
        None => db::list_agents(conn)
            .into_iter()
            .filter(|a| a.roles.iter().any(|r| r == role))
            .map(|a| a.id)
            .collect(),
    };
    for id in candidates {
        if tried_ids.iter().any(|t| t == &id) {
            continue;
        }
        let Some(a) = db::get_agent(conn, &id) else { continue };
        if a.status == "deleted" || !a.roles.iter().any(|r| r == role) {
            continue;
        }
        if circuit_open(state, &id) {
            continue;
        }
        return Some(id);
    }
    None
}

/// 失败换人：把当前 agent 记入 tried，解绑（若非 pinned），返回是否换人成功。
pub fn switch_on_failure(
    state: &Arc<App>,
    conn: &Connection,
    task: &Task,
    role: &str,
    reason: &str,
) -> bool {
    if task.agent_pinned {
        return false; // 人类指定：不自动换人
    }
    let current = if role == "acceptor" {
        task.acceptor_agent_id.clone()
    } else {
        task.executor_agent_id.clone()
    };
    if let Some(cur) = &current {
        db::task_tried_add(conn, &task.id, role, cur);
    }
    // 重新解析（tried 已排除当前）
    let fresh = db::get_task(conn, &task.id).unwrap_or_else(|| task.clone());
    let next = resolve_role(state, conn, &task.project_id, &fresh, role);
    match next {
        Some(next_id) => {
            db::set_task_agent(conn, &task.id, role, Some(&next_id), false);
            let name = db::get_agent(conn, &next_id).map(|a| a.name).unwrap_or_default();
            events::push_event(state, conn, &task.project_id, Some(&task.id), "agent.switched",
                json!({"role": role, "from": current, "to": next_id, "to_name": name,
                       "reason": reason, "note": "失败容错：换下一优先级 agent 重试（不计入失败预算）"}));
            events::log_line_state(state, &task.id, &format!("失败换人：{role} → {name}（{reason}）"));
            true
        }
        None => false,
    }
}


/// 供 UI：池 + 各 agent 角色配置 + 熔断状态（规划台表格数据源）。
pub fn pool_view(state: &Arc<App>, conn: &Connection, project_id: &str) -> Value {
    let agents = db::list_agents(conn);
    let pool = db::project_pool(conn, project_id);
    let circuits = circuit_snapshot(state);
    let table: Vec<Value> = agents
        .iter()
        .map(|a| {
            let roles: HashMap<String, Value> = a
                .roles
                .iter()
                .map(|role| {
                    let entry = pool
                        .get(role)
                        .and_then(|list| list.iter().position(|id| id == &a.id))
                        .map(|idx| json!({"enabled": true, "priority": idx as i64}))
                        .unwrap_or_else(|| json!({"enabled": false}));
                    (role.clone(), entry)
                })
                .collect();
            let open = circuits
                .as_array()
                .map(|l| l.iter().any(|c| c["agent_id"].as_str() == Some(a.id.as_str())))
                .unwrap_or(false);
            json!({"id": a.id, "name": a.name, "adapter": a.adapter, "status": a.status,
                   "circuit_open": open, "roles": roles})
        })
        .collect();
    json!({"agents": table, "pool": pool, "circuits": circuits})
}
