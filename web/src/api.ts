import type {
  Agent,
  AgentInput,
  ArbitrationContext,
  AuditEvent,
  CreateProjectInput,
  CreateTaskInput,
  DefaultTeamPool,
  Health,
  JobActivity,
  JobDetail,
  ProjectAgentPool,
  ProjectDetail,
  ProjectSummary,
  Role,
  Task,
  TaskAgentPool,
  UsageReport,
} from "./types";

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(init?.headers ?? {}),
    },
  });
  if (!response.ok) {
    let message = `请求失败 (${response.status})`;
    try {
      const body = (await response.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // Keep the status-based message.
    }
    throw new Error(message);
  }
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}

export const api = {
  health: () => request<Health>("/api/health"),
  listProjects: () => request<ProjectSummary[]>("/api/projects"),
  createProject: (input: CreateProjectInput) =>
    request<ProjectDetail>("/api/projects", {
      method: "POST",
      body: JSON.stringify(input),
    }),
  getProject: (id: string) => request<ProjectDetail>(`/api/projects/${id}`),
  createTask: (projectId: string, input: CreateTaskInput) =>
    request<Task>(`/api/projects/${projectId}/tasks`, {
      method: "POST",
      body: JSON.stringify(input),
    }),
  getProjectAgents: (id: string) =>
    request<ProjectAgentPool>(`/api/projects/${id}/agents`),
  getDefaultTeam: () => request<DefaultTeamPool>("/api/team/default"),
  addDefaultTeamAgent: (role: Role, agentId: string) =>
    request<DefaultTeamPool>("/api/team/default", {
      method: "POST",
      body: JSON.stringify({ role, agent_id: agentId }),
    }),
  updateDefaultTeamAgent: (
    role: Role,
    agentId: string,
    input: { enabled?: boolean; direction?: "up" | "down"; position?: number },
  ) =>
    request<DefaultTeamPool>(`/api/team/default/${role}/${agentId}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),
  deleteDefaultTeamAgent: (role: Role, agentId: string) =>
    request<DefaultTeamPool>(`/api/team/default/${role}/${agentId}`, {
      method: "DELETE",
    }),
  addProjectAgent: (id: string, role: Role, agentId: string) =>
    request<ProjectAgentPool>(`/api/projects/${id}/agents`, {
      method: "POST",
      body: JSON.stringify({ role, agent_id: agentId }),
    }),
  updateProjectAgent: (
    id: string,
    role: Role,
    agentId: string,
    input: { enabled?: boolean; direction?: "up" | "down"; position?: number },
  ) =>
    request<ProjectAgentPool>(`/api/projects/${id}/agents/${role}/${agentId}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),
  getTaskAgentPool: (projectId: string, taskId: string) =>
    request<TaskAgentPool>(`/api/projects/${projectId}/tasks/${taskId}/agents`),
  addTaskAgent: (projectId: string, taskId: string, role: Role, agentId: string) =>
    request<TaskAgentPool>(`/api/projects/${projectId}/tasks/${taskId}/agents`, {
      method: "POST",
      body: JSON.stringify({ role, agent_id: agentId }),
    }),
  updateTaskAgent: (
    projectId: string,
    taskId: string,
    role: Role,
    agentId: string,
    input: { enabled?: boolean; direction?: "up" | "down"; position?: number },
  ) =>
    request<TaskAgentPool>(
      `/api/projects/${projectId}/tasks/${taskId}/agents/${role}/${agentId}`,
      {
        method: "PATCH",
        body: JSON.stringify(input),
      },
    ),
  removeTaskAgent: (projectId: string, taskId: string, role: Role, agentId: string) =>
    request<TaskAgentPool>(
      `/api/projects/${projectId}/tasks/${taskId}/agents/${role}/${agentId}`,
      {
        method: "DELETE",
      },
    ),
  postAnalysisMessage: (
    id: string,
    message: string,
    confirmMode?: "manual" | "auto",
  ) =>
    request<ProjectDetail>(`/api/projects/${id}/analysis/messages`, {
      method: "POST",
      body: JSON.stringify({
        message,
        confirm_mode: confirmMode ?? null,
      }),
    }),
  confirmAnalysis: (id: string) =>
    request<ProjectDetail>(`/api/projects/${id}/analysis/confirm`, {
      method: "POST",
      body: JSON.stringify({}),
    }),
  updateAnalysisPolicy: (id: string, mode: "manual" | "auto") =>
    request<ProjectDetail>(`/api/projects/${id}/analysis/policy`, {
      method: "PATCH",
      body: JSON.stringify({ mode }),
    }),
  getJobOutput: (id: string) =>
    request<{
      stdout: string;
      stderr: string;
      pid: number | null;
      running: boolean;
    }>(`/api/jobs/${id}/output`, { cache: "no-store" }),
  getJobActivity: (id: string) =>
    request<JobActivity>(`/api/jobs/${id}/activity`, { cache: "no-store" }),
  pauseProject: (id: string, force = false) =>
    request<{ ok: boolean; forced: boolean }>(`/api/projects/${id}/pause`, {
      method: "POST",
      body: JSON.stringify({ force }),
    }),
  resumeProject: (id: string) =>
    request<{ ok: boolean }>(`/api/projects/${id}/resume`, {
      method: "POST",
      body: JSON.stringify({}),
    }),
  archiveProject: (id: string) =>
    request<{ ok: boolean }>(`/api/projects/${id}/archive`, { method: "POST" }),
  pauseTask: (projectId: string, taskId: string, force = false) =>
    request<ProjectDetail>(
      `/api/projects/${projectId}/tasks/${taskId}/pause`,
      { method: "POST", body: JSON.stringify({ force }) },
    ),
  resumeTask: (projectId: string, taskId: string) =>
    request<ProjectDetail>(
      `/api/projects/${projectId}/tasks/${taskId}/resume`,
      { method: "POST", body: JSON.stringify({}) },
    ),
  reorderTaskQueue: (
    projectId: string,
    taskId: string,
    direction: "up" | "down",
  ) =>
    request<ProjectDetail>(
      `/api/projects/${projectId}/tasks/${taskId}/queue`,
      { method: "PATCH", body: JSON.stringify({ direction }) },
    ),
  retryJob: (id: string) =>
    request<ProjectDetail>(`/api/jobs/${id}/retry`, { method: "POST" }),
  retryTask: (projectId: string, taskId: string) =>
    request<{ ok: boolean }>(
      `/api/projects/${projectId}/tasks/${taskId}/retry`,
      { method: "POST" },
    ),
  saveTaskGuidance: (projectId: string, taskId: string, guidance: string) =>
    request<Task>(
      `/api/projects/${projectId}/tasks/${taskId}/guidance`,
      {
        method: "POST",
        body: JSON.stringify({ guidance }),
      },
    ),
  setTaskMilestone: (projectId: string, taskId: string, milestone: boolean) =>
    request<Task>(`/api/projects/${projectId}/tasks/${taskId}/milestone`, {
      method: "POST",
      body: JSON.stringify({ milestone }),
    }),
  acceptMilestone: (projectId: string, taskId: string, reason?: string) =>
    request<ProjectDetail>(
      `/api/projects/${projectId}/tasks/${taskId}/milestone/accept`,
      {
        method: "POST",
        body: JSON.stringify({ reason: reason || null }),
      },
    ),
  rejectMilestone: (
    projectId: string,
    taskId: string,
    reason: string,
    guidance?: string,
    action: "retry" | "replan" = "retry",
  ) =>
    request<ProjectDetail>(
      `/api/projects/${projectId}/tasks/${taskId}/milestone/reject`,
      {
        method: "POST",
        body: JSON.stringify({ reason, guidance: guidance || null, action }),
      },
    ),
  getTaskArbitration: (projectId: string, taskId: string) =>
    request<ArbitrationContext>(
      `/api/projects/${projectId}/tasks/${taskId}/arbitration`,
    ),
  submitTaskArbitration: (
    projectId: string,
    taskId: string,
    input: { action: "retry" | "switch_agent" | "guide"; agent_id?: string; guidance?: string },
  ) =>
    request<ProjectDetail>(
      `/api/projects/${projectId}/tasks/${taskId}/arbitration`,
      {
        method: "POST",
        body: JSON.stringify(input),
      },
    ),
  replanProject: (id: string, reason: string) =>
    request<ProjectDetail>(`/api/projects/${id}/replan`, {
      method: "POST",
      body: JSON.stringify({ reason }),
    }),
  submitReport: (
    id: string,
    action: "accept" | "reject" | "replan" | "comment",
    feedback?: string,
  ) =>
    request<{ ok: boolean; action: string }>(`/api/projects/${id}/report`, {
      method: "POST",
      body: JSON.stringify({ action, feedback: feedback || null }),
    }),
  mergeProject: (id: string) =>
    request<{
      status: "merged" | "up_to_date";
      message: string;
      merged_at: string;
      merged_commit: string;
      project: ProjectDetail;
    }>(`/api/projects/${id}/merge`, { method: "POST" }),
  getJob: (id: string) => request<JobDetail>(`/api/jobs/${id}`),
  listEvents: (projectId?: string, after?: number) => {
    const params = new URLSearchParams();
    if (projectId) params.set("project_id", projectId);
    if (after) params.set("after", String(after));
    return request<AuditEvent[]>(`/api/events?${params.toString()}`);
  },
  getEvent: (id: number) => request<AuditEvent>(`/api/events/${id}`),
  listAgents: () => request<Agent[]>("/api/agents"),
  createAgent: (input: AgentInput) =>
    request<Agent>("/api/agents", {
      method: "POST",
      body: JSON.stringify(input),
    }),
  updateAgent: (id: string, input: Partial<AgentInput>) =>
    request<Agent>(`/api/agents/${id}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),
  deleteAgent: (id: string) =>
    request<void>(`/api/agents/${id}`, { method: "DELETE" }),
  resetAgentUsage: (id: string) =>
    request<Agent>(`/api/agents/${id}/usage/reset`, { method: "POST" }),
  getUsage: (params: {
    project_id?: string;
    agent_id?: string;
    role?: string;
    from?: string;
    to?: string;
    group_by?: "project" | "agent" | "role" | "none";
  }) => {
    const search = new URLSearchParams();
    Object.entries(params).forEach(([key, value]) => {
      if (value) search.set(key, value);
    });
    return request<UsageReport>(`/api/usage?${search.toString()}`);
  },
};

export function subscribeEvents(
  onEvent: () => void,
  onStatus?: (connected: boolean) => void,
): () => void {
  if (typeof EventSource === "undefined") return () => undefined;
  const source = new EventSource("/api/events/stream");
  source.onopen = () => onStatus?.(true);
  source.onerror = () => onStatus?.(false);
  source.onmessage = () => onEvent();
  return () => source.close();
}

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
