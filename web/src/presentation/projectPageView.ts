/// 项目详情页的派生视图：只读取项目快照与页面的临时状态，
/// 把阶段判定、筛选、徽标与作业操作定位统一在这里计算，组件只负责呈现。
import type { ProjectDetail } from "../types";
import { analysisThread } from "./analysisThread";
import { retryableProjectJobId } from "./jobSelection";
import { reordableQueueIds } from "../taskQueue";
import {
  filterTasks,
  hasActiveQuery,
  replanSummary,
  taskFilterCounts,
  type TaskFilter,
} from "./taskList";
import {
  resolveProjectTab,
} from "./projectLayout";

export interface ProjectPageViewInput {
  activeTab: string | null;
  selectedTaskId: string | null;
  taskQuery: string;
  taskFilter: TaskFilter;
  message: string;
  busy: boolean;
  confirming: boolean;
}

export function projectPageView(
  detail: ProjectDetail,
  input: ProjectPageViewInput,
) {
  const { project, tasks, jobs, messages, events, agents, proposal } = detail;
  const activeTasks = tasks.filter((task) => task.status !== "canceled");
  const archived = project.status === "archived";
  const paused = project.status === "paused" || project.status === "pausing";
  const goalMessages = messages.filter((item) => item.role !== "review");
  const reviewMessages = messages.filter((item) => item.role === "review");
  const analysisConversation = analysisThread(goalMessages);
  const canReplan =
    !archived &&
    project.stage !== "completed" &&
    (project.stage === "executing" || project.stage === "summarizing");
  const replanEvents = events.filter(
    (event) => event.kind === "replan_requested" || event.kind === "plan_superseded",
  );
  const agentName = (agentId: string | null) =>
    agentId ? agents.find((agent) => agent.id === agentId)?.name ?? agentId : "—";
  const projectJobs = jobs.filter((job) => !job.task_id);
  const analysisJobs = jobs.filter(
    (job) => job.role === "analysis" && !job.task_id,
  );
  const analysisJob =
    analysisJobs.find((job) => job.status === "running") ?? null;
  const retryableJobId = retryableProjectJobId(project, projectJobs);
  // 队列位置由可调整任务的 (queue_order, seq) 顺序推导，与服务端重排口径一致。
  const queueIds = reordableQueueIds(tasks, archived);

  const taskCounts = taskFilterCounts(tasks);
  const searchActive = hasActiveQuery(input.taskQuery, input.taskFilter);
  const replanPreview = replanSummary(tasks);
  const activeBlock = resolveProjectTab(project.stage, input.activeTab);
  // 选项卡徽标：任务数、待确认方案与报告就绪标记。
  const reportReady =
    !!project.report &&
    (project.stage === "summarizing" || project.stage === "completed");
  const badgeInput = {
    taskCount: activeTasks.length,
    proposalPending: project.stage === "analysis" && !!proposal,
    reportReady,
  };
  const selectedTask =
    detail.tasks.find((task) => task.id === input.selectedTaskId) ?? null;
  const visibleTasks = filterTasks(tasks, input.taskQuery, input.taskFilter);
  // 方案确认：有未发送的修改意见或直接完成冲突时，用提示说明确认被暂时挡住的原因。
  const confirmBlockedReason = !proposal
    ? null
    : input.message.trim()
      ? "输入框里有未发送的修改意见，发送或清空后再确认。"
      : null;

  return {
    ...detail,
    activeTasks,
    archived,
    paused,
    goalMessages,
    reviewMessages,
    analysisConversation,
    canReplan,
    replanEvents,
    agentName,
    projectJobs,
    analysisJob,
    retryableProjectJobId: retryableJobId,
    queueIds,
    taskCounts,
    searchActive,
    replanPreview,
    activeBlock,
    badgeInput,
    selectedTask,
    visibleTasks,
    confirmBlockedReason,
    // 确认按钮与底部输入框共用 busy，避免重复提交。
    confirmBusy: input.busy || input.confirming,
  };
}

export type ProjectPageView = ReturnType<typeof projectPageView>;
