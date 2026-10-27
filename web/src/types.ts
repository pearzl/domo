export type AgentKind = "mock" | "opencode" | "cli";
export type Role = "planner" | "executor" | "verifier";
export type UsageMetric = "tokens" | "input_tokens" | "output_tokens" | "cost";
export type UsageWindowKind = "day" | "month";

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

export interface UsageTotals {
  jobs: number;
  input_tokens: number;
  output_tokens: number;
  cost: number;
  duration_ms: number;
  unknown_jobs: number;
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
  priority: number;
  max_concurrency: number;
  enabled: boolean;
  command: string[];
  model_args: string[];
  cooldown_until: string | null;
  last_error: string | null;
  failure_kind: string | null;
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
  available: boolean;
  unavailable_reason: string | null;
  max_concurrency: number;
  cooldown_until: string | null;
  last_error: string | null;
  failure_kind: string | null;
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

export interface ProjectSummary {
  id: string;
  name: string;
  goal: string;
  repo_path: string;
  base_branch: string;
  branch: string;
  stage: string;
  status: string | null;
  blocked_reason: string | null;
  task_total: number;
  task_completed: number;
  replan_reason: string | null;
  replan_source: string | null;
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
  stage: string;
  status: string | null;
  blocked_reason: string | null;
  report: string | null;
  report_feedback: string | null;
  replan_reason: string | null;
  replan_source: string | null;
  replan_requested_at: string | null;
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
  executor_agent_id: string | null;
  verifier_agent_id: string | null;
  stage: string;
  status: string | null;
  agent_id: string | null;
  last_agent_id: string | null;
  worktree: string | null;
  branch: string | null;
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

export interface JobSummary {
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
  input_tokens: number | null;
  output_tokens: number | null;
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
  kind: string;
  message: string;
  created_at: string;
}

export interface ProjectDetail {
  project: Project;
  tasks: Task[];
  jobs: JobSummary[];
  messages: Message[];
  events: AuditEvent[];
  agents: Agent[];
  agent_pool: ProjectAgentPool;
  usage: ProjectUsage;
}

export interface ArbitrationAgent {
  id: string;
  name: string;
  kind: string;
  priority: number;
  enabled: boolean;
  cooldown_until: string | null;
  last_error: string | null;
}

export interface ArbitrationJob {
  id: string;
  role: string;
  agent_id: string | null;
  agent_name: string | null;
  status: string;
  attempt: number;
  error: string | null;
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
  goal: string;
  repo_path: string;
  base_branch?: string;
}

export interface AgentInput {
  name: string;
  kind: AgentKind;
  model?: string;
  roles?: Role[];
  priority?: number;
  max_concurrency?: number;
  enabled?: boolean;
  command?: string[];
  model_args?: string[];
  availability?: AgentAvailability | null;
  usage_limit?: AgentUsageLimit | null;
}
