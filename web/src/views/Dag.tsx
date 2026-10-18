import { useMemo } from 'react';
import type { Agent, Milestone, Task } from '../types';
import { MILESTONE_STATUS_LABEL, TASK_STATUS_LABEL } from '../ui';

/** 状态 → 节点描边/强调色（与看板语义一致） */
export const TASK_STATUS_COLOR: Record<string, string> = {
  planned: '#a6b0bf',
  ready: '#58a6ff',
  plan_drafting: '#4c8dff',
  plan_review: '#c9a227',
  executing: '#d9a014',
  awaiting_acceptance: '#a37ff0',
  escalated: '#e5534b',
  suspended: '#e08a3c',
  done: '#4cae6d',
  cancelled: '#6b7280',
};

/** 里程碑泳道配色（按泳道序号轮转） */
const LANE_COLORS = ['#4c8dff', '#a37ff0', '#4cae6d', '#d9a014', '#e08a3c', '#58a6ff', '#e5534b'];

const NODE_W = 179;
const NODE_H = 76;
const GAP_X = 26;
const GAP_Y = 44;
const PAD = 24;
const LANE_LABEL_H = 36;
const LANE_PAD_TOP = 8;
const LANE_PAD_BOTTOM = 14;
const LANE_GAP = 18;

const NONE_LANE_KEY = '__none__';

interface Lane {
  key: string;
  name: string;
  milestone: Milestone | null;
  tasks: Task[];
}

interface NodePos {
  task: Task;
  x: number;
  y: number;
  laneKey: string;
}

/**
 * 最长路径分层：无依赖=0，其余=依赖层最大值+1。
 * 依赖边保证层号严格递增 → 图上每条边都从左向右。
 */
function computeLayers(tasks: Task[]): Map<string, number> {
  const byId = new Map(tasks.map(t => [t.id, t]));
  const layer = new Map<string, number>();
  const visiting = new Set<string>();
  const resolve = (id: string): number => {
    const cached = layer.get(id);
    if (cached !== undefined) return cached;
    if (visiting.has(id)) return 0; // 防御性环路保护
    visiting.add(id);
    const t = byId.get(id);
    let l = 0;
    if (t) {
      for (const d of t.depends_on) {
        if (byId.has(d)) l = Math.max(l, resolve(d) + 1);
      }
    }
    visiting.delete(id);
    layer.set(id, l);
    return l;
  };
  for (const t of tasks) resolve(t.id);
  return layer;
}

/** 按里程碑分组：已知里程碑按后端顺序，未关联任务的泳道置于最后。 */
function buildLanes(tasks: Task[], milestones: Milestone[]): Lane[] {
  const lanes: Lane[] = milestones.map(m => ({ key: m.id, name: m.name, milestone: m, tasks: [] }));
  const byMilestone = new Map(lanes.map(l => [l.key, l]));
  const none: Lane = { key: NONE_LANE_KEY, name: '未关联里程碑', milestone: null, tasks: [] };
  for (const t of tasks) {
    const lane = t.milestone_id ? byMilestone.get(t.milestone_id) ?? none : none;
    lane.tasks.push(t);
  }
  if (none.tasks.length > 0) lanes.push(none);
  return lanes.filter(l => l.tasks.length > 0);
}

export function TaskDag({
  tasks,
  milestones,
  agents,
  selectedTaskId,
  onOpenTask,
  dir = 'tb',
  onToggleDir,
}: {
  tasks: Task[];
  milestones: Milestone[];
  agents: Agent[];
  selectedTaskId: string | null;
  onOpenTask: (id: string) => void;
  /** 布局方向：tb=自上而下（默认）/ lr=自左向右 */
  dir?: 'tb' | 'lr';
  onToggleDir?: () => void;
}) {
  const agentName = useMemo(() => {
    const m = new Map(agents.map(a => [a.id, a.name]));
    return (id: string | null) => (id ? m.get(id) ?? '未指派' : '未指派');
  }, [agents]);

  const layout = useMemo(() => {
    const layer = computeLayers(tasks);
    const lanes = buildLanes(tasks, milestones);
    const order = new Map(tasks.map((t, i) => [t.id, i]));
    for (const lane of lanes) {
      lane.tasks.sort((a, b) => {
        const la = layer.get(a.id) ?? 0;
        const lb = layer.get(b.id) ?? 0;
        if (la !== lb) return la - lb;
        return (order.get(a.id) ?? 0) - (order.get(b.id) ?? 0);
      });
    }

    const nodePos = new Map<string, NodePos>();
    let maxLayer = 0;
    for (const v of layer.values()) maxLayer = Math.max(maxLayer, v);

    const edges: { id: string; d: string; fromId: string; toId: string }[] = [];
    let totalWidth: number;
    let totalHeight: number;

    if (dir === 'tb') {
      // 纵向：层号 → Y（自上而下）；里程碑泳道 → 纵列（X），列内同层节点横向并排
      const placedTB: { lane: Lane; x: number; width: number; color: string }[] = [];
      let x = PAD;
      lanes.forEach((lane, laneIndex) => {
        const perLayer = new Map<number, number>();
        const assignments: { task: Task; layer: number; row: number }[] = [];
        let rows = 1;
        for (const t of lane.tasks) {
          const lv = layer.get(t.id) ?? 0;
          const row = perLayer.get(lv) ?? 0;
          perLayer.set(lv, row + 1);
          rows = Math.max(rows, row + 1);
          assignments.push({ task: t, layer: lv, row });
        }
        const width = LANE_PAD_TOP + rows * NODE_W + (rows - 1) * GAP_X + LANE_PAD_BOTTOM;
        placedTB.push({ lane, x, width, color: LANE_COLORS[laneIndex % LANE_COLORS.length] });
        for (const a of assignments) {
          nodePos.set(a.task.id, {
            task: a.task,
            x: x + LANE_PAD_TOP + a.row * (NODE_W + GAP_X),
            y: PAD + LANE_LABEL_H + a.layer * (NODE_H + GAP_Y),
            laneKey: lane.key,
          });
        }
        x += width + LANE_GAP;
      });
      for (const t of tasks) {
        const to = nodePos.get(t.id);
        if (!to) continue;
        for (const dep of t.depends_on) {
          const from = nodePos.get(dep);
          if (!from) continue;
          // 自上而下：从依赖节点底部出发，进入本节点顶部
          const x1 = from.x + NODE_W / 2;
          const y1 = from.y + NODE_H;
          const x2 = to.x + NODE_W / 2;
          const y2 = to.y;
          const dy = Math.max(24, (y2 - y1) * 0.45);
          edges.push({
            id: `${dep}->${t.id}`,
            fromId: dep,
            toId: t.id,
            d: `M ${x1} ${y1} C ${x1} ${y1 + dy}, ${x2} ${y2 - dy}, ${x2} ${y2}`,
          });
        }
      }
      totalWidth = Math.max(x, 200);
      totalHeight = PAD * 2 + LANE_LABEL_H + (maxLayer + 1) * NODE_H + maxLayer * GAP_Y + 60;
      return { nodes: [...nodePos.values()], edges, placedTB, placedLR: [], dir, totalWidth, totalHeight };
    }

    // 横向：层号 → X（自左向右）；里程碑泳道 → 横带（Y），带内同层节点纵向并排
    const placedLR: { lane: Lane; y: number; height: number; color: string }[] = [];
    let y0 = PAD;
    lanes.forEach((lane, laneIndex) => {
      const perLayer = new Map<number, number>();
      const assignments: { task: Task; layer: number; row: number }[] = [];
      let rows = 1;
      for (const t of lane.tasks) {
        const lv = layer.get(t.id) ?? 0;
        const row = perLayer.get(lv) ?? 0;
        perLayer.set(lv, row + 1);
        rows = Math.max(rows, row + 1);
        assignments.push({ task: t, layer: lv, row });
      }
      const height = LANE_LABEL_H + LANE_PAD_TOP + rows * NODE_H + (rows - 1) * GAP_Y + LANE_PAD_BOTTOM;
      placedLR.push({ lane, y: y0, height, color: LANE_COLORS[laneIndex % LANE_COLORS.length] });
      for (const a of assignments) {
        nodePos.set(a.task.id, {
          task: a.task,
          x: PAD + a.layer * (NODE_W + GAP_X),
          y: y0 + LANE_LABEL_H + LANE_PAD_TOP + a.row * (NODE_H + GAP_Y),
          laneKey: lane.key,
        });
      }
      y0 += height + LANE_GAP;
    });
    for (const t of tasks) {
      const to = nodePos.get(t.id);
      if (!to) continue;
      for (const dep of t.depends_on) {
        const from = nodePos.get(dep);
        if (!from) continue;
        // 自左向右：从依赖节点右侧出发，进入本节点左侧
        const x1 = from.x + NODE_W;
        const y1 = from.y + NODE_H / 2;
        const x2 = to.x;
        const y2 = to.y + NODE_H / 2;
        const dx = Math.max(28, (x2 - x1) * 0.45);
        edges.push({
          id: `${dep}->${t.id}`,
          fromId: dep,
          toId: t.id,
          d: `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`,
        });
      }
    }
    totalWidth = PAD * 2 + (maxLayer + 1) * NODE_W + maxLayer * GAP_X + 60;
    totalHeight = Math.max(y0, 160);
    return { nodes: [...nodePos.values()], edges, placedTB: [], placedLR, dir, totalWidth, totalHeight };
  }, [tasks, milestones, dir]);

  if (tasks.length === 0) {
    return <div className="empty">该项目还没有任务，无法绘制 DAG。</div>;
  }

  return (
    <div className="dag-wrap">
      <div className="dag-legend">
        {Object.entries(TASK_STATUS_COLOR).map(([status, color]) => (
          <span key={status} className="dag-legend-item" title={status}>
            <span className="dag-legend-dot" style={{ background: color }} />
            {TASK_STATUS_LABEL[status]?.label ?? status}
          </span>
        ))}
        <span className="dag-legend-sep" />
        <span className="muted">
          {tasks.length} 节点 · {layout.edges.length} 条依赖边
        </span>
        {onToggleDir && (
          <button className="btn btn-sm" style={{ marginLeft: 'auto' }} onClick={onToggleDir}>
            {dir === 'tb' ? '⇄ 改为左右布局' : '⇅ 改为上下布局'}
          </button>
        )}
      </div>

      <div className="dag-scroll">
        <div
          className="dag-canvas"
          style={{ width: layout.totalWidth, height: layout.totalHeight }}
        >
          {(layout.dir === 'tb' ? layout.placedTB : layout.placedLR).map(p => {
            const total = p.lane.tasks.length;
            const done = p.lane.tasks.filter(t => t.status === 'done').length;
            const status = p.lane.milestone?.status;
            const meta = status ? MILESTONE_STATUS_LABEL[status] : null;
            return (
              <div
                key={p.lane.key}
                className="dag-lane"
                style={
                  layout.dir === 'tb'
                    ? {
                        left: (p as { x: number }).x,
                        width: (p as { width: number }).width,
                        top: 8,
                        bottom: 8,
                        borderColor: `${p.color}55`,
                        background: `${p.color}10`,
                      }
                    : {
                        top: (p as { y: number }).y,
                        height: (p as { height: number }).height,
                        left: 8,
                        right: 8,
                        borderColor: `${p.color}55`,
                        background: `${p.color}10`,
                      }
                }
              >
                <div className="dag-lane-label" title={`${p.lane.name} · ${done}/${total} 完成${meta ? ` · ${meta.label}` : ''}`}>
                  <span className="dag-lane-dot" style={{ background: p.color }} />
                  <span className="dag-lane-name">{p.lane.name}</span>
                  {meta && <span className={`dag-lane-status badge badge-${meta.tone}`}>{meta.label}</span>}
                  <span className="muted">
                    {done}/{total} 完成
                  </span>
                </div>
              </div>
            );
          })}

          <svg
            className="dag-edges"
            width={layout.totalWidth}
            height={layout.totalHeight}
            aria-hidden
          >
            <defs>
              <marker
                id="dag-arrow"
                viewBox="0 0 10 10"
                refX="9"
                refY="5"
                markerWidth="7"
                markerHeight="7"
                orient="auto-start-reverse"
              >
                <path d="M 0 0 L 10 5 L 0 10 z" fill="#4a566a" />
              </marker>
            </defs>
            {layout.edges.map(e => (
              <path
                key={e.id}
                className={`dag-edge${
                  selectedTaskId && (e.fromId === selectedTaskId || e.toId === selectedTaskId)
                    ? ' active'
                    : ''
                }`}
                d={e.d}
                markerEnd="url(#dag-arrow)"
              />
            ))}
          </svg>

          {layout.nodes.map(n => {
            const t = n.task;
            const color = TASK_STATUS_COLOR[t.status] ?? '#8a94a4';
            return (
              <button
                key={t.id}
                type="button"
                className={`dag-node${t.id === selectedTaskId ? ' selected' : ''}`}
                style={{ left: n.x, top: n.y, width: NODE_W, height: NODE_H, borderColor: color }}
                onClick={() => onOpenTask(t.id)}
                title={`执行者：${agentName(t.executor_agent_id)} · ${t.id}`}
              >
                <span className="dag-node-stripe" style={{ background: color }} />
                <span className="dag-node-title">{t.title}</span>
                {/* 缩略单行：状态 · 依赖 · 产物（执行者名移至 hover） */}
                <span className="dag-node-compact">
                  <span className="dag-node-dot" style={{ background: color }} />
                  <span className="dag-node-status">{TASK_STATUS_LABEL[t.status]?.label ?? t.status}</span>
                  <span className="muted">· {t.depends_on.length > 0 ? `依赖${t.depends_on.length}` : '无依赖'}</span>
                  <span className="muted">· 产物{t.artifacts.length}</span>
                </span>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
