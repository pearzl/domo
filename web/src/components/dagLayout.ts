import type { Task } from "../types";

export const DAG_NODE_WIDTH = 236;
export const DAG_NODE_HEIGHT = 124;

const COLUMN_GAP = 104;
const ROW_GAP = 24;
const PADDING = 24;

/// 垂直通道与列边界（也就是节点矩形的左右边）之间的最小间距，
/// 保证连线在列间通道里行走时不会贴到任何节点矩形。
const LANE_CLEARANCE = 10;
/// 同一层间区段内相邻通道的步长；通道过多时按可用宽度收缩。
const LANE_STEP = 12;
/// 折线横向走廊与节点矩形之间的最小间距。
const CORRIDOR_PADDING = 6;
/// 同一列上相邻走廊的最小间距，避免多条跨层边重叠成一条线。
const CORRIDOR_SEPARATION = 10;
/// 层间区段编号：-1 表示最左列左侧的空白边距。
const LEFT_MARGIN_GAP = -1;

export type DagEdgeKind = "dependency" | "derived";

/** 折线顶点；只读二元组便于直接断言坐标。 */
export type DagPoint = readonly [number, number];

export interface DagRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type DagEdgeRouting = "curve" | "channel";

export interface DagNodeLayout {
  task: Task;
  level: number;
  row: number;
  x: number;
  y: number;
  /** 节点矩形的边界，连线避让与测试断言都用它。 */
  bounds: DagRect;
}

export interface DagEdgeLayout {
  id: string;
  from: string;
  to: string;
  kind: DagEdgeKind;
  path: string;
  /** 路径顶点：首点是源端口，末点是目标端口，中间是折线/采样点。 */
  points: DagPoint[];
  /** 跨层依赖：目标层级 > 来源层级 + 1，走层间垂直通道折线。 */
  crossLevel: boolean;
  routing: DagEdgeRouting;
}

export interface DagLevel {
  level: number;
  /** 列头文案，例如“第 1 层”。 */
  label: string;
  /** 该层的任务数。 */
  count: number;
  x: number;
  width: number;
}

export interface MissingDependency {
  taskKey: string;
  dep: string;
}

export interface DagLayout {
  nodes: DagNodeLayout[];
  edges: DagEdgeLayout[];
  levels: DagLevel[];
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

interface PlannedEdge {
  spec: EdgeSpec;
  startX: number;
  startY: number;
  endX: number;
  endY: number;
  /** 目标层级 - 来源层级。 */
  span: number;
  /** 走通道折线时依次经过的层间区段编号。 */
  gaps: number[];
  /** 是否使用通道折线路由（跨层或同列）。 */
  channel: boolean;
}

/// 布局只依赖任务的 deps 与 parent_id：取消的任务不进入图，缺失依赖被忽略，
/// 环内任务统一放到最右侧一列，保证布局计算在异常数据下仍能终止。
/// 列分配只由依赖层级决定；连线在列间通道里折线行走，避让所有节点矩形。
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
      const x = PADDING + taskLevel * (DAG_NODE_WIDTH + COLUMN_GAP);
      const y = topOffset + row * (DAG_NODE_HEIGHT + ROW_GAP);
      const node: DagNodeLayout = {
        task,
        level: taskLevel,
        row,
        x,
        y,
        bounds: { x, y, width: DAG_NODE_WIDTH, height: DAG_NODE_HEIGHT },
      };
      nodes.push(node);
      nodeByTaskId.set(task.id, node);
    });
  }

  const width =
    levelCount > 0
      ? PADDING * 2 + levelCount * DAG_NODE_WIDTH + (levelCount - 1) * COLUMN_GAP
      : 0;
  const height =
    maxRows > 0 ? PADDING * 2 + maxRows * DAG_NODE_HEIGHT + (maxRows - 1) * ROW_GAP : 0;

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

  // 第一遍：算出每条边的端口位置、跨了几层，并统计每个层间区段要几条垂直通道。
  const plans: PlannedEdge[] = [];
  const gapDemand = new Map<number, number>();
  for (const spec of specs) {
    const fromNode = nodeByTaskId.get(spec.from.id);
    const toNode = nodeByTaskId.get(spec.to.id);
    if (!fromNode || !toNode) continue;
    const outIndex = outgoingCursor.get(spec.from.id) ?? 0;
    const inIndex = incomingCursor.get(spec.to.id) ?? 0;
    outgoingCursor.set(spec.from.id, outIndex + 1);
    incomingCursor.set(spec.to.id, inIndex + 1);

    const derived = spec.kind === "derived";
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

    const span = toNode.level - fromNode.level;
    const backwards = toNode.x < fromNode.x;
    let startX: number;
    let endX: number;
    let gaps: number[] = [];
    let channel: boolean;

    if (span >= 2) {
      // 跨层（正向）：右侧出、左侧进，中间的每个层间区段各占一条通道。
      startX = fromNode.x + DAG_NODE_WIDTH;
      endX = toNode.x;
      gaps = ascend(fromNode.level, toNode.level - 1);
      channel = true;
    } else if (span <= -2) {
      // 跨层（反向，例如派生关系跨回更左侧）：左侧出、右侧进。
      startX = fromNode.x;
      endX = toNode.x + DAG_NODE_WIDTH;
      gaps = descend(fromNode.level - 1, toNode.level);
      channel = true;
    } else if (span === 0) {
      // 同列（只可能来自环内任务）：绕到右侧通道，再从左侧通道回落到目标。
      startX = fromNode.x + DAG_NODE_WIDTH;
      endX = toNode.x;
      gaps = [fromNode.level, fromNode.level - 1 >= 0 ? fromNode.level - 1 : LEFT_MARGIN_GAP];
      channel = true;
    } else {
      // 相邻层：平滑曲线，端口分别在源节点右侧与目标节点左侧。
      startX = backwards ? fromNode.x : fromNode.x + DAG_NODE_WIDTH;
      endX = backwards ? toNode.x + DAG_NODE_WIDTH : toNode.x;
      channel = false;
    }

    if (channel) {
      for (const gap of gaps) {
        gapDemand.set(gap, (gapDemand.get(gap) ?? 0) + 1);
      }
    }

    plans.push({
      spec,
      startX,
      startY,
      endX,
      endY,
      span,
      gaps,
      channel,
    });
  }

  // 第二遍：按边数给每个层间区段分配通道位置。
  const gapLanes = new Map<number, number[]>();
  for (const [gap, demand] of gapDemand) {
    gapLanes.set(gap, allocateLanes(gap, demand));
  }
  const gapCursor = new Map<number, number>();

  const nodesByLevel = new Map<number, DagNodeLayout[]>();
  for (const node of nodes) {
    const column = nodesByLevel.get(node.level);
    if (column) column.push(node);
    else nodesByLevel.set(node.level, [node]);
  }
  const usedCorridors = new Map<number, number[]>();

  /// 横穿中间列时的走廊 y：必须落在该列节点矩形之间的空白带内，
  /// 同一列已用的走廊就近错开 CORRIDOR_SEPARATION。
  const corridorFor = (crossedLevel: number, desired: number): number => {
    const columnNodes = nodesByLevel.get(crossedLevel) ?? [];
    const ranges = freeBands(columnNodes, height)
      .map((band) => {
        const top = Math.min(band.top + CORRIDOR_PADDING, band.bottom - CORRIDOR_PADDING);
        const bottom = Math.max(band.top + CORRIDOR_PADDING, band.bottom - CORRIDOR_PADDING);
        return { top, bottom };
      })
      .filter((range) => range.bottom >= range.top)
      .sort(
        (a, b) =>
          Math.abs(clamp(desired, a.top, a.bottom) - desired) -
          Math.abs(clamp(desired, b.top, b.bottom) - desired),
      );
    if (ranges.length === 0) {
      return clamp(desired, 2, Math.max(2, height - 2));
    }
    const used = usedCorridors.get(crossedLevel) ?? [];
    const isFree = (value: number) =>
      used.every((existing) => Math.abs(existing - value) >= CORRIDOR_SEPARATION);
    const candidates: number[] = [];
    for (const range of ranges) {
      for (let value = range.top; value < range.bottom; value += CORRIDOR_SEPARATION) {
        candidates.push(value);
      }
      candidates.push(range.bottom);
    }
    candidates.sort((a, b) => Math.abs(a - desired) - Math.abs(b - desired));
    const chosen = candidates.find(isFree) ?? clamp(desired, ranges[0].top, ranges[0].bottom);
    used.push(chosen);
    usedCorridors.set(crossedLevel, used);
    return chosen;
  };

  const edges: DagEdgeLayout[] = [];
  for (const plan of plans) {
    const lanes = plan.gaps.map((gap) => {
      const list = gapLanes.get(gap) ?? [];
      const index = gapCursor.get(gap) ?? 0;
      gapCursor.set(gap, index + 1);
      if (list.length === 0) return gapCenter(gap);
      return list[Math.min(index, list.length - 1)];
    });

    // 两个相邻区段之间横穿的列：区段 g 在 g 列右侧、区段 g-1 在 g 列左侧，
    // 所以“向右下一格”横穿的是 g+1 列，“向左下一格”横穿的是 g 列。
    const corridors: number[] = [];
    for (let index = 0; index + 1 < plan.gaps.length; index += 1) {
      const current = plan.gaps[index];
      const next = plan.gaps[index + 1];
      const crossed = next === current + 1 ? current + 1 : current;
      corridors.push(corridorFor(crossed, (plan.startY + plan.endY) / 2));
    }

    const points = plan.channel
      ? channelPoints(plan.startX, plan.startY, plan.endX, plan.endY, lanes, corridors)
      : curveSamples(plan.startX, plan.startY, plan.endX, plan.endY);
    const path = plan.channel
      ? polylinePath(points)
      : curvePath(plan.startX, plan.startY, plan.endX, plan.endY);

    edges.push({
      id: `${plan.spec.kind}:${plan.spec.from.id}->${plan.spec.to.id}`,
      from: plan.spec.from.id,
      to: plan.spec.to.id,
      kind: plan.spec.kind,
      path,
      points,
      crossLevel: plan.span >= 2,
      routing: plan.channel ? "channel" : "curve",
    });
  }

  const levels: DagLevel[] = levelNumbers.map((taskLevel, index) => ({
    level: taskLevel,
    label: `第 ${index + 1} 层`,
    count: (columns.get(taskLevel) ?? []).length,
    x: PADDING + taskLevel * (DAG_NODE_WIDTH + COLUMN_GAP),
    width: DAG_NODE_WIDTH,
  }));

  return {
    nodes,
    edges,
    levels,
    width,
    height,
    cycleKeys,
    missingDependencies,
  };
}

function ascend(from: number, to: number): number[] {
  const result: number[] = [];
  for (let gap = from; gap <= to; gap += 1) result.push(gap);
  return result;
}

function descend(from: number, to: number): number[] {
  const result: number[] = [];
  for (let gap = from; gap >= to; gap -= 1) result.push(gap);
  return result;
}

interface Band {
  top: number;
  bottom: number;
}

/// 某一列里所有节点矩形都没覆盖到的 y 区间，用于折线横穿该列时的走廊。
function freeBands(columnNodes: DagNodeLayout[], graphHeight: number): Band[] {
  const sorted = [...columnNodes].sort((a, b) => a.y - b.y);
  const bands: Band[] = [];
  let cursor = 0;
  for (const node of sorted) {
    if (node.y > cursor) bands.push({ top: cursor, bottom: node.y });
    cursor = Math.max(cursor, node.y + DAG_NODE_HEIGHT);
  }
  if (graphHeight > cursor) bands.push({ top: cursor, bottom: graphHeight });
  return bands;
}

function clamp(value: number, min: number, max: number): number {
  if (max < min) return min;
  return Math.min(Math.max(value, min), max);
}

/// 层间区段就是第 gap 列右侧到第 gap+1 列左侧之间的空白。
function gapSpan(gap: number): { left: number; right: number } {
  if (gap === LEFT_MARGIN_GAP) return { left: 0, right: PADDING };
  const left = PADDING + gap * (DAG_NODE_WIDTH + COLUMN_GAP) + DAG_NODE_WIDTH;
  return { left, right: left + COLUMN_GAP };
}

function gapCenter(gap: number): number {
  const span = gapSpan(gap);
  return (span.left + span.right) / 2;
}

/// 按该区段的边数分配通道：居中均匀铺开，并与两侧列保持 LANE_CLEARANCE。
function allocateLanes(gap: number, count: number): number[] {
  const span = gapSpan(gap);
  const margin = gap === LEFT_MARGIN_GAP ? 4 : LANE_CLEARANCE;
  const usableLeft = span.left + margin;
  const usableRight = span.right - margin;
  const center = (usableLeft + usableRight) / 2;
  if (count <= 1) return [center];
  const step = Math.min(LANE_STEP, (usableRight - usableLeft) / (count - 1));
  const start = center - ((count - 1) * step) / 2;
  return Array.from({ length: count }, (_, index) => start + index * step);
}

/// 通道折线：源端口水平出门 → 在列间通道里垂直行走 → 横穿中间列（走空白带）
/// → 在目标侧通道里垂直行走 → 水平进入目标端口。
function channelPoints(
  startX: number,
  startY: number,
  endX: number,
  endY: number,
  lanes: number[],
  corridors: number[],
): DagPoint[] {
  const points: DagPoint[] = [
    [startX, startY],
    [lanes[0], startY],
  ];
  for (let index = 0; index + 1 < lanes.length; index += 1) {
    const corridor = corridors[index] ?? (startY + endY) / 2;
    points.push([lanes[index], corridor], [lanes[index + 1], corridor]);
  }
  points.push([lanes[lanes.length - 1], endY], [endX, endY]);
  return dedupePoints(points);
}

function dedupePoints(points: DagPoint[]): DagPoint[] {
  const result: DagPoint[] = [];
  for (const point of points) {
    const previous = result[result.length - 1];
    if (previous && nearlyEqual(previous[0], point[0]) && nearlyEqual(previous[1], point[1])) {
      continue;
    }
    result.push(point);
  }
  return result;
}

function nearlyEqual(a: number, b: number): boolean {
  return Math.abs(a - b) < 0.01;
}

function curveReach(startX: number, endX: number): number {
  return Math.max(48, Math.abs(endX - startX) / 2);
}

function curveDirection(startX: number, endX: number): number {
  return endX >= startX ? 1 : -1;
}

/// 相邻层之间的平滑曲线；控制点沿走向伸出，曲线整体落在两列之间的通道里。
function curvePath(startX: number, startY: number, endX: number, endY: number): string {
  const reach = curveReach(startX, endX);
  const direction = curveDirection(startX, endX);
  const c1x = startX + direction * reach;
  const c2x = endX - direction * reach;
  return `M ${round(startX)} ${round(startY)} C ${round(c1x)} ${round(startY)}, ${round(c2x)} ${round(endY)}, ${round(endX)} ${round(endY)}`;
}

/// 与 curvePath 用同一条三次贝塞尔，采样成折线点供测试与高亮判定使用。
function curveSamples(
  startX: number,
  startY: number,
  endX: number,
  endY: number,
  steps = 32,
): DagPoint[] {
  const reach = curveReach(startX, endX);
  const direction = curveDirection(startX, endX);
  const c1x = startX + direction * reach;
  const c2x = endX - direction * reach;
  const samples: DagPoint[] = [];
  for (let index = 0; index <= steps; index += 1) {
    const t = index / steps;
    const u = 1 - t;
    samples.push([
      u * u * u * startX + 3 * u * u * t * c1x + 3 * u * t * t * c2x + t * t * t * endX,
      u * u * u * startY + 3 * u * u * t * startY + 3 * u * t * t * endY + t * t * t * endY,
    ]);
  }
  return samples;
}

function polylinePath(points: DagPoint[]): string {
  if (points.length === 0) return "";
  const [first, ...rest] = points;
  const head = `M ${round(first[0])} ${round(first[1])}`;
  return [head, ...rest.map((point) => `L ${round(point[0])} ${round(point[1])}`)].join(" ");
}

function round(value: number): number {
  return Math.round(value * 100) / 100;
}

function anchorOffset(index: number, count: number): number {
  if (count <= 1) return 0;
  const spread = Math.min(26, (DAG_NODE_HEIGHT * 0.56) / (count - 1));
  return (index - (count - 1) / 2) * spread;
}
