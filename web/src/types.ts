export type ProjectStatus =
  | 'initializing'
  | 'pending_confirm'
  | 'running'
  | 'pausing'
  | 'paused'
  | 'completed'
  | 'archived';

export type TaskStatus =
  | 'planned'
  | 'ready'
  | 'plan_drafting'
  | 'plan_review'
  | 'executing'
  | 'awaiting_acceptance'
  | 'escalated'
  | 'suspended'
  | 'done'
  | 'cancelled';

export type Role = 'planner' | 'executor' | 'acceptor' | 'decision_maker';

export type Adapter = 'mock' | 'opencode';

export type InboxType =
  | 'escalation'
  | 'replan'
  | 'negotiation_failed'
  | 'decision_maker_down'
  | 'milestone_gate';

export type MilestoneStatus = 'pending' | 'awaiting_confirm' | 'passed' | 'failed';

/** M3 里程碑：阶段目标 + 效果定义，人工确认 gate 的锚点 */
export interface Milestone {
  id: string;
  project_id: string;
  name: string;
  goal: string | null;
  effect: string | null;
  /** 与 tasks.milestone_id 同步的任务成员 */
  task_ids: string[];
  status: MilestoneStatus;
}

/** 人工介入方案变更项（loose 结构，由后端逐项校验） */
export type ReplanChange = Record<string, unknown>;

export interface InterveneResult {
  analysis: string;
  reply: string;
  changes: ReplanChange[];
}

export interface ApplyResult {
  applied: { index: number; op: string; task_id?: string; title?: string }[];
  skipped: { index?: number; op?: string; task_id?: string; title?: string; reason: string }[];
}

export type InboxStatus = 'open' | 'resolved';

export type AgentStatus = 'idle' | 'busy';

export interface Workspace {
  kind: 'local' | 'git';
  path?: string; // kind=local
  url?: string; // kind=git
  branch?: string; // kind=git，可选
}

export interface Project {
  id: string;
  name: string;
  brief: string;
  status: ProjectStatus;
  plan?: { confirmed_at?: number | null; content?: unknown } | null;
  workspace: Workspace | null;
  /** 项目 agent 池（null/缺省=全部已配置 agent；规划台可显式选择） */
  agent_pool?: string[] | null;
  created_at: number;
}

export interface Task {
  id: string;
  project_id: string;
  milestone_id: string | null;
  title: string;
  description: string;
  depends_on: string[];
  assignee_role?: string;
  executor_agent_id: string | null;
  acceptor_agent_id: string | null;
  acceptance_required: boolean;
  acceptance_criteria: string | null;
  negotiation_status?: string;
  negotiation_round?: number;
  /** M4 任务计划（执行者产出，验收者审查） */
  execution_plan?: string | null;
  /** M4 计划审查打回轮数 */
  plan_rejection_count?: number;
  status: TaskStatus;
  attempt: number;
  rejection_count: number;
  failure_count: number;
  artifacts: string[];
  created_at: number;
}

export interface LogLine {
  ts: number;
  line: string;
}

export interface AuditEvent {
  id: number;
  ts: number;
  project_id: string;
  task_id: string | null;
  type: string;
  payload: unknown;
}

/** M5 项目知识条目：人工指导沉淀为可复用的项目/全局规范 */
export interface KnowledgeEntry {
  id: string;
  /** 来源项目（提升为全局后仍保留，便于审计归属） */
  project_id: string;
  /** 'project'=仅来源项目可见 | 'global'=全部项目可见并引用 */
  scope: 'project' | 'global';
  /** 规范正文（注入 planner/executor/acceptor 提示词的内容） */
  body: string;
  /** 适用范围说明（可空） */
  applies_to: string | null;
  /** 来源人工指导事件（events.id，回链审计） */
  source_event_id: number;
  created_at: number;
  updated_at: number;
}

export interface Agent {
  id: string;
  name: string;
  adapter: Adapter;
  roles: Role[];
  config: { duration_ms?: number; model?: string; timeout_ms?: number; [k: string]: unknown };
  capabilities: { supports_resume: boolean; max_parallel: number };
  status: AgentStatus;
  current_task_ids: string[];
}

export interface InboxItem {
  id: string;
  project_id: string;
  type: InboxType;
  payload: unknown;
  status: InboxStatus;
  resolution?: string | null;
  created_at: number;
  /** escalation 条目的裁决上下文（GET /inbox 时由后端读时计算，仅 open 项附加） */
  context?: InboxContext;
}

/** 裁决上下文：任务全貌 + 最近失败原因 + 涉及 agent（帮助人判断执行端/验收端问题） */
export interface InboxContext {
  task: {
    id: string;
    title: string;
    status: string;
    description: string;
    acceptance_criteria?: string | null;
    attempt: number;
    failure_count: number;
    rejection_count: number;
  };
  executor?: InboxAgentBrief | null;
  acceptor?: InboxAgentBrief | null;
  /** 最近一次 run 级失败的事件类型：task.execution_failed | acceptance.rejected | acceptance.run_failed */
  last_failure_type?: string | null;
  last_error?: string | null;
  phase_hint?: string;
}

export interface InboxAgentBrief {
  id: string;
  name: string;
  adapter: string;
  roles: Role[];
  status: string;
  model?: string | null;
  duration_ms?: number | null;
}

/** 单条协商轮次记录（executor 提案 / acceptor 审核） */
export interface NegotiationRound {
  round: number;
  role: 'executor' | 'acceptor';
  /** executor 轮：提案内容 */
  proposal?: string;
  /** acceptor 轮：是否同意 */
  agree?: boolean;
  /** acceptor 轮：意见 / 拒绝理由 */
  comment?: string;
  /** acceptor 轮：若同意可直接给出最终标准 */
  criteria?: string | null;
  /** 本轮携带的上轮 acceptor 意见 */
  carried_feedback?: string | null;
  agent_id?: string;
  /** 事件时间戳（由后端从事件流补充） */
  ts?: number;
}

/** 任务详情页附带的协商全貌（后端从事件流重建，跨重启不丢） */
export interface Negotiation {
  status: string;
  round: number;
  max_rounds: number;
  acceptance_criteria: string | null;
  rounds: NegotiationRound[];
}

export interface TaskDetail extends Task {
  logs: LogLine[];
  events: AuditEvent[];
  negotiation?: Negotiation;
}

export type ReportStatus = 'pending' | 'generating' | 'ready' | 'failed';

/**
 * M3 项目总结报告（`GET /api/projects/:id/report` 返回详情；
 * `GET /api/projects/:id` 附带的 `report` 为轻量摘要——缺 content/narrative/facts）。
 */
export interface Report {
  id: string;
  project_id: string;
  status: ReportStatus;
  /** 完整 Markdown 报告（ready 时存在；摘要为空） */
  content?: string | null;
  /** 规划者 agent 产出的叙述段落（摘要不含） */
  narrative?: string | null;
  /** 生成失败原因（failed 时存在） */
  error?: string | null;
  /** 落盘路径 <data>/projects/<id>/summary-report.md */
  path?: string | null;
  /** 结构化事实快照（摘要不含） */
  facts?: Record<string, unknown>;
  /** 累计触发次数（含自动与手工重试） */
  attempts: number;
  created_at: number;
  updated_at: number;
  generated_at?: number | null;
}

/** 重规划上报结果：decision 为后端登记（或命中的未决）决策请求 */
export interface ReplanReportResult {
  decision: { id: string; kind: string; status: string; scope: string };
  reasons: string[];
  scope: 'tactical' | 'strategic';
  deduplicated?: boolean;
}

export interface TaskFormValue {
  title: string;
  description: string;
  depends_on: string[];
  executor_agent_id: string;
  acceptor_agent_id: string;
  acceptance_required: boolean;
  acceptance_criteria: string;
}

export interface AgentFormValue {
  name: string;
  adapter: Adapter;
  roles: Role[];
  max_parallel: number;
  /** mock 专用 */
  duration_ms: number;
  /** opencode 可选 */
  model?: string;
  /** opencode 可选：单次 run 超时（ms） */
  timeout_ms?: number;
}

/** M4.5 项目 agent 池视图（表格数据源） */
export interface PoolView {
  agents: {
    id: string;
    name: string;
    adapter: string;
    status: string;
    circuit_open: boolean;
    roles: Record<string, { enabled: boolean; priority?: number }>;
  }[];
  pool: Record<string, string[]>;
  circuits: { agent_id: string; class: string; remaining_ms: number }[];
}
