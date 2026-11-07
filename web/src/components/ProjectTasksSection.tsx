import type { JobSummary, Task } from "../types";
import { EmptySteps } from "../ui";
import { TaskDag } from "./TaskDag";
import {
  TaskList,
  TaskPanel,
  TaskPanelActions,
} from "./TaskBoard";
import { TASK_FILTERS, type TaskFilter } from "../presentation/taskList";

/// 任务选项卡：视图切换、搜索筛选、任务列表或 DAG。
export function ProjectTasksSection({
  tasks,
  jobs,
  activeTasks,
  visibleTasks,
  archived,
  busy,
  taskView,
  onTaskViewChange,
  taskQuery,
  onTaskQueryChange,
  taskFilter,
  onTaskFilterChange,
  taskCounts,
  searchActive,
  onResetTaskFilter,
  selectedTaskId,
  onSelectTask,
  agentName,
  queueIds,
  onReorder,
}: {
  tasks: Task[];
  jobs?: JobSummary[];
  activeTasks: Task[];
  visibleTasks: Task[];
  archived: boolean;
  busy: boolean;
  taskView: "list" | "dag";
  onTaskViewChange: (view: "list" | "dag") => void;
  taskQuery: string;
  onTaskQueryChange: (query: string) => void;
  taskFilter: TaskFilter;
  onTaskFilterChange: (filter: TaskFilter) => void;
  taskCounts: Record<TaskFilter, number>;
  searchActive: boolean;
  onResetTaskFilter: () => void;
  selectedTaskId: string | null;
  onSelectTask: (taskId: string | null) => void;
  agentName: (agentId: string | null) => string;
  queueIds: string[];
  onReorder: (taskId: string, direction: "up" | "down") => void;
}) {
  const taskActions = (
    <TaskPanelActions>
      <div className="view-toggle" role="group" aria-label="任务视图">
        <button
          type="button"
          className={taskView === "list" ? "active" : undefined}
          aria-pressed={taskView === "list"}
          onClick={() => onTaskViewChange("list")}
        >
          列表
        </button>
        <button
          type="button"
          className={taskView === "dag" ? "active" : undefined}
          aria-pressed={taskView === "dag"}
          onClick={() => onTaskViewChange("dag")}
        >
          DAG
        </button>
      </div>
    </TaskPanelActions>
  );

  const taskToolbar = tasks.length > 0 ? (
    <div className="task-toolbar">
      <input
        className="task-search"
        type="search"
        value={taskQuery}
        onChange={(event) => onTaskQueryChange(event.target.value)}
        placeholder="按 key 或标题筛选"
        aria-label="按 key 或标题筛选任务"
      />
      <div className="task-filters" role="group" aria-label="任务筛选">
        {TASK_FILTERS.map((filter) => (
          <button
            key={filter.id}
            type="button"
            className={taskFilter === filter.id ? "active" : undefined}
            aria-pressed={taskFilter === filter.id}
            onClick={() => onTaskFilterChange(filter.id)}
          >
            {filter.label}
            <span className="filter-count">{taskCounts[filter.id]}</span>
          </button>
        ))}
      </div>
    </div>
  ) : null;

  const taskBody = tasks.length === 0 ? (
    <EmptySteps
      title="还没有任务"
      hint="规划者确认方案后会按方案创建任务，任务从这里开始推进："
      steps={[
        "在“项目分析”里描述目标、边界和验收预期并发送。",
        "查看规划者给出的方案，确认方案后任务自动创建。",
        "任务按依赖自动派发；需要调整时由规划者重规划。",
      ]}
    />
  ) : taskView === "dag" ? (
    <TaskDag
      tasks={tasks}
      selectedTaskId={selectedTaskId}
      onSelect={onSelectTask}
    />
  ) : (
    <>
      {visibleTasks.length === 0 ? (
        <div className="empty">
          <div className="empty-title">没有匹配的任务</div>
          <div className="empty-hint">
            {taskQuery.trim()
              ? "没有匹配搜索“" + taskQuery.trim() + "”的任务"
              : "“" + (TASK_FILTERS.find((item) => item.id === taskFilter)?.label ?? "当前筛选") + "”下没有任务"}
            ，可清除搜索或切换筛选条件。
          </div>
          {searchActive ? (
            <div className="empty-actions">
              <button className="secondary" onClick={onResetTaskFilter}>
                清除搜索与筛选
              </button>
            </div>
          ) : null}
        </div>
      ) : (
        <TaskList
          tasks={visibleTasks}
          jobs={jobs}
          activeTasks={activeTasks}
          selectedTaskId={selectedTaskId}
          onSelect={onSelectTask}
          agentName={agentName}
          queueIds={queueIds}
          archived={archived}
          busy={busy}
          onReorder={onReorder}
        />
      )}
    </>
  );

  const taskFooter = tasks.length > 0 && taskView === "list" ? (
    <div className="legend muted">
      阶段：待启动 → 执行 → 验收 → 完成。待启动表示等待前置任务，排队表示等待 Agent 派发。作业数按当前记录统计，自动尝试与拒绝计数不等于历史轮次。派生任务与普通任务遵循同一验收规则；Git 工作区合入后完成，普通工作区直接保留产物。队列调整不改变依赖关系。
    </div>
  ) : null;

  return (
    <TaskPanel
      title={<h2>任务</h2>}
      actions={taskActions}
      toolbar={taskToolbar}
      list={taskBody}
      footer={taskFooter}
    />
  );
}
