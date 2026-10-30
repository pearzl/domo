import type { Task } from "../types";
import { queueEdge } from "../taskQueue";

/// 任务列表的筛选、编号、草稿校验与队列位置摘要都是纯派生：
/// 只读取页面已有的任务数据，不取值也不发请求。判定口径与服务端保持一致，
/// 保证界面提示与实际提交结果一致。

/// 任务工具栏的筛选标签；顺序即界面展示顺序。
export type TaskFilter =
  | "all"
  | "active"
  | "queued"
  | "pending"
  | "attention"
  | "completed"
  | "canceled";

export const TASK_FILTERS: { id: TaskFilter; label: string }[] = [
  { id: "all", label: "全部" },
  { id: "active", label: "进行中" },
  { id: "queued", label: "排队" },
  { id: "pending", label: "待启动" },
  { id: "attention", label: "待处理" },
  { id: "completed", label: "完成" },
  { id: "canceled", label: "已取消" },
];

/// 进行中：任务正在执行、验收或运行。
function isRunning(task: Task): boolean {
  return (
    task.status === "running" ||
    task.stage === "executing" ||
    task.stage === "verifying"
  );
}

/// 排队：等待调度、尚未开始执行的任务，含暂停后重新等待派发的待启动任务。
function isQueued(task: Task): boolean {
  if (task.status === "canceled" || task.stage === "completed") return false;
  if (isAttention(task)) return false;
  if (isRunning(task)) return false;
  return task.stage === "pending";
}

/// 待处理：阻塞、暂停或暂停中，需要人工或等待恢复的任务。
function isAttention(task: Task): boolean {
  if (task.status === "canceled") return false;
  return (
    task.status === "blocked" ||
    task.status === "paused" ||
    task.status === "pausing"
  );
}

export function matchesTaskFilter(task: Task, filter: TaskFilter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "active":
      return isRunning(task);
    case "queued":
      return isQueued(task);
    case "pending":
      return task.status !== "canceled" && task.stage === "pending";
    case "attention":
      return isAttention(task);
    case "completed":
      return task.status !== "canceled" && task.stage === "completed";
    case "canceled":
      return task.status === "canceled";
    default:
      return false;
  }
}

/// 各筛选标签的计数；排序与筛选标签一致。
export function taskFilterCounts(tasks: Task[]): Record<TaskFilter, number> {
  const counts: Record<TaskFilter, number> = {
    all: tasks.length,
    active: 0,
    queued: 0,
    pending: 0,
    attention: 0,
    completed: 0,
    canceled: 0,
  };
  for (const task of tasks) {
    for (const { id } of TASK_FILTERS) {
      if (id !== "all" && matchesTaskFilter(task, id)) counts[id] += 1;
    }
  }
  return counts;
}

/// 按 key 或标题搜索并应用筛选标签；保持任务原有顺序。
export function filterTasks(
  tasks: Task[],
  query: string,
  filter: TaskFilter,
): Task[] {
  const needle = query.trim().toLowerCase();
  return tasks.filter((task) => {
    if (!matchesTaskFilter(task, filter)) return false;
    if (!needle) return true;
    return (
      task.key.toLowerCase().includes(needle) ||
      task.title.toLowerCase().includes(needle)
    );
  });
}

/// 搜索结果是否为空：搜索词或筛选标签任一处于生效状态且没有匹配项。
export function hasActiveQuery(query: string, filter: TaskFilter): boolean {
  return query.trim().length > 0 || filter !== "all";
}

const KEY_PATTERN = /^T(\d+)$/;

/// 下一个可用的 T 编号：取现有 T<n> 编号的最大值加一，跳过已占用的编号。
/// 没有 T 编号的任务从 T1 开始；非 T 编号的 key 不参与推算。
export function nextTaskKey(tasks: Task[]): string {
  const used = new Set<string>();
  let max = 0;
  for (const task of tasks) {
    used.add(task.key);
    const match = KEY_PATTERN.exec(task.key);
    if (match) {
      const value = Number.parseInt(match[1], 10);
      if (value > max) max = value;
    }
  }
  let next = max + 1;
  while (used.has(`T${next}`)) next += 1;
  return `T${next}`;
}

export interface TaskDraft {
  key: string;
  title: string;
  description: string;
  deps: string[];
}

export interface TaskDraftErrors {
  key: string | null;
  title: string | null;
  description: string | null;
  deps: string | null;
}

export interface TaskDraftCheck {
  valid: boolean;
  errors: TaskDraftErrors;
}

const KEY_MAX_LENGTH = 32;

/// 草稿校验：key 非空、不超长、不与活动任务重复；标题与描述非空；
/// 依赖只能引用活动任务，且不能引用自身。已取消任务不占用 key，
/// 与服务端“活动任务”口径一致，因此先从传入任务中排除已取消任务。
export function validateTaskDraft(
  draft: TaskDraft,
  activeTasks: Task[],
): TaskDraftCheck {
  const key = draft.key.trim();
  const title = draft.title.trim();
  const description = draft.description.trim();
  const activeKeys = new Set(
    activeTasks
      .filter((task) => task.status !== "canceled")
      .map((task) => task.key),
  );

  let keyError: string | null = null;
  if (!key) {
    keyError = "任务 key 不能为空";
  } else if (key.length > KEY_MAX_LENGTH) {
    keyError = `任务 key 不能超过 ${KEY_MAX_LENGTH} 个字符`;
  } else if (activeKeys.has(key)) {
    keyError = `${key} 已存在，请换一个 key`;
  }

  const selfDeps = draft.deps.filter((dep) => dep === key && key !== "");
  const unknownDeps = draft.deps.filter(
    (dep) => !activeKeys.has(dep) && dep !== key,
  );
  let depsError: string | null = null;
  if (selfDeps.length > 0) {
    depsError = "依赖不能引用任务自身";
  } else if (unknownDeps.length > 0) {
    depsError = `依赖不在活动任务中：${unknownDeps.join("、")}`;
  }

  const errors: TaskDraftErrors = {
    key: keyError,
    title: title ? null : "标题不能为空",
    description: description ? null : "描述不能为空",
    deps: depsError,
  };
  return {
    valid: Object.values(errors).every((value) => value === null),
    errors,
  };
}

/// 依赖列表的一项：任务 key、标题与阶段，用于在添加任务表单里识别可依赖任务。
export interface DependencyOption {
  key: string;
  title: string;
  stage: string;
  stageLabel: string;
}

const STAGE_LABELS: Record<string, string> = {
  pending: "待启动",
  executing: "执行",
  verifying: "验收",
  completed: "完成",
};

export function dependencyOptions(tasks: Task[]): DependencyOption[] {
  return tasks.map((task) => ({
    key: task.key,
    title: task.title,
    stage: task.stage,
    stageLabel: STAGE_LABELS[task.stage] ?? task.stage,
  }));
}

export interface QueuePositionSummary {
  /// 任务在可调整队列中的位置，从 1 开始；不在队列时为 0。
  position: number;
  total: number;
  /// 位置摘要文案，如“可调整 2/5”。
  label: string;
  canMoveUp: boolean;
  canMoveDown: boolean;
}

/// 队列位置摘要：复用 queueEdge 的边界判定，只把结果转成界面文案。
export function queueSummary(queueIds: string[], taskId: string): QueuePositionSummary {
  const edge = queueEdge(queueIds, taskId);
  return {
    position: edge.position,
    total: edge.size,
    label: edge.position > 0 ? `可调整 ${edge.position}/${edge.size}` : `未入队 0/${edge.size}`,
    canMoveUp: edge.canMoveUp,
    canMoveDown: edge.canMoveDown,
  };
}

export interface ReplanSummary {
  cancelCount: number;
  cancelKeys: string[];
  keepCount: number;
  keepKeys: string[];
}

/// 重规划预览：未完成任务会退出 DAG 并被取消，已完成任务保留；
/// 已取消任务既不再保留也不再取消，不进入两份清单。
export function replanSummary(tasks: Task[]): ReplanSummary {
  const cancelKeys: string[] = [];
  const keepKeys: string[] = [];
  for (const task of tasks) {
    if (task.status === "canceled") continue;
    if (task.stage === "completed") {
      keepKeys.push(task.key);
    } else {
      cancelKeys.push(task.key);
    }
  }
  return {
    cancelCount: cancelKeys.length,
    cancelKeys,
    keepCount: keepKeys.length,
    keepKeys,
  };
}
