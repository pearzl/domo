import type { Task } from "../types";

export const DAG_NODE_WIDTH = 236;
export const DAG_NODE_HEIGHT = 124;

const COLUMN_GAP = 104;
const ROW_GAP = 24;
const PADDING = 24;

export type DagEdgeKind = "dependency" | "derived";

export interface DagNodeLayout {
  task: Task;
  level: number;
  row: number;
  x: number;
  y: number;
}

export interface DagEdgeLayout {
  id: string;
  from: string;
  to: string;
  kind: DagEdgeKind;
  path: string;
}

export interface MissingDependency {
  taskKey: string;
  dep: string;
}

export interface DagLayout {
  nodes: DagNodeLayout[];
  edges: DagEdgeLayout[];
  width: number;
  height: number;
  cycleKeys: string[];
  missingDependencies: MissingDependency[];
}

interface EdgeSpec {
  from: Task;
  to: Task;
  kind: DagEdgeKind;
}

/// 布局只依赖任务的 deps 与 parent_id：取消的任务不进入图，缺失依赖被忽略，
/// 环内任务统一放到最右侧一列，保证布局计算在异常数据下仍能终止。
export function layoutTaskDag(tasks: Task[]): DagLayout {
  const active = tasks.filter((task) => task.status !== "canceled");
  const byKey = new Map<string, Task>();
  const byId = new Map<string, Task>();
  for (const task of active) {
    byKey.set(task.key, task);
    byId.set(task.id, task);
  }

  const resolvedDeps = new Map<string, Task[]>();
  const missingDependencies: MissingDependency[] = [];
  const selfCycle = new Set<string>();
  for (const task of active) {
    const seen = new Set<string>();
    const resolved: Task[] = [];
    for (const key of task.deps) {
      if (seen.has(key)) continue;
      seen.add(key);
      if (key === task.key) {
        selfCycle.add(task.key);
        continue;
      }
      const dep = byKey.get(key);
      if (dep) {
        resolved.push(dep);
      } else {
        missingDependencies.push({ taskKey: task.key, dep: key });
      }
    }
    resolvedDeps.set(task.id, resolved);
  }

  const level = new Map<string, number>();
  const indegree = new Map<string, number>();
  const outgoing = new Map<string, string[]>();
  for (const task of active) {
    level.set(task.id, 0);
    indegree.set(task.id, 0);
    outgoing.set(task.id, []);
  }
  for (const task of active) {
    for (const dep of resolvedDeps.get(task.id) ?? []) {
      outgoing.get(dep.id)?.push(task.id);
      indegree.set(task.id, (indegree.get(task.id) ?? 0) + 1);
    }
  }

  const queue = active
    .filter((task) => (indegree.get(task.id) ?? 0) === 0)
    .map((task) => task.id);
  const processed = new Set<string>();
  let processedMaxLevel = -1;
  while (queue.length > 0) {
    const current = queue.shift() as string;
    processed.add(current);
    processedMaxLevel = Math.max(processedMaxLevel, level.get(current) ?? 0);
    for (const next of outgoing.get(current) ?? []) {
      level.set(next, Math.max(level.get(next) ?? 0, (level.get(current) ?? 0) + 1));
      const remaining = (indegree.get(next) ?? 1) - 1;
      indegree.set(next, remaining);
      if (remaining === 0) queue.push(next);
    }
  }

  const leftover = active.filter((task) => !processed.has(task.id));
  const cycleKeys = [
    ...selfCycle,
    ...leftover.map((task) => task.key),
  ].filter((key, index, all) => all.indexOf(key) === index);
  const fallbackLevel = processedMaxLevel + 1;
  for (const task of leftover) level.set(task.id, fallbackLevel);

  const order = new Map(active.map((task, index) => [task.id, index]));
  const columns = new Map<number, Task[]>();
  for (const task of active) {
    const taskLevel = level.get(task.id) ?? 0;
    const column = columns.get(taskLevel) ?? [];
    column.push(task);
    columns.set(taskLevel, column);
  }
  for (const column of columns.values()) {
    column.sort((a, b) => (order.get(a.id) ?? 0) - (order.get(b.id) ?? 0));
  }

  const levelNumbers = [...columns.keys()].sort((a, b) => a - b);
  const levelCount = levelNumbers.length;
  const maxRows = Math.max(0, ...[...columns.values()].map((column) => column.length));

  const nodeByTaskId = new Map<string, DagNodeLayout>();
  const nodes: DagNodeLayout[] = [];
  for (const taskLevel of levelNumbers) {
    const column = columns.get(taskLevel) ?? [];
    const topOffset = PADDING + ((maxRows - column.length) * (DAG_NODE_HEIGHT + ROW_GAP)) / 2;
    column.forEach((task, row) => {
      const node: DagNodeLayout = {
        task,
        level: taskLevel,
        row,
        x: PADDING + taskLevel * (DAG_NODE_WIDTH + COLUMN_GAP),
        y: topOffset + row * (DAG_NODE_HEIGHT + ROW_GAP),
      };
      nodes.push(node);
      nodeByTaskId.set(task.id, node);
    });
  }

  const specs: EdgeSpec[] = [];
  const seenEdges = new Set<string>();
  const addSpec = (from: Task, to: Task, kind: DagEdgeKind) => {
    const id = `${kind}:${from.id}->${to.id}`;
    if (seenEdges.has(id)) return;
    seenEdges.add(id);
    specs.push({ from, to, kind });
  };
  for (const task of active) {
    for (const dep of resolvedDeps.get(task.id) ?? []) {
      addSpec(dep, task, "dependency");
    }
  }
  for (const task of active) {
    if (!task.parent_id) continue;
    const parent = byId.get(task.parent_id);
    if (parent) addSpec(parent, task, "derived");
  }

  const outgoingCount = new Map<string, number>();
  const incomingCount = new Map<string, number>();
  for (const spec of specs) {
    outgoingCount.set(spec.from.id, (outgoingCount.get(spec.from.id) ?? 0) + 1);
    incomingCount.set(spec.to.id, (incomingCount.get(spec.to.id) ?? 0) + 1);
  }
  const outgoingCursor = new Map<string, number>();
  const incomingCursor = new Map<string, number>();

  const edges: DagEdgeLayout[] = [];
  for (const spec of specs) {
    const fromNode = nodeByTaskId.get(spec.from.id);
    const toNode = nodeByTaskId.get(spec.to.id);
    if (!fromNode || !toNode) continue;
    const outIndex = outgoingCursor.get(spec.from.id) ?? 0;
    const inIndex = incomingCursor.get(spec.to.id) ?? 0;
    outgoingCursor.set(spec.from.id, outIndex + 1);
    incomingCursor.set(spec.to.id, inIndex + 1);

    const derived = spec.kind === "derived";
    const startX =
      derived && toNode.x < fromNode.x
        ? fromNode.x
        : fromNode.x + DAG_NODE_WIDTH;
    const endX =
      derived && toNode.x < fromNode.x
        ? toNode.x + DAG_NODE_WIDTH
        : toNode.x;
    const derivedShift = derived ? -7 : 0;
    const startY =
      fromNode.y +
      DAG_NODE_HEIGHT / 2 +
      derivedShift +
      anchorOffset(outIndex, outgoingCount.get(spec.from.id) ?? 1);
    const endY =
      toNode.y +
      DAG_NODE_HEIGHT / 2 +
      derivedShift +
      anchorOffset(inIndex, incomingCount.get(spec.to.id) ?? 1);

    edges.push({
      id: `${spec.kind}:${spec.from.id}->${spec.to.id}`,
      from: spec.from.id,
      to: spec.to.id,
      kind: spec.kind,
      path: edgePath(startX, startY, endX, endY),
    });
  }

  const width =
    levelCount > 0
      ? PADDING * 2 + levelCount * DAG_NODE_WIDTH + (levelCount - 1) * COLUMN_GAP
      : 0;
  const height =
    maxRows > 0 ? PADDING * 2 + maxRows * DAG_NODE_HEIGHT + (maxRows - 1) * ROW_GAP : 0;

  return {
    nodes,
    edges,
    width,
    height,
    cycleKeys,
    missingDependencies,
  };
}

function anchorOffset(index: number, count: number): number {
  if (count <= 1) return 0;
  const spread = Math.min(26, (DAG_NODE_HEIGHT * 0.56) / (count - 1));
  return (index - (count - 1) / 2) * spread;
}

function edgePath(startX: number, startY: number, endX: number, endY: number): string {
  const reach = Math.max(48, Math.abs(endX - startX) / 2);
  return `M ${startX} ${startY} C ${startX + reach} ${startY}, ${endX - reach} ${endY}, ${endX} ${endY}`;
}
