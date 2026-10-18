pub mod mock;
pub mod opencode;

/// M3 协商：acceptor 对 executor 提出的验收标准的审核结论。
#[derive(Debug, Clone)]
#[allow(dead_code)] // M4 协商退场
pub struct Review {
    /// 是否同意该标准（一致）
    pub agree: bool,
    /// 审核意见（不同意时为反对理由，同意时可为空）
    pub comment: String,
    /// 同意时可给出最终采用的标准（缺省则采用 executor 的提案）
    pub criteria: Option<String>,
}

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
