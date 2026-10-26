import type {
  Agent,
  AgentInput,
  AuditEvent,
  CreateProjectInput,
  JobDetail,
  ProjectDetail,
  ProjectSummary,
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
  confirmGoal: (id: string, message?: string) =>
    request<ProjectDetail>(`/api/projects/${id}/confirm`, {
      method: "POST",
      body: JSON.stringify({ message: message || null }),
    }),
  pauseProject: (id: string) =>
    request<{ ok: boolean }>(`/api/projects/${id}/pause`, { method: "POST" }),
  resumeProject: (id: string) =>
    request<{ ok: boolean }>(`/api/projects/${id}/resume`, { method: "POST" }),
  archiveProject: (id: string) =>
    request<{ ok: boolean }>(`/api/projects/${id}/archive`, { method: "POST" }),
  retryProject: (id: string) =>
    request<ProjectDetail>(`/api/projects/${id}/retry`, { method: "POST" }),
  retryTask: (projectId: string, taskId: string) =>
    request<{ ok: boolean }>(
      `/api/projects/${projectId}/tasks/${taskId}/retry`,
      { method: "POST" },
    ),
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
