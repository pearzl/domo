//! 数据模型与术语（产品设计见 docs/DESIGN.md，仓库规范见 docs/DEV.md）。
//!
//! - 层级：工程（完整产品）→ 项目（project，即产品文档中的"项目/迭代"）→ 任务。
//! - 角色：规划者（planner，**兼该任务的验收者**）与执行者（executor）；
//!   一切裁决由人（收件箱）完成，没有决策者 agent 或验收标准协商。
//! - 任务状态：planned / ready / plan_drafting / plan_review / executing /
//!   awaiting_acceptance / done / escalated / suspended / recovering。
//!   `recovering` = 进程重启中断、等待恢复，**不得**统计或展示为"执行中"。
//! - 项目状态：initializing / running / pausing / paused / completed / archived；
//!   `archived` 全程只读。
//! - 验收结论只有**通过 / 退回**；"重试"仅指验收者自身运行失败（限流/超时/配置错）
//!   后的换人重试，不计失败预算、不占退回次数。
//! - 状态一律用字符串字面量落库；**非法转移由调用方拒绝（API 409）**，不得静默纠正；
//!   终态不再被调度器改写。
//! - 新增字段必须可选 + 默认值，保证旧数据缺字段时不崩溃；字段变更需同步 web/src/types.ts。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub confirmed_at: i64,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Engineering {
    pub id: String,
    pub name: String,
    pub brief: String,
    /// 工作区类型：single（锚点是 git 仓库根，任务 worktree）| plain（共享目录，串行）；
    /// multi 为旧数据 legacy 值，行为等同 plain。
    pub workspace_kind: String,
    /// 工作区锚点（绝对路径；single 为仓库根，其余为共享目录/默认工作区）
    pub workspace_path: Option<String>,
    /// 工程默认团队（角色 → agent id 列表）；项目未显式设置时继承
    pub default_agent_pool: Option<Vec<String>>,
    /// 防跑偏约束（技术栈 / 目录约定 / 禁改范围）
    pub constraints: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub id: String,
    /// 所属工程；旧数据/未归属时为 None
    #[serde(default)]
    pub engineering_id: Option<String>,
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
    /// P1 队列人工调序：项目整体优先级（越大越先；默认 0）
    #[serde(default)]
    pub queue_priority: i64,
    /// P2 项目级钩子：{"after_complete": "shell command"}
    #[serde(default)]
    pub hooks: Option<Value>,
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
    /// 状态条件更新（CAS）版本号：每次状态变更 +1，派发时校验，防重复派发。
    #[serde(default)]
    pub version: i64,
    /// P1 队列人工调序：任务优先级（越大越先；默认 0）
    #[serde(default)]
    pub queue_priority: i64,
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

/// M5 项目知识条目：把人工指导沉淀为可复用的项目规范。
///
/// 每条条目随 SQLite 持久化，可增删改查；`source_event_id` 回链到产生它的
/// 人工指导事件（events.id，通常为 `task.guidance_added`），保证来源可追溯。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnowledgeEntry {
    pub id: String,
    /// 来源项目（提升为全局后仍保留，便于审计归属）
    pub project_id: String,
    /// 'project'（仅来源项目可见）| 'global'（全部项目可见并引用）
    pub scope: String,
    /// 规范正文（注入提示词的内容）
    pub body: String,
    /// 适用范围说明（可选，如「全部任务」「仅前端任务」）
    pub applies_to: Option<String>,
    /// 来源人工指导事件（events.id）
    pub source_event_id: i64,
    pub created_at: i64,
    pub updated_at: i64,
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
