import type {
  Agent,
  AgentInput,
  ArbitrationContext,
  AuditEvent,
  CreateProjectInput,
  JobDetail,
  ProjectAgentPool,
  ProjectDetail,
  ProjectSummary,
  Role,
  Task,
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
  listProjects: () => request<ProjectSummary[]>("/api/projects"),
  createProject: (input: CreateProjectInput) =>
    request<ProjectDetail>("/api/projects", {
      method: "POST",
      body: JSON.stringify(input),
    }),
  getProject: (id: string) => request<ProjectDetail>(`/api/projects/${id}`),
  getProjectAgents: (id: string) =>
    request<ProjectAgentPool>(`/api/projects/${id}/agents`),
  addProjectAgent: (id: string, role: Role, agentId: string) =>
    request<ProjectAgentPool>(`/api/projects/${id}/agents`, {
      method: "POST",
      body: JSON.stringify({ role, agent_id: agentId }),
    }),
  updateProjectAgent: (
    id: string,
    role: Role,
    agentId: string,
    input: { enabled?: boolean; direction?: "up" | "down" },
  ) =>
    request<ProjectAgentPool>(`/api/projects/${id}/agents/${role}/${agentId}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),
  setTaskAgents: (
    projectId: string,
    taskId: string,
    input: { executor_agent_id?: string | null; verifier_agent_id?: string | null },
  ) =>
    request<Task>(`/api/projects/${projectId}/tasks/${taskId}/agents`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),
  confirmGoal: (id: string, message?: string) =>
    request<ProjectDetail>(`/api/projects/${id}/confirm`, {
      method: "POST",
      body: JSON.stringify({ message: message || null }),
    }),
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
  retryProject: (id: string) =>
    request<ProjectDetail>(`/api/projects/${id}/retry`, { method: "POST" }),
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
  ) =>
    request<ProjectDetail>(
      `/api/projects/${projectId}/tasks/${taskId}/milestone/reject`,
      {
        method: "POST",
        body: JSON.stringify({ reason, guidance: guidance || null }),
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
  submitReport: (id: string, action: "accept" | "reject", feedback?: string) =>
    request<{ ok: boolean }>(`/api/projects/${id}/report`, {
      method: "POST",
      body: JSON.stringify({ action, feedback: feedback || null }),
    }),
  getJob: (id: string) => request<JobDetail>(`/api/jobs/${id}`),
  listEvents: (projectId?: string, after?: number) => {
    const params = new URLSearchParams();
    if (projectId) params.set("project_id", projectId);
    if (after) params.set("after", String(after));
    return request<AuditEvent[]>(`/api/events?${params.toString()}`);
  },
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

export function subscribeEvents(onEvent: () => void): () => void {
  if (typeof EventSource === "undefined") return () => undefined;
  const source = new EventSource("/api/events/stream");
  source.onmessage = () => onEvent();
  return () => source.close();
}

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
