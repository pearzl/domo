/**
 * 全局状态与 SSE 订阅。
 *
 * - 所有视图都要在后台保持最新：SSE 到达即更新全局缓存，不以"是否当前视图"为判断；
 *   切回视图直接读缓存，不重新拉取、不轮询。
 * - 视图只读后端给出的状态与计数，不在前端推导业务状态（如"能否派发""是否卡死"）。
 * - 跨视图 UI 状态（选中任务、视图模式、聚焦条目）统一提升到这里，切页不丢。
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { api, SSE_URL } from './api';
import type {
  Adapter,
  Agent,
  AgentFormValue,
  ApplyResult,
  AuditEvent,
  Engineering,
  InterveneResult,
  InboxItem,
  LogLine,
  Milestone,
  Project,
  ReplanChange,
  ReplanReportResult,
  Report,
  Task,
  TaskDetail,
  TaskFormValue,
  Workspace,
} from './types';

export type ConnState = 'connecting' | 'open' | 'closed';
export type ViewId = 'engineering' | 'projects' | 'tasks' | 'org' | 'inbox' | 'audit' | 'knowledge';
/** 任务视图呈现方式：看板 / DAG 图（提升到 store，切页再切回不丢失） */
export type TasksViewMode = 'kanban' | 'dag';
/** DAG 布局方向：tb=上下（里程碑横泳道）/ lr=左右（同上看板流向）——同样跨视图保持 */
export type DagDir = 'tb' | 'lr';

/** M5 知识沉淀预填：从任务详情/Audit 一键带过来的来源人工指导事件 */
export interface KnowledgePrefill {
  event_id: number;
  project_id: string;
  /** 指导正文，作为新条目的 body 预填 */
  text: string;
}

export interface DomoStore {
  conn: ConnState;
  view: ViewId;
  setView: (v: ViewId) => void;
  /** 组织视图聚焦的 agent（审计点跳转用；高亮展示，含软删除快照回退） */
  focusAgentId: string | null;
  setFocusAgentId: (id: string | null) => void;

  /** M5 知识沉淀预填（任务详情/Audit 一键带来源事件 → 知识库视图开新建表单） */
  knowledgePrefill: KnowledgePrefill | null;
  /** 触发预填并跳到知识库视图；consume=true 清空预填（表单已读取） */
  distillGuidance: (p: KnowledgePrefill | null, consume?: boolean) => void;
  error: string | null;
  setError: (e: string | null) => void;

  projects: Project[];
  engineerings: Engineering[];
  agents: Agent[];
  inbox: InboxItem[];
  events: AuditEvent[];
  tasksByProject: Record<string, Task[]>;
  milestonesByProject: Record<string, Milestone[]>;
  logsByTask: Record<string, LogLine[]>;
  /** M3 项目总结报告：按项目缓存（SSE report 事件实时刷新） */
  reports: Record<string, Report>;

  selectedProjectId: string | null;
  selectedTaskId: string | null;
  taskDetail: TaskDetail | null;

  /** 任务视图呈现方式（看板 / DAG），跨视图切换保持 */
  tasksViewMode: TasksViewMode;
  setTasksViewMode: (m: TasksViewMode) => void;
  /** DAG 布局方向（tb/lr），跨视图切换保持 */
  dagDir: DagDir;
  setDagDir: (d: DagDir) => void;
  /** 收件箱聚焦条目：从里程碑 gate 一键跳转时高亮并滚动到该项 */
  focusedInboxId: string | null;
  focusInbox: (itemId: string) => void;

  selectProject: (id: string) => void;
  openTask: (id: string) => void;
  closeTask: () => void;
  refreshEvents: (projectId?: string) => Promise<void>;

  createProject: (name: string, brief: string, workspace?: Workspace | null, engineeringId?: string | null) => Promise<boolean>;
  createEngineering: (body: {
    name: string;
    brief: string;
    workspace_kind: 'single' | 'multi' | 'plain';
    workspace_path?: string | null;
    constraints?: string | null;
  }) => Promise<Engineering | null>;
  updateEngineering: (id: string, body: Partial<Omit<Engineering, 'id' | 'created_at'>>) => Promise<boolean>;
  updateProject: (
    id: string,
    value: { name?: string; brief?: string; workspace?: Workspace | null; agent_pool?: string[] | null },
  ) => Promise<boolean>;
  projectAction: (id: string, action: 'confirm' | 'pause' | 'resume' | 'cancel', force?: boolean) => Promise<void>;
  /** M3 项目总结报告：拉取详情（未生成时返回 null） */
  loadReport: (projectId: string) => Promise<Report | null>;
  /** M3 项目总结报告：手工生成 / 失败重试 */
  generateReport: (projectId: string) => Promise<Report | null>;
  /** 自动规划：长等待（LLM 可能数分钟）。成功返回新增任务数，失败返回 -1 并弹错误 toast */
  planProject: (id: string, opts?: { planner_agent_id?: string; executor_agent_id?: string; acceptor_agent_id?: string; feedback?: string }) => Promise<number>;
  submitTaskForm: (projectId: string, value: TaskFormValue, editTaskId?: string) => Promise<boolean>;
  /** 人工介入：规划者分析并给出调整方案（探讨用），返回方案 */
  intervene: (projectId: string, note: string) => Promise<InterveneResult | null>;
  /** 采纳方案：逐项应用 changes */
  applyIntervention: (projectId: string, changes: ReplanChange[]) => Promise<ApplyResult | null>;
  /** 里程碑窗口：应用窗口内的追加任务 / 依赖调整（成环项被拒并返回原因） */
  applyMilestoneWindow: (
    projectId: string,
    milestoneId: string,
    changes: ReplanChange[],
    note: string,
  ) => Promise<ApplyResult | null>;
  /** 全局规划长等待状态：跨视图切换保持 */
  planning: { projectId: string; startedAt: number } | null;
  sendGuidance: (taskId: string, text: string) => Promise<boolean>;
  reviewTask: (taskId: string, verdict: 'accept' | 'reject', comment: string) => Promise<boolean>;
  /** 上报规划问题（executor/acceptor → 重规划请求）；成功返回决策信息用于提示 */
  reportReplan: (
    taskId: string,
    body: { reason: string; detail?: string; proposed_changes?: ReplanChange[] },
  ) => Promise<ReplanReportResult | null>;
  createAgent: (value: AgentFormValue) => Promise<boolean>;
  /** 编辑 agent（name/roles/config/capabilities，adapter 不变） */
  updateAgent: (id: string, value: AgentFormValue) => Promise<boolean>;
  /** 删除 agent（被非终态任务引用时后端拒绝并报错） */
  deleteAgent: (id: string) => Promise<boolean>;
  resolveInboxItem: (id: string, resolution: string, note: string, agentId?: string, criteria?: string) => Promise<boolean>;
}

const DomoCtx = createContext<DomoStore | null>(null);

export function useDomo(): DomoStore {
  const v = useContext(DomoCtx);
  if (!v) throw new Error('useDomo 必须在 DomoProvider 内使用');
  return v;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function upsertById<T extends { id: string }>(list: T[], item: T): T[] {
  const i = list.findIndex(x => x.id === item.id);
  if (i === -1) return [...list, item];
  const copy = list.slice();
  copy[i] = item;
  return copy;
}

function mergeEvents(list: AuditEvent[], incoming: AuditEvent[]): AuditEvent[] {
  const seen = new Set(list.map(e => e.id));
  const merged = [...list];
  for (const e of incoming) {
    if (!seen.has(e.id)) {
      seen.add(e.id);
      merged.push(e);
    }
  }
  merged.sort((a, b) => a.id - b.id);
  return merged.length > 3000 ? merged.slice(merged.length - 2000) : merged;
}

export function DomoProvider({ children }: { children: ReactNode }) {
  const [conn, setConn] = useState<ConnState>('connecting');
  const [view, setViewRaw] = useState<ViewId>('engineering');
  const [error, setError] = useState<string | null>(null);

  const [projects, setProjects] = useState<Project[]>([]);
  const [engineerings, setEngineerings] = useState<Engineering[]>([]);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [inbox, setInbox] = useState<InboxItem[]>([]);
  const [events, setEvents] = useState<AuditEvent[]>([]);
  const [tasksByProject, setTasksByProject] = useState<Record<string, Task[]>>({});
  const [milestonesByProject, setMilestonesByProject] = useState<Record<string, Milestone[]>>({});
  const [logsByTask, setLogsByTask] = useState<Record<string, LogLine[]>>({});
  const [reports, setReports] = useState<Record<string, Report>>({});

  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [taskDetail, setTaskDetail] = useState<TaskDetail | null>(null);

  const [tasksViewMode, setTasksViewMode] = useState<TasksViewMode>('dag');
  const [dagDir, setDagDir] = useState<DagDir>('tb');
  const [focusedInboxId, setFocusedInboxId] = useState<string | null>(null);
  const [focusAgentId, setFocusAgentId] = useState<string | null>(null);
  const [knowledgePrefill, setKnowledgePrefill] = useState<KnowledgePrefill | null>(null);

  /** M5 知识沉淀：带来源指导事件跳到知识库视图（consume=true 时清空，供表单读取一次） */
  const distillGuidance = useCallback((p: KnowledgePrefill | null, consume = false) => {
    setKnowledgePrefill(p);
    if (p) setViewRaw('knowledge');
    if (consume) setKnowledgePrefill(null);
  }, []);

  /** 切换视图：离开收件箱时清除一次性高亮（下次进入不再残留脉冲） */
  const setView = useCallback((v: ViewId) => {
    setViewRaw(v);
    if (v !== 'inbox') setFocusedInboxId(null);
  }, []);

  // 全局规划长等待：跨视图切换保持
  const [planning, setPlanning] = useState<{ projectId: string; startedAt: number } | null>(null);
  const planningRef = useRef<{ projectId: string; startedAt: number } | null>(null);

  const inflightProjects = useRef<Set<string>>(new Set());

  /** 合并报告：列表摘要（无 content）不覆盖已有的完整报告，避免查看入口把正文清空 */
  const mergeReport = useCallback((r: Report) => {
    setReports(m => {
      const prev = m[r.project_id];
      if (prev && prev.content != null && r.content == null) return m;
      return { ...m, [r.project_id]: r };
    });
  }, []);

  /** 拉取某项目的任务列表（含项目信息兜底） */
  const loadProjectTasks = useCallback(async (id: string) => {
    if (inflightProjects.current.has(id)) return;
    inflightProjects.current.add(id);
    try {
      const detail = await api.getProject(id);
      setTasksByProject(m => ({ ...m, [id]: detail.tasks ?? [] }));
      setMilestonesByProject(m => ({ ...m, [id]: detail.milestones ?? [] }));
      if (detail.report) mergeReport(detail.report);
      // 仅当列表里缺失时补入，避免覆盖 SSE 已推送的更新状态
      setProjects(p =>
        p.some(x => x.id === detail.project.id) ? p : upsertById(p, detail.project),
      );
      setAgents(a => {
        let next = a;
        for (const ag of detail.agents ?? []) {
          if (!next.some(x => x.id === ag.id)) next = upsertById(next, ag);
        }
        return next;
      });
    } catch (e) {
      setError(errMsg(e));
    } finally {
      inflightProjects.current.delete(id);
    }
  }, [mergeReport]);

  const applyTaskUpdate = useCallback((t: Task) => {
    setTasksByProject(m => {
      const list = m[t.project_id];
      if (!list) return m; // 该项目任务未加载，等选择时拉取
      return { ...m, [t.project_id]: upsertById(list, t) };
    });
    setTaskDetail(d => (d && d.id === t.id ? { ...d, ...t } : d));
  }, []);

  /** SSE 订阅：按 event 名分发，EventSource 自带断线重连 */
  useEffect(() => {
    const es = new EventSource(SSE_URL);
    const on = <T,>(name: string, fn: (data: T) => void) =>
      es.addEventListener(name, ev => {
        try {
          fn(JSON.parse((ev as MessageEvent).data) as T);
        } catch {
          /* 忽略坏帧 */
        }
      });

    es.onopen = () => setConn('open');
    es.onerror = () => setConn('closed'); // 浏览器自动重连

    on('snapshot', (d: { engineerings?: Engineering[]; projects?: Project[]; agents?: Agent[]; inbox?: InboxItem[] }) => {
      if (d.engineerings) setEngineerings(d.engineerings);
      if (d.projects) setProjects(d.projects);
      if (d.agents) setAgents(d.agents);
      if (d.inbox) setInbox(d.inbox);
      // 重连后补拉缺失的任务数据
      for (const p of d.projects ?? []) {
        setTasksByProject(m => {
          if (!m[p.id]) void loadProjectTasks(p.id);
          return m;
        });
      }
    });
    on('project', (d: Project) => setProjects(p => upsertById(p, d)));
    on('engineering', (d: Engineering) => setEngineerings(list => upsertById(list, d)));
    on('task', (d: Task) => applyTaskUpdate(d));
    on('log', (d: { task_id: string; ts: number; line: string }) =>
      setLogsByTask(m => ({
        ...m,
        [d.task_id]: [...(m[d.task_id] ?? []), { ts: d.ts, line: d.line }],
      })),
    );
    on('agent', (d: Agent) => setAgents(a => upsertById(a, d)));
    on('milestone', (d: Milestone) =>
      setMilestonesByProject(m => ({
        ...m,
        [d.project_id]: upsertById(m[d.project_id] ?? [], d),
      })),
    );
    on('inbox', (d: InboxItem) => setInbox(i => upsertById(i, d)));
    // M3 报告：状态/内容变化（生成中 / 失败可重试 / 就绪）实时刷新查看入口
    on('report', (d: Report) => mergeReport(d));
    on('event', (d: AuditEvent) => {
      setEvents(ev => mergeEvents(ev, [d]));
      setTaskDetail(t =>
        t && d.task_id && d.task_id === t.id ? { ...t, events: mergeEvents(t.events, [d]) } : t,
      );
    });

    return () => es.close();
  }, [applyTaskUpdate, loadProjectTasks, mergeReport]);

  /** 初次加载：REST 拉全量 */
  useEffect(() => {
    void (async () => {
      try {
        const [ps, ags, ib] = await Promise.all([
          api.listProjects(),
          api.listAgents(),
          api.listInbox(),
        ]);
        setProjects(ps);
        setAgents(ags);
        setInbox(ib);
        await Promise.all(ps.map(p => loadProjectTasks(p.id)));
      } catch (e) {
        setError(`后端连接失败：${errMsg(e)}`);
      }
    })();
  }, [loadProjectTasks]);

  const selectProject = useCallback(
    (id: string) => {
      setSelectedProjectId(id);
      setSelectedTaskId(null);
      setTaskDetail(null);
      setView('tasks');
      void loadProjectTasks(id);
    },
    [loadProjectTasks],
  );

  const openTask = useCallback(async (id: string) => {
    setSelectedTaskId(id);
    setTaskDetail(null);
    try {
      const d = await api.getTaskDetail(id);
      setTaskDetail(d);
      setLogsByTask(m => ({ ...m, [id]: d.logs }));
    } catch (e) {
      setError(errMsg(e));
      setSelectedTaskId(null);
    }
  }, []);

  const closeTask = useCallback(() => {
    setSelectedTaskId(null);
    setTaskDetail(null);
  }, []);

  /** 从里程碑 gate 一键跳转到收件箱对应项：切到收件箱并聚焦高亮该条目 */
  const focusInbox = useCallback((itemId: string) => {
    setFocusedInboxId(itemId);
    setView('inbox');
  }, []);

  const refreshEvents = useCallback(async (projectId?: string) => {
    try {
      const res = await api.listEvents({ project_id: projectId });
      const list = Array.isArray(res) ? res : (res.events ?? []);
      setEvents(ev => mergeEvents(ev, list));
    } catch (e) {
      setError(errMsg(e));
    }
  }, []);

  const createProject = useCallback(
    async (name: string, brief: string, workspace?: Workspace | null, engineeringId?: string | null) => {
      try {
        const p = await api.createProject({
          name,
          brief,
          workspace,
          ...(engineeringId ? { engineering_id: engineeringId } : {}),
        });
        setProjects(list => upsertById(list, p));
        return true;
      } catch (e) {
        setError(errMsg(e));
        return false;
      }
    },
    [],
  );

  /** 工程（完整产品）：创建 / 更新；成功后并入本地缓存 */
  const createEngineering = useCallback(
    async (body: {
      name: string;
      brief: string;
      workspace_kind: 'single' | 'multi' | 'plain';
      workspace_path?: string | null;
      constraints?: string | null;
    }) => {
      try {
        const e = await api.createEngineering(body);
        setEngineerings(list => upsertById(list, e));
        return e;
      } catch (err) {
        setError(errMsg(err));
        return null;
      }
    },
    [],
  );

  const updateEngineering = useCallback(
    async (id: string, body: Partial<Omit<Engineering, 'id' | 'created_at'>>) => {
      try {
        const e = await api.updateEngineering(id, body);
        setEngineerings(list => upsertById(list, e));
        return true;
      } catch (err) {
        setError(errMsg(err));
        return false;
      }
    },
    [],
  );

  const updateProject = useCallback(
    async (id: string, value: { name?: string; brief?: string; workspace?: Workspace | null }) => {
      try {
        const p = await api.updateProject(id, value);
        setProjects(list => upsertById(list, p));
        return true;
      } catch (e) {
        setError(errMsg(e));
        return false;
      }
    },
    [],
  );

  /** 自动规划：同步阻塞请求可能等待数分钟，成功后将新增任务并入任务列表。
   *  规划状态保存在全局 store，切到其他视图再切回不会丢失进度。 */
  const planProject = useCallback(
    async (id: string, opts?: { planner_agent_id?: string; executor_agent_id?: string; acceptor_agent_id?: string; feedback?: string }) => {
      if (planningRef.current) return -1;
      planningRef.current = { projectId: id, startedAt: Date.now() };
      setPlanning(planningRef.current);
      try {
        const res = await api.planProject(id, opts);
        const tasks = res.tasks_added ?? [];
        setTasksByProject(m => {
          const list = m[id] ?? [];
          const next = list.slice();
          for (const t of tasks) {
            const i = next.findIndex(x => x.id === t.id);
            if (i >= 0) next[i] = t;
            else next.push(t);
          }
          return { ...m, [id]: next };
        });
        void loadProjectTasks(id); // 刷新任务列表与计数
        return tasks.length;
      } catch (e) {
        setError(errMsg(e));
        return -1;
      } finally {
        planningRef.current = null;
        setPlanning(null);
      }
    },
    [loadProjectTasks],
  );

  const intervene = useCallback(
    async (projectId: string, note: string) => {
      try {
        return await api.intervene(projectId, note);
      } catch (e) {
        setError(errMsg(e));
        return null;
      }
    },
    [],
  );

  const applyIntervention = useCallback(
    async (projectId: string, changes: ReplanChange[]) => {
      try {
        const res = await api.applyIntervention(projectId, changes);
        void loadProjectTasks(projectId);
        return res;
      } catch (e) {
        setError(errMsg(e));
        return null;
      }
    },
    [loadProjectTasks],
  );

  const applyMilestoneWindow = useCallback(
    async (projectId: string, milestoneId: string, changes: ReplanChange[], note: string) => {
      try {
        const res = await api.applyMilestoneWindow(milestoneId, changes, note);
        void loadProjectTasks(projectId);
        return { applied: res.applied, skipped: res.skipped };
      } catch (e) {
        setError(errMsg(e));
        return null;
      }
    },
    [loadProjectTasks],
  );

  const projectAction = useCallback(async (id: string, action: 'confirm' | 'pause' | 'resume' | 'cancel', force?: boolean) => {
    try {
      const p = await api.projectAction(id, action, force);
      setProjects(list => upsertById(list, p));
    } catch (e) {
      setError(errMsg(e));
    }
  }, []);

  const loadReport = useCallback(
    async (projectId: string) => {
      try {
        const { report } = await api.getProjectReport(projectId);
        if (report) mergeReport(report);
        return report;
      } catch (e) {
        setError(errMsg(e));
        return null;
      }
    },
    [mergeReport],
  );

  const generateReport = useCallback(
    async (projectId: string) => {
      try {
        const { report } = await api.generateProjectReport(projectId);
        mergeReport(report);
        return report;
      } catch (e) {
        setError(errMsg(e));
        return null;
      }
    },
    [mergeReport],
  );

  const submitTaskForm = useCallback(
    async (projectId: string, value: TaskFormValue, editTaskId?: string) => {
      try {
        const t = editTaskId
          ? await api.updateTask(editTaskId, value)
          : await api.createTask(projectId, value);
        setTasksByProject(m => {
          const list = m[projectId] ?? [];
          return { ...m, [projectId]: upsertById(list, t) };
        });
        setTaskDetail(d => (d && d.id === t.id ? { ...d, ...t } : d));
        return true;
      } catch (e) {
        setError(errMsg(e));
        return false;
      }
    },
    [],
  );

  const reloadTaskDetail = useCallback(async (id: string) => {
    try {
      const d = await api.getTaskDetail(id);
      setTaskDetail(cur => (cur && cur.id === id ? d : cur));
      setLogsByTask(m => ({ ...m, [id]: d.logs }));
    } catch (e) {
      setError(errMsg(e));
    }
  }, []);

  const sendGuidance = useCallback(
    async (taskId: string, text: string) => {
      try {
        await api.sendGuidance(taskId, text);
        await reloadTaskDetail(taskId);
        return true;
      } catch (e) {
        setError(errMsg(e));
        return false;
      }
    },
    [reloadTaskDetail],
  );

  const reviewTask = useCallback(
    async (taskId: string, verdict: 'accept' | 'reject', comment: string) => {
      try {
        if (verdict === 'accept') await api.acceptTask(taskId, comment);
        else await api.rejectTask(taskId, comment);
        await reloadTaskDetail(taskId);
        return true;
      } catch (e) {
        setError(errMsg(e));
        return false;
      }
    },
    [reloadTaskDetail],
  );

  const reportReplan = useCallback(
    async (
      taskId: string,
      body: { reason: string; detail?: string; proposed_changes?: ReplanChange[] },
    ) => {
      try {
        const res = await api.reportReplan(taskId, { reported_by: 'human', ...body });
        await reloadTaskDetail(taskId);
        return res;
      } catch (e) {
        setError(errMsg(e));
        return null;
      }
    },
    [reloadTaskDetail],
  );

  const createAgent = useCallback(async (value: AgentFormValue) => {
    try {
      const adapter: Adapter = value.adapter === 'opencode' ? 'opencode' : 'mock';
      const config: { duration_ms?: number; model?: string; timeout_ms?: number } =
        adapter === 'mock'
          ? { duration_ms: value.duration_ms }
          : {
              ...(value.model && value.model.trim() ? { model: value.model.trim() } : {}),
              ...(value.timeout_ms && value.timeout_ms > 0 ? { timeout_ms: value.timeout_ms } : {}),
            };
      const a = await api.createAgent({
        name: value.name,
        adapter,
        roles: value.roles,
        // opencode CLI --session 天然支持恢复；mock 不支持
        capabilities: {
          supports_resume: adapter === 'opencode',
          max_parallel: value.max_parallel,
        },
        config,
      });
      setAgents(list => upsertById(list, a));
      return true;
    } catch (e) {
      setError(errMsg(e));
      return false;
    }
  }, []);

  const resolveInboxItem = useCallback(async (id: string, resolution: string, note: string, agentId?: string, criteria?: string) => {
    try {
      const item = await api.resolveInbox(id, resolution, note, agentId, criteria);
      setInbox(list => upsertById(list, item));
      return true;
    } catch (e) {
      setError(errMsg(e));
      return false;
    }
  }, []);

  const updateAgent = useCallback(async (id: string, value: AgentFormValue) => {
    try {
      const a = await api.updateAgent(id, {
        name: value.name,
        roles: value.roles,
        config: value.adapter === 'opencode'
          ? {
              ...(value.model && value.model.trim() ? { model: value.model.trim() } : {}),
              ...(value.timeout_ms && value.timeout_ms > 0 ? { timeout_ms: value.timeout_ms } : {}),
            }
          : { duration_ms: value.duration_ms },
        capabilities: { max_parallel: value.max_parallel },
      });
      setAgents(list => upsertById(list, a));
      return true;
    } catch (e) {
      setError(errMsg(e));
      return false;
    }
  }, []);

  const deleteAgent = useCallback(async (id: string) => {
    try {
      await api.deleteAgent(id);
      setAgents(list => list.filter(a => a.id !== id));
      return true;
    } catch (e) {
      setError(errMsg(e));
      return false;
    }
  }, []);

  const store: DomoStore = {
    conn,
    view,
    setView,
    error,
    setError,
    projects,
    engineerings,
    agents,
    inbox,
    events,
    tasksByProject,
    milestonesByProject,
    logsByTask,
    reports,
    selectedProjectId,
    selectedTaskId,
    taskDetail,
    tasksViewMode,
    dagDir,
    setDagDir,
    focusAgentId,
    setFocusAgentId,
    knowledgePrefill,
    distillGuidance,
    setTasksViewMode,
    focusedInboxId,
    focusInbox,
    selectProject,
    openTask,
    closeTask,
    refreshEvents,
    createProject,
    createEngineering,
    updateEngineering,
    updateProject,
    projectAction,
    planProject,
    intervene,
    applyIntervention,
    applyMilestoneWindow,
    loadReport,
    generateReport,
    planning,
    submitTaskForm,
    sendGuidance,
    reviewTask,
    reportReplan,
    createAgent,
    updateAgent,
    deleteAgent,
    resolveInboxItem,
  };

  return <DomoCtx.Provider value={store}>{children}</DomoCtx.Provider>;
}
