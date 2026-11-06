import { Fragment, useEffect, useId, useMemo, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import type { Task } from "../types";
import { isTaskRunning } from "../presentation/taskList";
import { taskMotionStyle, useTaskCompletion } from "./useTaskCompletion";
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

const MIN_ZOOM = 0.25;
const MAX_ZOOM = 2;
const ZOOM_STEP = 0.1;
/// 滚动容器里留给缩放工具条的纵向空间，避免工具条压住层级列头。
const TOOLBAR_BAND = 44;

interface Viewport {
  x: number;
  y: number;
  zoom: number;
}

function zoomAt(current: Viewport, zoom: number, x: number, y: number): Viewport {
  const next = clampZoom(zoom);
  const ratio = next / current.zoom;
  return { x: x - (x - current.x) * ratio, y: y - (y - current.y) * ratio, zoom: next };
}

/// 任务 DAG 只读取任务的 deps、parent_id 与展示字段；选中任务通过回调整理，
/// 图本身不触发任何任务状态变更。
/// 列间通道折线由 dagLayout 生成；视口平移与缩放独立于任务布局坐标。
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
  const markerId = useId().replace(/:/g, "");
  const completed = useTaskCompletion(tasks);
  const [hoveredTaskId, setHoveredTaskId] = useState<string | null>(null);
  const [view, setView] = useState<Viewport>({ x: 0, y: 0, zoom: 1 });
  const [dragging, setDragging] = useState(false);
  const drag = useRef<{ id: number; x: number; y: number; view: Viewport; moved: boolean } | null>(null);
  const suppressClick = useRef(false);
  const hasNodes = layout.nodes.length > 0;

  useEffect(() => {
    const element = scrollRef.current;
    if (!element) return;
    const wheel = (event: WheelEvent) => {
      event.preventDefault();
      const rect = element.getBoundingClientRect();
      const delta = event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? element.clientHeight : 1);
      const factor = Math.exp(-Math.max(-200, Math.min(200, delta)) * 0.002);
      setView(current => zoomAt(current, current.zoom * factor, event.clientX - rect.left - element.clientLeft, event.clientY - rect.top - element.clientTop - TOOLBAR_BAND));
    };
    element.addEventListener("wheel", wheel, { passive: false });
    return () => element.removeEventListener("wheel", wheel);
  }, [hasNodes]);

  const finishDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (drag.current?.id !== event.pointerId) return;
    drag.current = null;
    setDragging(false);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
  };

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
    const element = scrollRef.current;
    if (!element) return;
    setView(current => zoomAt(current, current.zoom + delta, element.clientWidth / 2, (element.clientHeight - TOOLBAR_BAND) / 2));
  };

  const fitToWindow = () => {
    const element = scrollRef.current;
    if (!element || layout.width <= 0 || layout.height <= 0) return;
    const horizontal = (element.clientWidth - 32) / layout.width;
    const vertical = (element.clientHeight - TOOLBAR_BAND - 16) / layout.height;
    const zoom = clampZoom(Math.min(1, horizontal, vertical));
    setView({ zoom, x: (element.clientWidth - layout.width * zoom) / 2, y: (element.clientHeight - TOOLBAR_BAND - layout.height * zoom) / 2 });
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
      <div className="dag-scroll-hint" id={`${markerId}-hint`}>
        <span className="dag-scroll-hint-icon" aria-hidden="true">↔</span>
        滚轮缩放 · 拖动画布 · 方向键平移 · Home 适应窗口
      </div>
      <div
        ref={scrollRef}
        className="dag-scroll"
        data-dragging={dragging}
        role="region"
        aria-label="任务依赖图"
        aria-describedby={`${markerId}-hint`}
        tabIndex={0}
        onPointerDown={event => {
          if (event.button !== 0 || !event.isPrimary || (event.target as Element).closest(".dag-toolbar")) return;
          suppressClick.current = false;
          drag.current = { id: event.pointerId, x: event.clientX, y: event.clientY, view, moved: false };
          if (!(event.target as Element).closest(".dag-node")) event.currentTarget.focus({ preventScroll: true });
        }}
        onPointerMove={event => {
          const start = drag.current;
          if (!start || start.id !== event.pointerId) return;
          const dx = event.clientX - start.x;
          const dy = event.clientY - start.y;
          if (!start.moved && Math.hypot(dx, dy) < 5) return;
          if (!start.moved) {
            start.moved = true;
            suppressClick.current = true;
            setDragging(true);
            setHoveredTaskId(null);
            event.currentTarget.setPointerCapture(event.pointerId);
          }
          setView({ ...start.view, x: start.view.x + dx, y: start.view.y + dy });
        }}
        onPointerUp={finishDrag}
        onPointerCancel={finishDrag}
        onLostPointerCapture={finishDrag}
        onClickCapture={event => {
          if (!suppressClick.current) return;
          event.preventDefault();
          event.stopPropagation();
          suppressClick.current = false;
        }}
        onKeyDown={event => {
          if (event.target !== event.currentTarget) return;
          const directions: Record<string, [number, number]> = { ArrowLeft: [40, 0], ArrowRight: [-40, 0], ArrowUp: [0, 40], ArrowDown: [0, -40] };
          const delta = directions[event.key];
          if (delta) {
            event.preventDefault();
            setView(current => ({ ...current, x: current.x + delta[0], y: current.y + delta[1] }));
          } else if (event.key === "+" || event.key === "=" || event.key === "-") {
            event.preventDefault();
            zoomBy(event.key === "-" ? -ZOOM_STEP : ZOOM_STEP);
          } else if (event.key === "Home") {
            event.preventDefault();
            fitToWindow();
          }
        }}
      >
        <div
          className="dag-zoom-space"
        >
          <div className="dag-toolbar" role="toolbar" aria-label="依赖图缩放">
            <button
              type="button"
              className="dag-zoom-btn"
              aria-label="缩小"
              title="缩小"
              onClick={() => zoomBy(-ZOOM_STEP)}
              disabled={view.zoom <= MIN_ZOOM + 0.001}
            >
              <span aria-hidden="true">−</span>
            </button>
            <span className="dag-zoom-level" aria-live="polite">
              {Math.round(view.zoom * 100)}%
            </span>
            <button
              type="button"
              className="dag-zoom-btn"
              aria-label="放大"
              title="放大"
              onClick={() => zoomBy(ZOOM_STEP)}
              disabled={view.zoom >= MAX_ZOOM - 0.001}
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
              transform: `translate(${view.x}px, ${view.y}px) scale(${view.zoom})`,
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
                  id={`${markerId}-dependency`}
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
                  id={`${markerId}-derived`}
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
                const source = taskById.get(edge.from);
                const target = taskById.get(edge.to);
                const running = edge.kind === "dependency" && source?.stage === "completed" && target && isTaskRunning(target);
                const classes = ["dag-edge"];
                classes.push(
                  edge.kind === "dependency"
                    ? "dag-edge-dependency"
                    : "dag-edge-derived",
                );
                if (highlightActive) {
                  classes.push(related ? "dag-edge-highlight" : "dag-edge-dimmed");
                }
                if (running) classes.push("dag-edge-running");
                return (
                  <Fragment key={edge.id}>
                  <path
                    d={edge.path}
                    className={classes.join(" ")}
                    markerEnd={`url(#${markerId}-${edge.kind})`}
                  />
                  {running ? <path d={edge.path} className={`dag-edge-flow${highlightActive && !related ? " dag-edge-dimmed" : ""}`} pathLength={100} style={taskMotionStyle(edge.to)} /> : null}
                  </Fragment>
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
                  data-running={isTaskRunning(task)}
                  data-completed={completed.has(task.id)}
                  data-stage={task.stage}
                  style={{ ...taskMotionStyle(task.id), left: x, top: y, width: DAG_NODE_WIDTH, height: DAG_NODE_HEIGHT }}
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
                    {isTaskRunning(task) ? <span className="runtime-dot" aria-hidden="true" /> : null}
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
        <span className="dag-legend-item"><span className="dag-legend-line dag-legend-running" />运行路径</span>
        <span>任务按依赖层级排列，连线沿直角通道避让任务卡片；已取消任务不参与依赖图。</span>
      </div>
    </div>
  );
}

function clampZoom(value: number): number {
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, Math.round(value * 100) / 100));
}
