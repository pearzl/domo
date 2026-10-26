export type AgentKind = "mock" | "opencode" | "cli";
export type Role = "planner" | "executor" | "verifier";

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
  created_at: string;
  updated_at: string;
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
  created_at: string;
  updated_at: string;
}

export interface Task {
  id: string;
  project_id: string;
  seq: number;
  key: string;
  title: string;
  description: string;
  acceptance: string[];
  deps: string[];
  stage: string;
  status: string | null;
  agent_id: string | null;
  last_agent_id: string | null;
  worktree: string | null;
  branch: string | null;
  attempts: number;
  rejections: number;
  last_error: string | null;
  result_summary: string | null;
  verifier_issues: string[];
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
}
