/**
 * 任务面板的呈现骨架：面板头部操作区、添加任务表单与任务列表三层结构。
 *
 * 约束：
 * - 操作区（视图切换、添加任务按钮）与列表之间保留固定间距，按钮不贴面板边框；
 * - 添加任务表单作为面板内的下拉卡片，表单容器与列表容器是各自独立、互不嵌套的兄弟节点，
 *   并且有进入过渡动画（尊重减少动效偏好）；
 * - 任务行首行是 key、标题与状态徽标，第二行才是依赖、Agent 与计数，
 *   窄屏下状态徽标仍在首屏；
 * - 本文件只决定布局、类名与显示顺序，不改变任务筛选与队列操作逻辑，
 *   行内的重排按钮仍回调页面传入的处理函数。
 */
import type { ReactNode } from "react";
import type { Task } from "../types";
import {
  queueSummary,
  type QueuePositionSummary,
} from "../presentation/taskList";
import { isReordableQueueTask } from "../taskQueue";
import { Badge, TaskStateBadge, taskStateLabel } from "../ui";

/// 任务面板头部的操作区，位于面板标题右侧。
export function TaskPanelActions({ children }: { children: ReactNode }) {
  return <div className="task-panel-actions">{children}</div>;
}

/// 任务面板：头部（标题 + 操作区）、工具栏、添加任务表单、列表依次排布。
/// 表单与列表是同一父节点下的兄弟节点，二者之间靠外边距与各自边框分隔。
export function TaskPanel({
  title,
  actions,
  toolbar,
  leading,
  form,
  list,
  footer,
}: {
  title: ReactNode;
  actions?: ReactNode;
  /// 搜索与筛选工具栏；筛选逻辑仍在页面内，这里只负责位置。
  toolbar?: ReactNode;
  /// 直接完成模式等前置说明区块。
  leading?: ReactNode;
  /// 添加任务表单卡片，未打开时为 null。
  form?: ReactNode;
  /// 任务列表、DAG 或空状态。
  list: ReactNode;
  /// 列表下方的图例或说明。
  footer?: ReactNode;
}) {
  return (
    <section className="panel task-panel">
      <div className="panel-head">
        {title}
        {actions}
      </div>
      {leading}
      {toolbar}
      {form}
      {list}
      {footer}
    </section>
  );
}

/// 添加任务表单容器：与任务列表平级的独立卡片，带进入过渡。
export function TaskFormCard({ children }: { children: ReactNode }) {
  return <div className="add-task-card">{children}</div>;
}

/// 任务列表容器：行内边框由行自身绘制，容器不再与按钮共享边界。
export function TaskListShell({ children }: { children: ReactNode }) {
  return <div className="task-list">{children}</div>;
}

/// 依赖摘要：只列出依赖 key；派生、子任务与等待原因由 TaskRow 追加在第二行。
export function depsText(task: Task): string {
  return task.deps.length ? `依赖 ${task.deps.join(", ")}` : "无依赖";
}

/// 任务行：首行 key + 标题 + 状态徽标，第二行依赖、Agent 与计数。
export function TaskRow({
  task,
  selected,
  agent,
  counts,
  deps,
  summary,
  onSelect,
  children,
}: {
  task: Task;
  selected: boolean;
  agent: string;
  counts: string;
  /// 完整依赖说明；省略时使用任务自身的依赖 key。
  deps?: ReactNode;
  /// 计数区的说明文案。
  summary?: string;
  onSelect: () => void;
  /// 队列重排控件，保持在计数区内。
  children?: ReactNode;
}) {
  return (
    <div
      className={`task-row ${selected ? "selected" : ""}`}
      role="button"
      tabIndex={0}
      aria-label={`${task.key} ${task.title}，${taskStateLabel(task.stage, task.status)}`}
      onClick={onSelect}
      onKeyDown={(event) => {
        if (event.key === " " || event.key === "Enter") {
          // Space 默认会滚动页面，打开抽屉时阻止滚动。
          event.preventDefault();
          onSelect();
        }
      }}
    >
      <div className="task-row-main">
        <span className="task-key">{task.key}</span>
        <span className="task-title">{task.title}</span>
        <span className="task-state">
          <TaskStateBadge stage={task.stage} status={task.status} />
          {task.queue_position !== null ? (
            <Badge tone="info">队列 #{task.queue_position}</Badge>
          ) : null}
          {task.milestone ? <Badge tone="warn">里程碑</Badge> : null}
        </span>
      </div>
      <div className="task-row-meta">
        <span className="task-deps muted" title={task.waiting_reason ?? ""}>
          {deps ?? depsText(task)}
        </span>
        <span className="task-agent muted" title={agent}>
          <span className="task-agent-label">Agent </span>
          {agent}
        </span>
        <span className="task-counts muted" title={summary}>
          {counts}
          {children}
        </span>
      </div>
    </div>
  );
}

/// 任务行的可调整队列控件：展示位置摘要，按钮使用明确文案，首尾按 queueEdge 禁用。
export function QueueControls({
  task,
  summary,
  busy,
  onReorder,
}: {
  task: Task;
  summary: QueuePositionSummary;
  busy: boolean;
  onReorder: (direction: "up" | "down") => void;
}) {
  return (
    <span
      className="queue-controls"
      title={`可调整队列共 ${summary.total} 个任务；上移/下移只改变派发顺序，不改变依赖关系`}
    >
      <span className="queue-position">{summary.label}</span>
      <button
        className="ghost"
        type="button"
        title={summary.canMoveUp ? "上移一位" : "已经在可调整队列首位"}
        aria-label={`${task.key} 排队上移`}
        aria-disabled={!summary.canMoveUp}
        disabled={busy || !summary.canMoveUp}
        onClick={(event) => {
          event.stopPropagation();
          onReorder("up");
        }}
      >
        上移
      </button>
      <button
        className="ghost"
        type="button"
        title={summary.canMoveDown ? "下移一位" : "已经在可调整队列末位"}
        aria-label={`${task.key} 排队下移`}
        aria-disabled={!summary.canMoveDown}
        disabled={busy || !summary.canMoveDown}
        onClick={(event) => {
          event.stopPropagation();
          onReorder("down");
        }}
      >
        下移
      </button>
    </span>
  );
}

/// 任务列表视图：逐行渲染任务，行内直接给出队列位置与重排入口。
export function TaskList({
  tasks,
  activeTasks,
  selectedTaskId,
  onSelect,
  agentName,
  queueIds,
  archived,
  busy,
  onReorder,
}: {
  tasks: Task[];
  /// 全部活动任务，用于统计子任务完成数。
  activeTasks: Task[];
  selectedTaskId: string | null;
  onSelect: (taskId: string) => void;
  agentName: (agentId: string | null) => string;
  queueIds: string[];
  archived: boolean;
  busy: boolean;
  onReorder: (taskId: string, direction: "up" | "down") => void;
}) {
  return (
    <TaskListShell>
      {tasks.map((task) => {
        const children = activeTasks.filter(
          (item) => item.parent_id === task.id,
        );
        const childCompleted = children.filter(
          (item) => item.stage === "completed",
        ).length;
        const waitingChildren =
          children.length > 0 &&
          task.stage === "pending" &&
          childCompleted < children.length;
        const counts = [
          task.attempts ? `执行 ${task.attempts}` : "",
          task.rejections ? `拒绝 ${task.rejections}` : "",
        ]
          .filter(Boolean)
          .join(" ");
        const deps = [
          depsText(task),
          children.length ? `子任务 ${childCompleted}/${children.length}` : "",
          waitingChildren ? "等待子任务" : "",
          task.parent_id ? "派生任务" : "",
          task.waiting_reason ?? "",
        ]
          .filter(Boolean)
          .join(" · ");
        return (
          <TaskRow
            key={task.id}
            task={task}
            selected={task.id === selectedTaskId}
            agent={agentName(task.agent_id)}
            counts={counts}
            deps={deps}
            summary={counts || "无执行或拒绝记录"}
            onSelect={() => onSelect(task.id)}
          >
            {isReordableQueueTask(task, archived) ? (
              <QueueControls
                task={task}
                summary={queueSummary(queueIds, task.id)}
                busy={busy}
                onReorder={(direction) => onReorder(task.id, direction)}
              />
            ) : null}
          </TaskRow>
        );
      })}
    </TaskListShell>
  );
}
