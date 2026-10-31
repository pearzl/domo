import type { AuditEvent, JobSummary, Project, Task } from "../types";
/// 任务阶段文案统一取自 ui.tsx，执行现场与任务列表不会出现两套措辞。
import { taskStageLabel } from "../ui";

/// 执行现场只读取项目详情里已有的 tasks / jobs / events，不发起任何请求。
/// 页面是否刷新由外层已有的取数决定，这里只做纯派生。

export const RECENT_EVENT_LIMIT = 3;

/// 与 ui.tsx 的角色标签一致；表现层保持纯粹，不依赖组件模块。
const ROLE_TEXT: Record<string, string> = {
  analysis: "项目分析",
  direct: "直接完成",
  planner: "规划",
  executor: "执行",
  verifier: "验收",
  summary: "总结",
};

/// 配置类事件在执行现场属于噪音，只在事件时间线的“配置”视图展示。
const CONFIG_KINDS = new Set([
  "project_agents_updated",
  "task_agents_updated",
  "default_team_updated",
  "agent_usage_reset",
  "analysis_policy_updated",
  "agent_unavailable",
]);

/// “关键”事件里保留的作业节点：失败、卡死与恢复，其余作业阶段由运行中作业承担。
/// 与 components/EventTimeline.tsx 的关键过滤集合保持一致。
const KEY_JOB_KINDS = new Set([
  "job_failed",
  "job_stalled",
  "job_recovered",
  "job_stall_recovered",
]);

const JOB_KINDS = new Set([
  "job_queued",
  "job_started",
  "job_continued",
  "job_finished",
  "job_failed",
  "job_stalled",
  "job_stall_recovered",
  "job_recovered",
]);

export interface ExecutionInput {
  project: Project;
  tasks: Task[];
  jobs: JobSummary[];
  events: AuditEvent[];
  /// 计算已运行时长的参照时刻；页面传入当前时间，调用方可固定时间。
  now: number;
}

export interface ExecutionJobItem {
  id: string;
  /// 作业角色原值，用于区分项目作业与任务作业。
  role: string;
  roleLabel: string;
  agentName: string | null;
  /// 项目级作业（分析、直接完成、总结）没有所属任务。
  taskKey: string | null;
  taskTitle: string | null;
  attempt: number;
  startedAt: string | null;
  /// 已运行时长；开始时间缺失或不可解析时为未知。
  elapsedMs: number | null;
}

export interface ExecutionQueueItem {
  id: string;
  key: string;
  title: string;
  stage: string;
  stageLabel: string;
  /// 队列位置；未排入队列时为 null。
  position: number | null;
  positionLabel: string;
  /// 服务端给出的等待原因，缺失时按状态补一个默认说明。
  waitingReason: string;
}

export interface ExecutionEventItem {
  id: number;
  kind: string;
  message: string;
  createdAt: string;
  taskKey: string | null;
  jobId: string | null;
}

export type ExecutionIdleTone = "muted" | "info" | "warn" | "danger";

export interface ExecutionIdle {
  tone: ExecutionIdleTone;
  title: string;
  detail: string | null;
}

export interface ExecutionNow {
  /// 运行中的项目作业与任务作业，按开始时间先后排列。
  running: ExecutionJobItem[];
  /// 排队与待启动任务，按队列位置、任务序号排列。
  queue: ExecutionQueueItem[];
  /// 最近的关键事件，最新在前。
  events: ExecutionEventItem[];
  /// 没有运行中作业时说明当前阻塞原因或下一个排队任务。
  idle: ExecutionIdle;
}

export function executionNow(input: ExecutionInput): ExecutionNow {
  const taskMap = new Map<string, Task>();
  for (const task of input.tasks) taskMap.set(task.id, task);
  const queue = queuedTasks(input.tasks);
  return {
    running: runningJobs(input.jobs, taskMap, input.now),
    queue,
    events: recentKeyEvents(input.events, taskMap, RECENT_EVENT_LIMIT),
    idle: idleReason(input.project, queue),
  };
}

/// 只保留运行中的作业；项目作业在前、任务作业按开始时间排序。
export function runningJobs(
  jobs: JobSummary[],
  taskMap: Map<string, Task>,
  now: number,
): ExecutionJobItem[] {
  const items = jobs
    .filter((job) => job.status === "running")
    .map((job) => {
      const task = job.task_id ? taskMap.get(job.task_id) : undefined;
      return {
        id: job.id,
        role: job.role,
        roleLabel: ROLE_TEXT[job.role] ?? job.role,
        agentName: job.agent_name,
        taskKey: task?.key ?? null,
        taskTitle: task?.title ?? null,
        attempt: job.attempt,
        startedAt: job.started_at,
        elapsedMs: jobElapsedMs(job, now),
      } satisfies ExecutionJobItem;
    });
  return items.sort((left, right) => {
    const leftStart = left.startedAt ? Date.parse(left.startedAt) : NaN;
    const rightStart = right.startedAt ? Date.parse(right.startedAt) : NaN;
    if (Number.isNaN(leftStart) && Number.isNaN(rightStart)) return 0;
    if (Number.isNaN(leftStart)) return 1;
    if (Number.isNaN(rightStart)) return -1;
    return leftStart - rightStart;
  });
}

/// 已运行时长的终点取结束时刻或参照时刻；开始时间缺失、不可解析或晚于终点时为未知。
export function jobElapsedMs(job: JobSummary, now: number): number | null {
  if (!job.started_at) return null;
  const start = Date.parse(job.started_at);
  if (Number.isNaN(start)) return null;
  const end = job.finished_at ? Date.parse(job.finished_at) : now;
  if (Number.isNaN(end) || end < start) return null;
  return end - start;
}

export function formatElapsedLabel(milliseconds: number | null): string {
  if (milliseconds === null || milliseconds < 0) return "时长未知";
  const seconds = Math.floor(milliseconds / 1000);
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分 ${seconds % 60} 秒`;
  const hours = Math.floor(minutes / 60);
  return `${hours} 时 ${minutes % 60} 分`;
}

/// 排队与待启动任务按队列位置排序，没有位置的任务排在后面并按任务序号先后排列。
export function queuedTasks(tasks: Task[]): ExecutionQueueItem[] {
  const pending = tasks.filter((task) => isWaitingTask(task));
  pending.sort((left, right) => {
    const leftPosition = left.queue_position;
    const rightPosition = right.queue_position;
    if (leftPosition !== null && rightPosition !== null) {
      if (leftPosition !== rightPosition) return leftPosition - rightPosition;
    } else if (leftPosition !== null) {
      return -1;
    } else if (rightPosition !== null) {
      return 1;
    }
    return left.seq - right.seq;
  });
  return pending.map((task) => ({
    id: task.id,
    key: task.key,
    title: task.title,
    stage: task.stage,
    stageLabel: taskStageLabel(task.stage),
    position: task.queue_position,
    positionLabel:
      task.queue_position === null ? "未入队" : `队列第 ${task.queue_position} 位`,
    waitingReason: waitingReason(task),
  }));
}

/// 等待中的任务：仍在待启动阶段且排队中，且不在运行、完成、取消、暂停或阻塞中。
export function isWaitingTask(task: Task): boolean {
  if (task.status === "canceled" || task.stage === "completed") return false;
  if (
    task.status === "paused" ||
    task.status === "pausing" ||
    task.status === "blocked"
  ) {
    return false;
  }
  if (task.status === "running") return false;
  return task.stage === "pending";
}

/// 等待原因优先采用服务端说明，缺失时按任务状态给出可读的默认说明。
export function waitingReason(task: Task): string {
  const reason = task.waiting_reason?.trim();
  if (reason) return reason;
  if (task.status === "queued") return "等待派发";
  if (task.stage === "pending") return "等待调度";
  return "等待派发";
}

/// 关键事件：非配置类，作业事件只保留失败、卡死与恢复；最新 limit 条，最新在前。
export function recentKeyEvents(
  events: AuditEvent[],
  taskMap: Map<string, Task>,
  limit: number = RECENT_EVENT_LIMIT,
): ExecutionEventItem[] {
  const key = events.filter((event) => isKeyEvent(event.kind));
  const recent = limit > 0 ? key.slice(-limit) : [];
  return recent.reverse().map((event) => ({
    id: event.id,
    kind: event.kind,
    message: event.message,
    createdAt: event.created_at,
    taskKey: event.task_id ? taskMap.get(event.task_id)?.key ?? null : null,
    jobId: event.job_id,
  }));
}

export function isKeyEvent(kind: string): boolean {
  if (CONFIG_KINDS.has(kind)) return false;
  if (JOB_KINDS.has(kind)) return KEY_JOB_KINDS.has(kind);
  return true;
}

/// 没有运行中作业时，先说明阻塞与暂停，再给出下一个排队任务，最后回到阶段说明。
export function idleReason(
  project: Project,
  queue: ExecutionQueueItem[],
): ExecutionIdle {
  if (project.status === "archived") {
    return { tone: "muted", title: "项目已归档", detail: "只读状态，没有在执行的工作。" };
  }
  if (project.status === "paused" || project.status === "pausing") {
    return {
      tone: "warn",
      title: "项目已暂停",
      detail: "恢复项目后继续派发任务。",
    };
  }
  if (project.status === "blocked") {
    return {
      tone: "danger",
      title: "等待人工处理",
      detail: firstLine(project.blocked_reason) ?? "查看阻塞原因并处理后再推进。",
    };
  }

  const next = queue[0];
  if (next) {
    return {
      tone: "info",
      title: `下一个排队任务：${next.key}`,
      detail: `${next.title}（${next.waitingReason}）`,
    };
  }

  return {
    tone: "muted",
    title: "没有运行中的作业",
    detail: stageHint(project.stage),
  };
}

function stageHint(stage: string): string {
  switch (stage) {
    case "analysis":
      return "分析阶段尚未派发作业，请检查目标与方案是否已提交。";
    case "executing":
      return "执行阶段没有运行、排队或待启动的任务，请检查阻塞原因或重新规划。";
    case "summarizing":
      return "总结阶段没有运行中的作业，报告可能已就绪或等待重新生成。";
    case "completed":
      return "项目已完成，没有正在执行的工作。";
    default:
      return "当前没有正在执行的工作。";
  }
}

function firstLine(value: string | null | undefined, max = 200): string | null {
  const line = (value ?? "")
    .split(/\r?\n/)
    .map((item) => item.trim())
    .find((item) => item.length > 0);
  if (!line) return null;
  const chars = Array.from(line);
  if (chars.length <= max) return line;
  return `${chars.slice(0, max - 1).join("")}…`;
}
