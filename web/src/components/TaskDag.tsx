import { useMemo, useRef, useState } from "react";
import type { Task } from "../types";
import {
  Badge,
  Empty,
  TaskStateBadge,
  taskStageLabel,
} from "../ui";
import {
  DAG_NODE_HEIGHT,
  DAG_NODE_WIDTH,
  layoutTaskDag,
} from "./dagLayout";

const MIN_ZOOM = 0.4;
const MAX_ZOOM = 1.6;
const ZOOM_STEP = 0.1;
/// 滚动容器里留给缩放工具条的纵向空间，避免工具条压住层级列头。
const TOOLBAR_BAND = 44;

/// 任务 DAG 只读取任务的 deps、parent_id 与展示字段；选中任务通过回调整理，
/// 图本身不触发任何任务状态变更。
/// 列间通道折线由 dagLayout 生成；这里只负责高亮、列头与滚动容器内的缩放。
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
  const scrollRef = useRef<HTMLDivElement>(null);
  const [hoveredTaskId, setHoveredTaskId] = useState<string | null>(null);
  const [zoom, setZoom] = useState(1);

  if (layout.nodes.length === 0) {
    return (
      <Empty
        title="没有可展示的任务"
        hint="已取消的任务不进入依赖图；添加或规划任务后可在这里查看 DAG。"
      />
    );
  }

  const activeTaskId = hoveredTaskId ?? selectedTaskId;
  const relatedEdgeIds = new Set<string>();
  const relatedNodeIds = new Set<string>();
  if (activeTaskId) {
    relatedNodeIds.add(activeTaskId);
    for (const edge of layout.edges) {
      if (edge.from === activeTaskId || edge.to === activeTaskId) {
        relatedEdgeIds.add(edge.id);
        relatedNodeIds.add(edge.from);
        relatedNodeIds.add(edge.to);
      }
    }
  }
  const highlightActive = activeTaskId !== null;

  const missing = layout.missingDependencies
    .filter(
      (item, index, all) =>
        all.findIndex(
          (candidate) =>
            candidate.taskKey === item.taskKey && candidate.dep === item.dep,
        ) === index,
    )
    .map((item) => `${item.taskKey} → ${item.dep}`);

  const zoomBy = (delta: number) => {
    setZoom((current) => clampZoom(current + delta));
  };

  const fitToWindow = () => {
    const element = scrollRef.current;
    if (!element || layout.width <= 0 || layout.height <= 0) return;
    const horizontal = (element.clientWidth - 32) / layout.width;
    const vertical = (element.clientHeight - TOOLBAR_BAND - 16) / layout.height;
    setZoom(clampZoom(Math.min(1, horizontal, vertical)));
  };

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
        依赖图内容较宽，可在图内横向或纵向滚动；聚焦图表后可用方向键滚动，左上角可放大、缩小或适应窗口。
      </div>
      <div
        ref={scrollRef}
        className="dag-scroll"
        role="region"
        aria-label="任务依赖图"
        aria-describedby="dag-scroll-hint"
        tabIndex={0}
      >
        <div
          className="dag-zoom-space"
          style={{
            width: layout.width * zoom,
            height: layout.height * zoom + TOOLBAR_BAND,
          }}
        >
          <div className="dag-toolbar" role="toolbar" aria-label="依赖图缩放">
            <button
              type="button"
              className="dag-zoom-btn"
              aria-label="缩小"
              title="缩小"
              onClick={() => zoomBy(-ZOOM_STEP)}
              disabled={zoom <= MIN_ZOOM + 0.001}
            >
              <span aria-hidden="true">−</span>
            </button>
            <span className="dag-zoom-level" aria-live="polite">
              {Math.round(zoom * 100)}%
            </span>
            <button
              type="button"
              className="dag-zoom-btn"
              aria-label="放大"
              title="放大"
              onClick={() => zoomBy(ZOOM_STEP)}
              disabled={zoom >= MAX_ZOOM - 0.001}
            >
              <span aria-hidden="true">+</span>
            </button>
            <button
              type="button"
              className="dag-zoom-btn dag-fit-btn"
              aria-label="适应窗口"
              title="适应窗口"
              onClick={fitToWindow}
            >
              适应窗口
            </button>
          </div>
          <div
            className="dag-canvas"
            style={{
              top: TOOLBAR_BAND,
              width: layout.width,
              height: layout.height,
              transform: `scale(${zoom})`,
              transformOrigin: "top left",
            }}
          >
            {layout.levels.map((level) => (
              <div
                key={level.level}
                className="dag-level-header"
                style={{ left: level.x, width: level.width }}
                title={`${level.label} · ${level.count} 个任务`}
              >
                <span className="dag-level-label">{level.label}</span>
                <span className="dag-level-count">{level.count}</span>
              </div>
            ))}
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
              {layout.edges.map((edge) => {
                const related = relatedEdgeIds.has(edge.id);
                const classes = ["dag-edge"];
                classes.push(
                  edge.kind === "dependency"
                    ? "dag-edge-dependency"
                    : "dag-edge-derived",
                );
                if (highlightActive) {
                  classes.push(related ? "dag-edge-highlight" : "dag-edge-dimmed");
                }
                return (
                  <path
                    key={edge.id}
                    d={edge.path}
                    className={classes.join(" ")}
                    markerEnd={`url(#dag-arrow-${edge.kind})`}
                  />
                );
              })}
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
              const related = highlightActive && relatedNodeIds.has(task.id);
              const classes = ["dag-node"];
              if (selected) classes.push("selected");
              if (related && !selected) classes.push("dag-node-related");
              if (highlightActive && !related) classes.push("dag-node-dimmed");
              return (
                <div
                  key={task.id}
                  className={classes.join(" ")}
                  style={{ left: x, top: y, width: DAG_NODE_WIDTH, height: DAG_NODE_HEIGHT }}
                  role="button"
                  tabIndex={0}
                  aria-label={`打开任务 ${task.key} ${task.title}`}
                  onClick={() => onSelect(task.id)}
                  onMouseEnter={() => setHoveredTaskId(task.id)}
                  onMouseLeave={() =>
                    setHoveredTaskId((current) => (current === task.id ? null : current))
                  }
                  onFocus={() => setHoveredTaskId(task.id)}
                  onBlur={() =>
                    setHoveredTaskId((current) => (current === task.id ? null : current))
                  }
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
                    {task.status && task.stage !== "completed" ? <Badge tone="muted">{taskStageLabel(task.stage)}</Badge> : null}
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
        <span>按依赖层级从左到右排列，同一层任务并列；跨层依赖经列间通道折线连接，不穿过任务卡片；已取消任务不参与依赖图。</span>
      </div>
    </div>
  );
}

function clampZoom(value: number): number {
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, Math.round(value * 100) / 100));
}
