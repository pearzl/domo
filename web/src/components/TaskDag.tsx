import { useMemo } from "react";
import type { Task } from "../types";
import {
  Badge,
  Empty,
  TASK_STAGE_LABELS,
  TaskStateBadge,
} from "../ui";
import {
  DAG_NODE_HEIGHT,
  DAG_NODE_WIDTH,
  layoutTaskDag,
} from "./dagLayout";

/// 任务 DAG 只读取任务的 deps、parent_id 与展示字段；选中任务通过回调整理，
/// 图本身不触发任何任务状态变更。
export function TaskDag({
  tasks,
  selectedTaskId,
  onSelect,
}: {
  tasks: Task[];
  selectedTaskId: string | null;
  onSelect: (taskId: string) => void;
}) {
  const layout = useMemo(() => layoutTaskDag(tasks), [tasks]);
  const taskById = useMemo(
    () => new Map(tasks.map((task) => [task.id, task])),
    [tasks],
  );

  if (layout.nodes.length === 0) {
    return (
      <Empty
        title="没有可展示的任务"
        hint="已取消的任务不进入依赖图；添加或规划任务后可在这里查看 DAG。"
      />
    );
  }

  const missing = layout.missingDependencies
    .filter(
      (item, index, all) =>
        all.findIndex(
          (candidate) =>
            candidate.taskKey === item.taskKey && candidate.dep === item.dep,
        ) === index,
    )
    .map((item) => `${item.taskKey} → ${item.dep}`);

  return (
    <div className="dag-view">
      {layout.cycleKeys.length > 0 ? (
        <div className="warn-banner">
          检测到循环依赖或受其阻塞的任务：{layout.cycleKeys.join("、")}。
          这些任务已放在最右侧一列展示，不影响任务数据与调度。
        </div>
      ) : null}
      {missing.length > 0 ? (
        <div className="muted dag-note">
          已忽略指向不存在或已取消任务的依赖：{missing.join("、")}
        </div>
      ) : null}
      <div className="dag-scroll-hint" id="dag-scroll-hint">
        <span className="dag-scroll-hint-icon" aria-hidden="true">↔</span>
        依赖图内容较宽，可在图内横向或纵向滚动；聚焦图表后可用方向键滚动。
      </div>
      <div
        className="dag-scroll"
        role="region"
        aria-label="任务依赖图"
        aria-describedby="dag-scroll-hint"
        tabIndex={0}
      >
        <div
          className="dag-canvas"
          style={{ width: layout.width, height: layout.height }}
        >
          <svg
            className="dag-edges"
            width={layout.width}
            height={layout.height}
            aria-hidden="true"
            focusable="false"
          >
            <defs>
              <marker
                id="dag-arrow-dependency"
                viewBox="0 0 8 8"
                markerWidth="8"
                markerHeight="8"
                refX="7"
                refY="4"
                orient="auto"
              >
                <path d="M 0 0 L 8 4 L 0 8 z" fill="#9aa7b8" />
              </marker>
              <marker
                id="dag-arrow-derived"
                viewBox="0 0 8 8"
                markerWidth="8"
                markerHeight="8"
                refX="7"
                refY="4"
                orient="auto"
              >
                <path d="M 0 0 L 8 4 L 0 8 z" fill="#b07a2a" />
              </marker>
            </defs>
            {layout.edges.map((edge) => (
              <path
                key={edge.id}
                d={edge.path}
                className={
                  edge.kind === "dependency"
                    ? "dag-edge dag-edge-dependency"
                    : "dag-edge dag-edge-derived"
                }
                markerEnd={`url(#dag-arrow-${edge.kind})`}
              />
            ))}
          </svg>
          {layout.nodes.map(({ task, x, y }) => {
            const parent = task.parent_id
              ? taskById.get(task.parent_id) ?? null
              : null;
            const deps = task.deps.join(", ");
            const meta = [
              deps ? `依赖 ${deps}` : "无依赖",
              parent ? `派生自 ${parent.key}` : "",
            ]
              .filter(Boolean)
              .join(" · ");
            const selected = task.id === selectedTaskId;
            return (
              <div
                key={task.id}
                className={`dag-node${selected ? " selected" : ""}`}
                style={{ left: x, top: y, width: DAG_NODE_WIDTH, height: DAG_NODE_HEIGHT }}
                role="button"
                tabIndex={0}
                aria-label={`打开任务 ${task.key} ${task.title}`}
                onClick={() => onSelect(task.id)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onSelect(task.id);
                  }
                }}
              >
                <div className="dag-node-head">
                  <span className="dag-node-key">{task.key}</span>
                  <span className="dag-node-flags">
                    {parent ? <Badge tone="info">派生</Badge> : null}
                    {task.milestone ? <Badge tone="warn">◆ 里程碑</Badge> : null}
                  </span>
                </div>
                <div className="dag-node-title" title={task.title}>
                  {task.title}
                </div>
                <div className="dag-node-meta">
                  <Badge tone="muted">
                    {TASK_STAGE_LABELS[task.stage] ?? task.stage}
                  </Badge>
                  <TaskStateBadge stage={task.stage} status={task.status} />
                </div>
                <div className="dag-node-deps" title={meta}>
                  {meta}
                </div>
              </div>
            );
          })}
        </div>
      </div>
      <div className="dag-legend muted">
        <span className="dag-legend-item">
          <span className="dag-legend-line dag-legend-dependency" />
          依赖（前置 → 后继）
        </span>
        <span className="dag-legend-item">
          <span className="dag-legend-line dag-legend-derived" />
          派生（父 → 子）
        </span>
        <span>按依赖层级从左到右排列，同一层任务并列；已取消任务不参与依赖图。</span>
      </div>
    </div>
  );
}
