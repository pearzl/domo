//! 适配器契约：不同 agent（不同 CLI / 模型 / 供应商）对上层必须表现一致。
//!
//! - 能力声明见 `AdapterCapabilities`：会话恢复、spawn、消息发送、过程观察。
//! - 派发形态见 `ExecMode`：`Fresh`（新会话）/ `Resume`（原生恢复：复用**同一 attempt**
//!   与逻辑会话名）/ `Handoff`（交接书重建：同一 attempt、**新**会话 id）。
//!   恢复时不得擅自 `attempt + 1`。
//! - 计划审查与验收的回复必须包含**独占一行**的 `VERDICT: pass` 或 `VERDICT: reject`
//!   （大小写不敏感）；无法解析出结论 = 基建设施失败（可换人重试），**不得**当作"退回"。
//! - 基建失败使用固定前缀（见 `opencode::INFRA_ACCEPT_ERROR_PREFIX`），与业务结论区分；
//!   限流/额度、配置错误、网络抖动、超时等内容失败必须分类上报。
//! - 每次调用的输入（提示词、上下文、注入知识）与完整输出都要归档，供审计回放。

pub mod mock;
pub mod opencode;

/// 一次执行派发的结果（适配器统一返回）。
#[derive(Debug, Clone, Default)]
pub struct ExecOutcome {
    /// 本次执行产出的任务级 artifacts（多为工作区内文件，通常为空）
    pub artifacts: Vec<String>,
    /// 真实 opencode 会话 id（CLI 流中解析；mock 适配器为 None）
    pub session_id: Option<String>,
}

/// 执行派发形态（M4 三档恢复，DESIGN §10）。
#[derive(Debug, Clone)]
pub enum ExecMode {
    /// 常规派发：attempt+1、全新会话
    Fresh,
    /// 第一档·原生会话恢复：沿用同一 attempt 与逻辑会话名，
    /// `opencode run --session <同一真实id>` 直接续作
    Resume { real_id: String },
    /// 第二档·交接摘要重建：同一 attempt、**新**逻辑会话（真实 id ≠ 原 id），携带交接书
    Handoff { session: String, doc: crate::recovery::HandoffDoc },
}

impl ExecMode {
    /// 恢复路径对应的 `task.recovered` tier；常规派发为 None。
    pub fn tier(&self) -> Option<&'static str> {
        match self {
            ExecMode::Fresh => None,
            ExecMode::Resume { .. } => Some(crate::recovery::TIER_NATIVE),
            ExecMode::Handoff { .. } => Some(crate::recovery::TIER_HANDOFF),
        }
    }

    /// 本次派发的逻辑会话名（agent.session_started/ended 的 session_id）。
    /// 第一档必须等于中断前的 `exec_session(task_id, attempt)`（attempt 不自增）。
    pub fn session_name(&self, task_id: &str, attempt: i64) -> String {
        match self {
            ExecMode::Fresh | ExecMode::Resume { .. } => opencode::exec_session(task_id, attempt),
            ExecMode::Handoff { session, .. } => session.clone(),
        }
    }
}

/// Adapter trait — declares required + optional capabilities (DESIGN §10).
/// M1 only ships the mock implementation; future adapters implement this trait.
#[allow(dead_code)]
pub trait AdapterCapabilities {
    fn name(&self) -> &'static str;
    fn supports_resume(&self) -> bool;
    fn supports_spawn(&self) -> bool;
    fn supports_send(&self) -> bool;
    fn supports_observe(&self) -> bool;
}

#[allow(unused_imports)]
pub use mock::MockAdapter;
 // re-export for future adapters
