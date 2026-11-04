import type { FormEvent } from "react";
import type { AnalysisProposal, JobSummary, Project, Task } from "../types";
import { EmptySteps, formatTime, Badge } from "../ui";
import { TaskDag } from "./TaskDag";
import {
  TaskList,
  TaskPanel,
  TaskPanelActions,
} from "./TaskBoard";
import { AddTaskForm } from "./AddTaskForm";
import {
  TASK_FILTERS,
  type DependencyOption,
  type TaskDraftCheck,
  type TaskFilter,
} from "../presentation/taskList";
import type { AddTaskDraft } from "../presentation/addTaskDraft";

/// 任务选项卡：视图切换、搜索筛选、添加任务表单、任务列表或 DAG。
export function ProjectTasksSection({
  project,
  tasks,
  jobs,
  activeTasks,
  visibleTasks,
  directMode,
  directJob,
  archived,
  busy,
  proposal,
  taskView,
  onTaskViewChange,
  addTaskOpen,
  onToggleAddTask,
  taskQuery,
  onTaskQueryChange,
  taskFilter,
  onTaskFilterChange,
  taskCounts,
  searchActive,
  onResetTaskFilter,
  draft,
  draftCheck,
  depOptions,
  onTaskField,
  onAddTask,
  selectedTaskId,
  onSelectTask,
  agentName,
  queueIds,
  onReorder,
}: {
  project: Project;
  tasks: Task[];
  jobs?: JobSummary[];
  activeTasks: Task[];
  visibleTasks: Task[];
  directMode: boolean;
  directJob: JobSummary | null;
  archived: boolean;
  busy: boolean;
  proposal: AnalysisProposal | null;
  taskView: "list" | "dag";
  onTaskViewChange: (view: "list" | "dag") => void;
  addTaskOpen: boolean;
  onToggleAddTask: () => void;
  taskQuery: string;
  onTaskQueryChange: (query: string) => void;
  taskFilter: TaskFilter;
  onTaskFilterChange: (filter: TaskFilter) => void;
  taskCounts: Record<TaskFilter, number>;
  searchActive: boolean;
  onResetTaskFilter: () => void;
  draft: AddTaskDraft;
  draftCheck: TaskDraftCheck;
  depOptions: DependencyOption[];
  onTaskField: <K extends keyof AddTaskDraft>(
    field: K,
    value: AddTaskDraft[K],
  ) => void;
  onAddTask: (event: FormEvent) => void;
  selectedTaskId: string | null;
  onSelectTask: (taskId: string | null) => void;
  agentName: (agentId: string | null) => string;
  queueIds: string[];
  onReorder: (taskId: string, direction: "up" | "down") => void;
}) {
  const taskActions = (
    <TaskPanelActions>
      {!directMode ? (
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
      ) : null}
      {!archived && !directMode && proposal?.kind !== "direct" ? (
        <button
          className="secondary"
          disabled={busy}
          onClick={onToggleAddTask}
        >
          {addTaskOpen ? "取消添加" : "添加任务"}
        </button>
      ) : null}
    </TaskPanelActions>
  );

  const taskDirectNotice = directMode ? (
    <div className="direct-mode">
      <p className="subtitle">
        本项目采用直接完成模式，不拆分任务。规划者会一次性完成实现、自检和总结，报告仍需人工验收。
      </p>
      {directJob ? (
        <div className="goal-running">
          <Badge tone="info">直接完成中</Badge>
          <span className="muted">
            {directJob.agent_name ?? "规划 Agent"} ·{" "}
            {formatTime(directJob.started_at)}
          </span>
        </div>
      ) : project.stage === "executing" ? (
        <div className="muted">等待直接完成作业派发。</div>
      ) : (
        <div className="muted">
          直接完成已结束，请查看下方项目报告。
        </div>
      )}
    </div>
  ) : null;

  const taskToolbar = !directMode && tasks.length > 0 ? (
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

  const taskFormCard =
    addTaskOpen && !archived && !directMode && proposal?.kind !== "direct" ? (
      <AddTaskForm
        draft={draft}
        draftCheck={draftCheck}
        depOptions={depOptions}
        busy={busy}
        onField={onTaskField}
        onSubmit={onAddTask}
      />
    ) : null;

  const taskBody = directMode ? null : tasks.length === 0 ? (
    <EmptySteps
      title="还没有任务"
      hint="规划者确认方案后会按方案创建任务，任务从这里开始推进："
      steps={[
        "在“项目分析”里描述目标、边界和验收预期并发送。",
        "查看规划者给出的方案，确认方案后任务自动创建。",
        "任务按依赖自动派发；想提前插入工作时可用右上角“添加任务”。",
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
              ? `没有匹配搜索“${taskQuery.trim()}”的任务`
              : `“${TASK_FILTERS.find((item) => item.id === taskFilter)?.label ?? "当前筛选"}”下没有任务`}
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

  const taskFooter = !directMode && tasks.length > 0 && taskView === "list" ? (
    <div className="legend muted">
      阶段：待启动 → 执行 → 验收 → 完成。待启动表示等待前置任务，排队表示等待 Agent 派发。作业数按当前记录统计，自动尝试与拒绝计数不等于历史轮次。派生任务与普通任务遵循同一验收规则；Git 工作区合入后完成，普通工作区直接保留产物。队列调整不改变依赖关系。
    </div>
  ) : null;

  return (
    <TaskPanel
      title={<h2>{directMode ? "直接完成" : "任务"}</h2>}
      actions={taskActions}
      leading={taskDirectNotice}
      toolbar={taskToolbar}
      form={taskFormCard}
      list={taskBody}
      footer={taskFooter}
    />
  );
}
