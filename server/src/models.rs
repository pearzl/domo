use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub confirmed_at: i64,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub brief: String,
    pub status: String,
    pub plan: Option<Plan>,
    /// M2: workspace spec JSON ({kind: local|git, path|url, branch?}) or null (DESIGN §13)
    pub workspace: Option<Value>,
    /// M2.5: 项目 agent 池（可用的 agent id 列表；None=全部已配置 agent）
    pub agent_pool: Option<Vec<String>>,
    /// M4: 项目工作区内登记的 git 仓库（[{rel, main}]，confirm 时扫描）
    #[serde(default)]
    pub repos: Vec<RepoRef>,
    pub created_at: i64,
}

/// M4: 项目工作区内的 git 仓库引用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRef {
    /// 相对工作区的路径（"." = 工作区本身是仓库）
    pub rel: String,
    pub main: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub supports_resume: bool,
    #[serde(default = "def_max_parallel")]
    pub max_parallel: i64,
}

fn def_max_parallel() -> i64 {
    1
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities { supports_resume: false, max_parallel: 1 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub adapter: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub config: Value,
    #[serde(default)]
    pub capabilities: Capabilities,
    pub status: String,
    #[serde(default)]
    pub current_task_ids: Vec<String>,
}

/// M3: a milestone is a phase goal + its intended effect, and the anchor of a
/// human-confirmation gate. Status: pending|awaiting_confirm|passed|failed.
#[derive(Debug, Clone, Serialize)]
pub struct Milestone {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub goal: Option<String>,
    pub effect: Option<String>,
    /// member task ids (kept in sync with tasks.milestone_id)
    pub task_ids: Vec<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub milestone_id: Option<String>,
    pub title: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub assignee_role: String,
    pub executor_agent_id: Option<String>,
    pub acceptor_agent_id: Option<String>,
    pub acceptance_required: bool,
    pub acceptance_criteria: Option<String>,
    pub negotiation_status: String,
    /// M3 协商：已完成的协商轮次（持久化，重启后从此轮继续）
    pub negotiation_round: i64,
    /// M4：执行者产出的任务计划（plan_drafting → plan_review → executing 使用）
    pub execution_plan: Option<String>,
    /// M4：计划审查打回次数（≥2 → escalated）
    pub plan_rejection_count: i64,
    /// M4：任务涉及的仓库（相对工作区路径；空 = 缺省主仓库）
    #[serde(default)]
    pub scope_repos: Vec<String>,
    /// M4.5：人类显式指定 agent（1=固定不动态解析；0=按角色+池动态选择）
    #[serde(default)]
    pub agent_pinned: bool,
    /// M4.5：本任务已尝试过的 agent（{role: [agent_id]}），用于失败换人时排除
    #[serde(default)]
    pub tried_agents: Value,
    pub status: String,
    pub attempt: i64,
    pub rejection_count: i64,
    pub failure_count: i64,
    pub artifacts: Vec<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct InboxItem {
    pub id: String,
    pub project_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub payload: Value,
    pub status: String,
    pub resolution: Option<Value>,
    pub created_at: i64,
}

/// M3 决策者自动裁决请求（DESIGN §6 权限分层）。
/// kind: exemption | negotiation_failed | change_review
/// scope: tactical（决策者自决）| strategic（必须升级到人）
/// status: pending | ruled | escalated | down
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub id: String,
    pub project_id: String,
    pub task_id: Option<String>,
    pub kind: String,
    pub request: Value,
    pub scope: String,
    pub status: String,
    pub ruling: Option<Value>,
    pub attempt: i64,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub id: i64,
    pub ts: i64,
    pub project_id: String,
    pub task_id: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Guidance {
    pub id: String,
    pub task_id: String,
    pub text: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub id: i64,
    pub task_id: String,
    pub ts: i64,
    pub line: String,
}

/// M3 项目总结报告（DESIGN §3「COMPLETED → 产出项目总结报告」、§9 汇报/总结）。
///
/// 项目进入 COMPLETED 后由调度器自动触发：planner 角色 agent 汇总事件审计、
/// 里程碑通过/退回、任务完成/退回/失败/挂起统计、验收记录与产物清单，生成
/// Markdown 报告，持久化到数据库与本项目数据目录。
///
/// status: pending | generating | ready | failed
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub id: String,
    pub project_id: String,
    pub status: String,
    /// 完整 Markdown 报告（ready 时存在）
    pub content: Option<String>,
    /// 规划者 agent 产出的叙述段落（报告概述 / 里程碑汇报）
    pub narrative: Option<String>,
    /// 生成失败原因（failed 时存在）
    pub error: Option<String>,
    /// 落盘路径（<data>/projects/<project_id>/summary-report.md）
    pub path: Option<String>,
    /// 结构化事实快照（统计 / 里程碑 / 事件 / 产物），供 API 与排障
    pub facts: Value,
    /// 累计触发次数（含自动与手工重试）
    pub attempts: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub generated_at: Option<i64>,
}
