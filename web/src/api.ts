import type {
  Agent,
  Adapter,
  ApplyResult,
  AuditEvent,
  InboxItem,
  KnowledgeEntry,
  LogLine,
  Milestone,
  PoolView,
  Project,
  ReplanChange,
  ReplanReportResult,
  Report,
  Role,
  Task,
  TaskDetail,
  TaskFormValue,
  Workspace,
} from './types';

const API_BASE = '/api';
// SSE 直连后端，不经 vite proxy
export const SSE_URL = 'http://127.0.0.1:8787/api/stream';

async function request<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    method,
    headers: body !== undefined ? { 'Content-Type': 'application/json' } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    let msg = `请求失败 (HTTP ${res.status})`;
    try {
      const data = (await res.json()) as Record<string, unknown>;
      if (data && typeof data === 'object') {
        const m = data.error ?? data.message ?? data.detail;
        if (typeof m === 'string') msg = m;
      }
    } catch {
      /* 忽略非 JSON 响应体 */
    }
    throw new Error(msg);
  }
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

export interface ProjectDetailPayload {
  project: Project;
  tasks: Task[];
  milestones: Milestone[];
  agents: Agent[];
  inbox_open_count: number;
  /** M3 项目总结报告摘要（未生成时缺省/null） */
  report?: Report | null;
}

/** 兼容两种返回形态：{...task, logs, events} 或 {task, logs, events} */
function normalizeTaskDetail(raw: unknown): TaskDetail {
  const obj = (raw ?? {}) as { task?: Task; logs?: unknown; events?: unknown };
  const base = (obj.task ?? obj) as unknown as Task;
  const logs = Array.isArray(obj.logs) ? (obj.logs as LogLine[]) : [];
  const events = Array.isArray(obj.events) ? (obj.events as AuditEvent[]) : [];
  return { ...base, logs, events };
}

export const api = {
  listProjects: () => request<Project[]>('/projects'),
  createProject: (body: { name: string; brief: string; workspace?: Workspace | null }) =>
    request<Project>('/projects', 'POST', body),
  getProject: (id: string) => request<ProjectDetailPayload>(`/projects/${id}`),
  /** PATCH 支持任意状态（archived 拒绝 409） */
  updateProject: (id: string, body: { name?: string; brief?: string; workspace?: Workspace | null; agent_pool?: string[] | null }) =>
    request<Project>(`/projects/${id}`, 'PATCH', body),
  projectAction: (id: string, action: 'confirm' | 'pause' | 'resume' | 'cancel', force?: boolean) =>
    request<Project>(`/projects/${id}/${action}`, 'POST', force ? { force: true } : {}),
  /** M3 项目总结报告：读取详情（未生成时 report 为 null） */
  getProjectReport: (id: string) =>
    request<{ report: Report | null }>(`/projects/${id}/report`),
  /** M3 手工（重新）生成项目总结报告 —— 失败重试入口（项目须已完成） */
  generateProjectReport: (id: string) =>
    request<{ report: Report }>(`/projects/${id}/report`, 'POST', {}),
  /** 自动规划：同步阻塞调用（LLM 可能耗时数分钟），fetch 无超时，调用方需长等待 */
  planProject: (id: string, opts?: { planner_agent_id?: string; executor_agent_id?: string; acceptor_agent_id?: string; feedback?: string }) =>
    request<{ tasks_added: Task[] }>(`/projects/${id}/plan`, 'POST', opts ?? {}),

  /** 人工介入：规划者分析说明并给出调整方案（探讨用） */
  intervene: (id: string, note: string) =>
    request<{ analysis: string; reply: string; changes: ReplanChange[] }>(`/projects/${id}/intervene`, 'POST', { note }),
  /** 采纳方案：逐项应用（单项失败自动跳过并在 skipped 中返回原因） */
  applyIntervention: (id: string, changes: ReplanChange[]) =>
    request<{ applied: ApplyResult['applied']; skipped: ApplyResult['skipped'] }>(`/projects/${id}/intervene/apply`, 'POST', { changes }),
  /** 里程碑窗口：应用窗口内的追加任务 / 依赖调整（成环项跳过并汇报原因） */
  applyMilestoneWindow: (milestoneId: string, changes: ReplanChange[], note: string) =>
    request<{ milestone_id: string; applied: ApplyResult['applied']; skipped: ApplyResult['skipped'] }>(
      `/milestones/${milestoneId}/window/apply`, 'POST', { changes, note }),

  createTask: (projectId: string, body: TaskFormValue) =>
    request<Task>(`/projects/${projectId}/tasks`, 'POST', body),
  updateTask: (id: string, body: Partial<TaskFormValue>) =>
    request<Task>(`/tasks/${id}`, 'PATCH', body),
  getTaskDetail: async (id: string): Promise<TaskDetail> =>
    normalizeTaskDetail(await request<unknown>(`/tasks/${id}`)),
  sendGuidance: (id: string, text: string) =>
    request<unknown>(`/tasks/${id}/guidance`, 'POST', { text }),

  /** M5 项目知识库：条目 CRUD + 提升为全局（全部写审计事件） */
  listKnowledge: (projectId: string) => request<KnowledgeEntry[]>(`/projects/${projectId}/knowledge`),
  createKnowledge: (projectId: string, body: { body: string; applies_to?: string; source_event_id: number }) =>
    request<KnowledgeEntry>(`/projects/${projectId}/knowledge`, 'POST', body),
  getKnowledge: (id: string) => request<KnowledgeEntry>(`/knowledge/${id}`),
  updateKnowledge: (id: string, body: { body: string; applies_to?: string }) =>
    request<KnowledgeEntry>(`/knowledge/${id}`, 'PATCH', body),
  deleteKnowledge: (id: string) => request<unknown>(`/knowledge/${id}`, 'DELETE'),
  /** 提升为全局条目（跨项目复用）；已是 global 时后端 409 */
  promoteKnowledge: (id: string) => request<KnowledgeEntry>(`/knowledge/${id}/promote`, 'POST', {}),
  acceptTask: (id: string, comment: string) =>
    request<unknown>(`/tasks/${id}/accept`, 'POST', { comment }),
  rejectTask: (id: string, comment: string) =>
    request<unknown>(`/tasks/${id}/reject`, 'POST', { comment }),
  /** 上报「发现规划问题」→ 生成 replan 决策请求（战略事项转人工收件箱） */
  reportReplan: (
    id: string,
    body: {
      reason: string;
      detail?: string;
      reported_by?: string;
      proposed_changes?: ReplanChange[];
    },
  ) => request<ReplanReportResult>(`/tasks/${id}/replan`, 'POST', body),

  createAgent: (body: {
    name: string;
    adapter: Adapter;
    roles: Role[];
    capabilities: { supports_resume: boolean; max_parallel: number };
    config: { duration_ms?: number; model?: string; timeout_ms?: number };
  }) => request<Agent>('/agents', 'POST', body),

  getProjectPool: (id: string) => request<PoolView>(`/projects/${id}/pool`),
  putProjectPool: (id: string, pool: Record<string, string[]>) =>
    request<PoolView>(`/projects/${id}/pool`, 'PUT', { agent_pool: pool }),

  listAgents: (opts?: { all?: boolean }) =>
    request<Agent[]>(`/agents${opts?.all ? '?all=1' : ''}`),
  updateAgent: (id: string, body: { name?: string; roles?: Role[]; config?: Record<string, unknown>; capabilities?: { max_parallel?: number; supports_resume?: boolean } }) =>
    request<Agent>(`/agents/${id}`, 'PATCH', body),
  deleteAgent: (id: string) =>
    request<{ deleted: string }>(`/agents/${id}`, 'DELETE'),
  listInbox: () => request<InboxItem[]>('/inbox'),
  resolveInbox: (id: string, resolution: string, note: string, agentId?: string, criteria?: string) =>
    request<InboxItem>(`/inbox/${id}/resolve`, 'POST', {
      resolution,
      note,
      ...(agentId ? { agent_id: agentId } : {}),
      ...(criteria ? { criteria } : {}),
    }),

  listEvents: (params: { project_id?: string; order?: 'asc' | 'desc'; type?: string; type_prefix?: string; from?: number; to?: number; limit?: number; offset?: number }) => {
    const usp = new URLSearchParams();
    if (params.project_id) usp.set('project_id', params.project_id);
    if (params.order) usp.set('order', params.order);
    if (params.type) usp.set('type', params.type);
    if (params.type_prefix) usp.set('type_prefix', params.type_prefix);
    if (params.from != null) usp.set('from', String(params.from));
    if (params.to != null) usp.set('to', String(params.to));
    if (params.limit != null) usp.set('limit', String(params.limit));
    if (params.offset != null) usp.set('offset', String(params.offset));
    const qs = usp.toString();
    return request<{ events: AuditEvent[]; total?: number; limit?: number; offset?: number } | AuditEvent[]>(
      `/events${qs ? `?${qs}` : ''}`,
    );
  },
};
