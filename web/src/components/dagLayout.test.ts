import { describe, expect, it } from "vitest";
import type { Task } from "../types";
import {
  DAG_NODE_HEIGHT,
  DAG_NODE_WIDTH,
  type DagPoint,
  type DagRect,
  layoutTaskDag,
} from "./dagLayout";

/// 端点附近的缓冲区：连线的端点落在节点端口上，允许贴着端点所在的节点，
/// 但其余所有线段与折线点都要与任何节点矩形保持这段距离。
const ENDPOINT_BUFFER = 6;

function task(key: string, deps: string[] = [], parentId: string | null = null): Task {
  return {
    id: `id-${key}`,
    project_id: "p1",
    seq: 1,
    queue_order: 1,
    queue_position: null,
    key,
    title: `任务 ${key}`,
    description: "",
    acceptance: [],
    deps,
    parent_id: parentId,
    milestone: false,
    human_gate: null,
    forced_agent_id: null,
    forced_agent_role: null,
    stage: "pending",
    status: null,
    agent_id: null,
    last_agent_id: null,
    worktree: null,
    branch: null,
    artifacts_released_at: null,
    attempts: 0,
    rejections: 0,
    last_error: null,
    last_failure_kind: null,
    waiting_reason: null,
    result_summary: null,
    verifier_issues: [],
    guidance: "",
    created_at: "2026-01-10T10:00:00Z",
    updated_at: "2026-01-10T10:00:00Z",
  };
}

function expand(rect: DagRect, by: number): DagRect {
  return {
    x: rect.x - by,
    y: rect.y - by,
    width: rect.width + by * 2,
    height: rect.height + by * 2,
  };
}

function rectContains(rect: DagRect, point: DagPoint): boolean {
  const [x, y] = point;
  return (
    x >= rect.x &&
    x <= rect.x + rect.width &&
    y >= rect.y &&
    y <= rect.y + rect.height
  );
}

/// 线段与矩形是否相交（Liang–Barsky 裁剪）。
function segmentIntersectsRect(
  start: DagPoint,
  end: DagPoint,
  rect: DagRect,
): boolean {
  const [x0, y0] = start;
  const [x1, y1] = end;
  if (rectContains(rect, start) || rectContains(rect, end)) return true;
  const dx = x1 - x0;
  const dy = y1 - y0;
  let t0 = 0;
  let t1 = 1;
  const checks: Array<[number, number]> = [
    [-dx, x0 - rect.x],
    [dx, rect.x + rect.width - x0],
    [-dy, y0 - rect.y],
    [dy, rect.y + rect.height - y0],
  ];
  for (const [p, q] of checks) {
    if (p === 0) {
      if (q < 0) return false;
      continue;
    }
    const r = q / p;
    if (p < 0) {
      if (r > t1) return false;
      if (r > t0) t0 = r;
    } else {
      if (r < t0) return false;
      if (r < t1) t1 = r;
    }
  }
  return t1 - t0 >= 0;
}

interface Intersection {
  edgeId: string;
  nodeKey: string;
  /** segment = 第几段；vertex = 第几个折线点。 */
  at: string;
}

/// 两类断言：
/// 1. 任何线段都不能进入任何节点矩形的内部（边界仅相切不算）；
/// 2. 除首末两个端口点之外的折线点，都要与所有节点矩形保持 ENDPOINT_BUFFER 的间距；
///    首末点只允许贴在自己所属的节点端口上，与其它节点同样要留出间距。
function findIntersections(layout: ReturnType<typeof layoutTaskDag>): Intersection[] {
  const hits: Intersection[] = [];
  const shrunk = (rect: DagRect) => expand(rect, -0.05);
  for (const edge of layout.edges) {
    const segments = edge.points.length - 1;
    // 沿路径的累计长度，用来给端点附近的缓冲区留出豁免区间。
    const arcLength: number[] = [0];
    for (let index = 1; index <= segments; index += 1) {
      const previous = edge.points[index - 1];
      const current = edge.points[index];
      arcLength.push(
        arcLength[index - 1] +
          Math.hypot(current[0] - previous[0], current[1] - previous[1]),
      );
    }
    const total = arcLength[segments] ?? 0;
    for (let index = 0; index < segments; index += 1) {
      const start = edge.points[index];
      const end = edge.points[index + 1];
      for (const node of layout.nodes) {
        if (segmentIntersectsRect(start, end, shrunk(node.bounds))) {
          hits.push({ edgeId: edge.id, nodeKey: node.task.key, at: `segment#${index}` });
        }
      }
    }
    edge.points.forEach((point, index) => {
      const isPort = index === 0 || index === segments;
      // 端点缓冲区豁免：路径首末 ENDPOINT_BUFFER 长度以内允许贴着端口节点。
      if (!isPort && arcLength[index] < ENDPOINT_BUFFER) return;
      if (!isPort && total - arcLength[index] < ENDPOINT_BUFFER) return;
      for (const node of layout.nodes) {
        if (!rectContains(expand(node.bounds, ENDPOINT_BUFFER), point)) continue;
        // 端口点允许落在自己这条边的源/目标节点上。
        if (isPort && (node.task.id === edge.from || node.task.id === edge.to)) continue;
        hits.push({ edgeId: edge.id, nodeKey: node.task.key, at: `vertex#${index}` });
      }
    });
  }
  return hits;
}

describe("layoutTaskDag 连线避让", () => {
  it("每个节点矩形都保存了边界，且与节点位置一致", () => {
    const layout = layoutTaskDag([task("A"), task("B", ["A"]), task("C", ["A", "B"])]);
    expect(layout.nodes.length).toBe(3);
    for (const node of layout.nodes) {
      expect(node.bounds).toEqual({
        x: node.x,
        y: node.y,
        width: DAG_NODE_WIDTH,
        height: DAG_NODE_HEIGHT,
      });
    }
  });

  it("跨层依赖 A→C 与 A→B→C 混合 DAG 的每条边都不穿过任何节点矩形", () => {
    const layout = layoutTaskDag([
      task("A"),
      task("B", ["A"]),
      task("C", ["A", "B"]),
    ]);
    const levels = new Map(layout.nodes.map((node) => [node.task.key, node.level]));
    expect(levels.get("A")).toBe(0);
    expect(levels.get("B")).toBe(1);
    expect(levels.get("C")).toBe(2);

    const cross = layout.edges.find(
      (edge) => edge.from === "id-A" && edge.to === "id-C",
    );
    expect(cross).toBeDefined();
    expect(cross?.crossLevel).toBe(true);
    expect(cross?.routing).toBe("channel");
    // 跨层折线：至少 5 个顶点，首点从源节点右侧出，末点从目标节点左侧进。
    expect(cross?.points.length).toBeGreaterThanOrEqual(5);
    const sourceNode = layout.nodes.find((node) => node.task.key === "A");
    const targetNode = layout.nodes.find((node) => node.task.key === "C");
    expect(cross?.points[0][0]).toBeCloseTo(
      (sourceNode?.x ?? 0) + DAG_NODE_WIDTH,
      6,
    );
    expect(cross?.points[cross.points.length - 1][0]).toBeCloseTo(targetNode?.x ?? 0, 6);

    // 折线全部是水平或垂直段，垂直段落在两列之间的通道里。
    const points = cross?.points ?? [];
    for (let index = 0; index + 1 < points.length; index += 1) {
      const [x0, y0] = points[index];
      const [x1, y1] = points[index + 1];
      expect(x0 === x1 || y0 === y1).toBe(true);
      if (x0 === x1) {
        expect(x0).toBeGreaterThan((sourceNode?.x ?? 0) + DAG_NODE_WIDTH + 8);
        expect(x0).toBeLessThan((targetNode?.x ?? 0) - 8);
      }
    }

    expect(findIntersections(layout)).toEqual([]);
  });

  it("多行多跨层密集 DAG 的每条边都不穿过任何节点矩形", () => {
    const layout = layoutTaskDag([
      task("A"),
      task("B"),
      task("C", ["A"]),
      task("D", ["B"]),
      task("E", ["A", "B"]),
      task("F", ["C", "A"]),
      task("G", ["D", "C"]),
      task("H", ["A", "E"]),
      task("I", ["F", "H", "D"]),
      task("J", ["B", "I"]),
    ]);
    expect(layout.nodes.length).toBe(10);
    expect(layout.edges.some((edge) => edge.crossLevel)).toBe(true);
    expect(findIntersections(layout)).toEqual([]);
  });

  it("同列环内任务的连线也不穿过节点矩形", () => {
    const layout = layoutTaskDag([
      task("A", ["B"]),
      task("B", ["A"]),
      task("C", ["A"]),
      task("D"),
      task("E", ["D", "A"]),
    ]);
    expect(layout.cycleKeys.length).toBeGreaterThan(0);
    expect(findIntersections(layout)).toEqual([]);
  });

  it("同一中间列的走廊按边数错开，且落在该列节点之外的空白带上", () => {
    const layout = layoutTaskDag([
      task("A"),
      task("B"),
      task("D"),
      task("C", ["A"]),
      task("E", ["C", "D"]),
      task("F", ["B", "C"]),
    ]);
    const crossEdges = layout.edges.filter((edge) => edge.crossLevel);
    expect(crossEdges.length).toBe(2);
    const middle = layout.nodes.find((node) => node.task.key === "C");
    expect(middle).toBeDefined();
    const corridors = crossEdges.map((edge) => edge.points[2][1]);
    expect(Math.abs(corridors[0] - corridors[1])).toBeGreaterThanOrEqual(8);
    for (const corridor of corridors) {
      const clearOfNode =
        corridor < (middle?.y ?? 0) - 4 ||
        corridor > (middle?.y ?? 0) + DAG_NODE_HEIGHT + 4;
      expect(clearOfNode).toBe(true);
    }
    expect(findIntersections(layout)).toEqual([]);
  });

  it("派生边（含跨回左侧）也不穿过节点矩形", () => {
    const tasks = [
      task("P"),
      task("A", ["P"]),
      task("C", ["A"]),
      task("K", [], "id-C"),
      task("L", [], "id-A"),
    ];
    const layout = layoutTaskDag(tasks);
    const backward = layout.edges.filter((edge) => edge.kind === "derived");
    expect(backward.length).toBe(2);
    expect(backward.some((edge) => edge.routing === "channel")).toBe(true);
    expect(findIntersections(layout)).toEqual([]);
  });

  it("通道索引按边数分配：跨层边在层间通道里的横向位置互不重叠", () => {
    // 列分配算法只把有中间依赖的任务推到更深一层，所以 A→C 与 A→D 各自经过
    // 一列只有一个任务的中间层：两条边在同一段列间通道里要占不同的横向位置。
    const layout = layoutTaskDag([
      task("A"),
      task("B", ["A"]),
      task("C", ["A"]),
      task("D", ["A", "B"]),
      task("E", ["A", "C"]),
    ]);
    const crossEdges = layout.edges.filter((edge) => edge.crossLevel);
    expect(crossEdges.length).toBeGreaterThanOrEqual(2);
    const firstLaneX = crossEdges.map((edge) => edge.points[1][0]);
    expect(new Set(firstLaneX.map((value) => value.toFixed(2))).size).toBe(
      firstLaneX.length,
    );
    const columnRight = Math.max(
      ...layout.nodes
        .filter((node) => node.level === 0)
        .map((node) => node.x + DAG_NODE_WIDTH),
    );
    for (const x of firstLaneX) {
      expect(x).toBeGreaterThan(columnRight + 8);
    }
    expect(findIntersections(layout)).toEqual([]);
  });
});
