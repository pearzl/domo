export type AgentKind = "mock" | "opencode" | "cli" | "codex" | "workbuddy";
export type Role = "planner" | "executor" | "verifier";
export type UsageMetric = "tokens" | "input_tokens" | "output_tokens" | "cost";
export type UsageWindowKind = "day" | "month";

export interface Health {
  ok: boolean;
  version: string;
  git_hash: string;
}

export interface AvailabilitySlot {
  /** 1 = Monday, 7 = Sunday. */
  weekday: number;
  start: string;
  end: string;
}

export interface AgentAvailability {
  enabled: boolean;
  slots: AvailabilitySlot[];
}

export interface AgentUsageLimit {
  enabled: boolean;
  metric: UsageMetric;
  window: UsageWindowKind;
  limit: number;
}

export interface AgentUsageWindow {
  metric: UsageMetric;
  window: UsageWindowKind;
  window_start: string;
  window_end: string;
  used: number;
  limit: number;
  exceeded: boolean;
}

/** 从原始错误派生的可读摘要；原文仍保留在 error/last_error。 */
export interface FailureSummary {
  kind: string;
  message: string;
  status: number | null;
  retry_after_ms: number | null;
}

export type UsageAggregateMetric =
  | "input_tokens"
  | "output_tokens"
  | "cache_read_tokens"
  | "cache_write_tokens"
  | "reasoning_tokens"
  | "cost";

export interface UsageTotals {
  jobs: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  reasoning_tokens: number;
  cost: number;
  duration_ms: number;
  known_jobs: number;
  unknown_jobs: number;
  known_metrics: Record<UsageAggregateMetric, number>;
}

export interface UsageGroup extends UsageTotals {
  key: string;
  project_id: string | null;
  agent_id: string | null;
  agent_name: string | null;
  role: string | null;
}

export interface UsageReport {
  group_by: string;
  filters: {
    project_id: string | null;
    agent_id: string | null;
    role: string | null;
    from: string | null;
    to: string | null;
  };
  totals: UsageTotals;
  groups: UsageGroup[];
}

export interface ProjectUsage {
  totals: UsageTotals;
  by_agent: UsageGroup[];
  by_role: UsageGroup[];
}

export interface Agent {
  id: string;
  name: string;
  kind: AgentKind;
  model: string;
  roles: Role[];
  max_concurrency: number;
  /// 并发已满时允许同角色下一优先级 Agent 接手；接口缺失该字段时按 false 处理。
  allow_priority_fallback: boolean;
  enabled: boolean;
  command: string[];
  model_args: string[];
  cooldown_until: string | null;
  last_error: string | null;
  failure_kind: string | null;
  failure_summary: FailureSummary | null;
  transient_failures: number;
  availability: AgentAvailability | null;
  usage_limit: AgentUsageLimit | null;
  usage_window: AgentUsageWindow | null;
  created_at: string;
  updated_at: string;
}

export interface ProjectAgentEntry {
  agent_id: string;
  name: string;
  kind: AgentKind | "missing";
  model: string;
  priority: number;
  enabled: boolean;
  agent_enabled: boolean;
  /** 项目 Agent 池中同一 Agent 在该角色下的启用状态；任务池据此展示项目层屏蔽。 */
  project_enabled: boolean;
  available: boolean;
  unavailable_reason: string | null;
  max_concurrency: number;
  /// 并发已满时允许同角色下一优先级 Agent 接手；接口缺失该字段时按 false 处理。
  allow_priority_fallback: boolean;
  cooldown_until: string | null;
  last_error: string | null;
  failure_kind: string | null;
  failure_summary: FailureSummary | null;
  transient_failures: number;
  availability: AgentAvailability | null;
  availability_reason: string | null;
  usage_limit: AgentUsageLimit | null;
  usage_window: AgentUsageWindow | null;
}

export interface ProjectRolePool {
  role: Role;
  entries: ProjectAgentEntry[];
}

export interface ProjectAgentPool {
  project_id: string;
  roles: ProjectRolePool[];
}

/** 任务专用 Agent 池：roles 与 ProjectAgentPool 相同形态，只有执行者与验收者。 */
export interface TaskAgentPool {
  project_id: string;
  task_id: string;
  roles: ProjectRolePool[];
}

/** 单条验收作业摘要：验收记录接口按所属执行轮返回验收者的解析结论。 */
export interface VerificationRecord {
  execution_job_id: string | null;
  evidence?: {
    contract_id: number;
    artifact_version: string | null;
    artifacts: string[];
    risks: string[];
    human_checks: string[];
    criteria: { criterion: string; passed: boolean; checks: { method: string; result: string }[] }[];
  } | null;
  id: string;
  /** 所属执行轮，从 1 开始，0 表示没有可关联的执行作业。 */
  round: number;
  attempt: number;
  status: string;
  passed: boolean | null;
  reason: string | null;
  issues: string[];
  parsed: boolean;
  parse_error: string | null;
  raw_excerpt: string | null;
  agent_id: string | null;
  agent_name: string | null;
  started_at: string | null;
  finished_at: string | null;
  usage_known: boolean;
}

/** 验收记录：按 created_at/id 升序，最多返回 limit 条；truncated 表示更早的记录已省略。 */
export interface VerificationHistory {
  project_id: string;
  task_id: string;
  total: number;
  limit: number;
  truncated: boolean;
  verifications: VerificationRecord[];
}

export type ProjectAttention = "confirm" | "review" | "failed" | "merge" | "blocked";
export type WorkspaceType = "git" | "plain";

export interface ProjectAttentionTask {
  id: string;
  key: string;
  title: string;
}

export interface AttentionAction {
  id: string;
  kind: ProjectAttention;
  task: ProjectAttentionTask | null;
  reason: string | null;
  since: string;
  affected_tasks: number;
  blocks_milestone: boolean;
  blocks_summary: boolean;
  project_advancing: boolean;
}

export interface ProjectSummary {
  attention_items: AttentionAction[];
  id: string;
  name: string;
  goal: string;
  repo_path: string;
  base_branch: string;
  branch: string;
  /** 缺省按 Git 工作区处理，兼容旧接口。 */
  workspace_type?: WorkspaceType;
  stage: string;
  status: string | null;
  blocked_reason: string | null;
  task_total: number;
  task_completed: number;
  replan_reason: string | null;
  replan_source: string | null;
  confirm_mode: "manual" | "auto";
  completion_mode: "tasks" | "direct";
  merged_at: string | null;
  merged_commit: string | null;
  created_at: string;
  updated_at: string;
}

export interface Project {
  id: string;
  name: string;
  goal: string;
  repo_path: string;
  base_branch: string;
  branch: string;
  workspace: string;
  /** 缺省按 Git 工作区处理，兼容旧接口。 */
  workspace_type?: WorkspaceType;
  stage: string;
  status: string | null;
  blocked_reason: string | null;
  report: string | null;
  report_feedback: string | null;
  replan_reason: string | null;
  replan_source: string | null;
  replan_requested_at: string | null;
  replan_active?: number;
  confirm_mode: "manual" | "auto";
  confirmed_at: string | null;
  completion_mode: "tasks" | "direct";
  merged_at: string | null;
  merged_commit: string | null;
  created_at: string;
  updated_at: string;
}

export interface Task {
  id: string;
  project_id: string;
  seq: number;
  queue_order: number;
  queue_position: number | null;
  key: string;
  title: string;
  description: string;
  acceptance: string[];
  deps: string[];
  parent_id: string | null;
  milestone: boolean;
  human_gate: string | null;
  forced_agent_id: string | null;
  forced_agent_role: string | null;
  stage: string;
  status: string | null;
  agent_id: string | null;
  last_agent_id: string | null;
  worktree: string | null;
  branch: string | null;
  artifacts_released_at: string | null;
  attempts: number;
  rejections: number;
  last_error: string | null;
  last_failure_kind: string | null;
  waiting_reason: string | null;
  result_summary: string | null;
  verifier_issues: string[];
  guidance: string;
  created_at: string;
  updated_at: string;
}

export interface DirtyWorktree {
  task_id: string | null;
  key: string | null;
  title: string | null;
  label: string;
  path: string;
}

export interface ArchivePreview {
  dirty: DirtyWorktree[];
}

export interface ArchivedRef {
  branch: string;
  reference: string;
  tip: string;
  recovery: string;
  log_command: string;
}

export interface ArchivedTaskRef extends ArchivedRef {
  task_id: string;
  key: string;
  title: string;
  released_at: string | null;
}

export interface ProjectArchiveRefs {
  project: ArchivedRef | null;
  tasks: ArchivedTaskRef[];
  backup_command: string;
}

export interface JobSummary {
  execution_job_id: string | null;
  id: string;
  project_id: string;
  task_id: string | null;
  role: string;
  agent_id: string | null;
  agent_name: string | null;
  status: string;
  attempt: number;
  error: string | null;
  exit_code: number | null;
  failure_kind: string | null;
  failure_summary: FailureSummary | null;
  input_tokens: number | null;
  output_tokens: number | null;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  reasoning_tokens: number | null;
  cost: number | null;
  duration_ms: number | null;
  usage_known: boolean;
  started_at: string | null;
  finished_at: string | null;
  created_at: string;
}

export interface JobDetail extends JobSummary {
  prompt: string;
  result: string | null;
  stdout: string | null;
  stderr: string | null;
  pid: number | null;
}

/** 作业活动条目：助手消息、推理（思维链）、工具调用、步骤或纯文本行预览。 */
export interface JobActivityItem {
  kind: "message" | "reasoning" | "tool" | "step" | "line";
  text: string | null;
  tool: string | null;
  target: string | null;
  status: string | null;
  error: string | null;
}

export interface JobActivitySummary {
  messages: number;
  reasoning: number;
  tool_calls: number;
  tool_failures: number;
  steps: number;
  truncated: boolean;
}

export interface JobActivity {
  format: "opencode" | "codex" | "workbuddy" | "text";
  bytes: number;
  items: JobActivityItem[];
  summary: JobActivitySummary;
}

export interface Message {
  id: string;
  project_id: string;
  role: string;
  content: string;
  created_at: string;
}

export interface AuditEvent {
  id: number;
  project_id: string | null;
  task_id: string | null;
  job_id: string | null;
  kind: string;
  message: string;
  message_length: number;
  message_truncated: boolean;
  created_at: string;
}

export interface ProposalTask {
  key: string;
  title: string;
  description: string;
  acceptance: string[];
  deps: string[];
}

export interface ReplanImpact {
  high_impact: boolean;
  canceled_task_keys: string[];
  retained_task_keys: string[];
  added_task_keys: string[];
  acceptance_changed: boolean;
}

export interface AnalysisProposal {
  kind: "plan" | "direct";
  summary: string;
  goal: string | null;
  assumptions: string[];
  acceptance: string[];
  tasks: ProposalTask[];
  plan: string[];
  verification: string[];
  skip_confirmation: boolean;
  replan_impact?: ReplanImpact | null;
}

export interface ProjectDetail {
  attention_items: AttentionAction[];
  delivery?: {
    replans?: { generation: number; goal: string; draft: AnalysisProposal | null; state: string; reason: string | null }[];
    acceptance?: { contract_id?: number; artifact_version?: string | null; criteria: { criterion: string; verdict: string; evidence: string[]; gap: string }[] } | null;
    merge_operation?: { phase: string; base_before: string; project_before: string; error: string | null } | null;
    current_contract_id: number | null;
    contracts: { id: number; goal: string; proposal: AnalysisProposal | null; source: string; created_at: string }[];
    limits: { cost_budget: number | null; task_attempt_limit: number | null; replan_limit: number | null };
    known_cost: number;
    unknown_cost_jobs: number;
  };
  project: Project;
  merged_at: string | null;
  merged_commit: string | null;
  merged_into_base: boolean;
  tasks: Task[];
  jobs: JobSummary[];
  messages: Message[];
  events: AuditEvent[];
  agents: Agent[];
  agent_pool: ProjectAgentPool;
  usage: ProjectUsage;
  proposal: AnalysisProposal | null;
}

export interface ArbitrationAgent {
  id: string;
  name: string;
  kind: string;
  enabled: boolean;
  cooldown_until: string | null;
  last_error: string | null;
  failure_summary: FailureSummary | null;
}

export interface ArbitrationJob {
  id: string;
  role: string;
  agent_id: string | null;
  agent_name: string | null;
  status: string;
  attempt: number;
  error: string | null;
  failure_summary: FailureSummary | null;
  exit_code: number | null;
  started_at: string | null;
  finished_at: string | null;
  created_at: string;
}

export interface ArbitrationTask {
  id: string;
  key: string;
  title: string;
  stage: string;
  status: string | null;
  last_error: string | null;
  failure_summary: FailureSummary | null;
  attempts: number;
  rejections: number;
  milestone: boolean;
  human_gate: string | null;
  forced_agent_id: string | null;
  forced_agent_role: string | null;
}

export interface ArbitrationContext {
  project: {
    id: string;
    name: string;
    stage: string;
    status: string | null;
    blocked_reason: string | null;
  };
  task: ArbitrationTask;
  role: Role;
  agents: ArbitrationAgent[];
  jobs: ArbitrationJob[];
  actions: string[];
}

export interface CreateProjectInput {
  name: string;
  repo_path: string;
  base_branch?: string;
  goal?: string;
  workspace_type?: WorkspaceType;
  confirm_mode?: "manual" | "auto";
}

export interface CreateTaskInput {
  key: string;
  title: string;
  description: string;
  acceptance?: string[];
  deps?: string[];
  milestone?: boolean;
}

export interface AgentInput {
  name: string;
  kind: AgentKind;
  model?: string;
  roles?: Role[];
  max_concurrency?: number;
  allow_priority_fallback?: boolean;
  enabled?: boolean;
  command?: string[];
  model_args?: string[];
  availability?: AgentAvailability | null;
  usage_limit?: AgentUsageLimit | null;
}

/// 默认团队是系统级 Agent 模板，覆盖全部已注册 Agent；新建项目的项目 Agent 池按它快照初始化。
/// 视图复用项目池形态（project_id 固定为 "default"）。
export type DefaultTeamPool = ProjectAgentPool;
