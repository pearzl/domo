/**
 * 项目详情页。主体选项卡与任务抽屉的选中状态同步到 URL 查询参数，
 * 刷新、复制链接与浏览器前进后退都回到同一视图。
 *
 * 人工核对清单（输入持久化）：
 * 1. 在“项目分析”输入框输入文字，刷新页面，内容仍在。
 * 2. 输入文字后离开项目页，再后退/前进回到项目页，内容仍在。
 * 3. 成功发送后输入框清空，刷新后仍为空。
 * 4. 打开任务抽屉或切换主体选项卡，地址栏出现 tab/task 参数；刷新与后退后视图一致。
 * 5. 方向键在选项卡内左右切换，Home/End 跳到首尾；每项徽标随任务、方案与报告状态更新。
 * 6. 概览里的用量摘要显示作业数、token、费用、时长与覆盖；没有记录的指标显示“—”，
 *    “查看用量明细”带着项目过滤跳到用量页；项目页不再有独立的用量面板。
 */
import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { FormEvent } from "react";
import type {
  ArbitrationContext,
  JobActivity,
  JobSummary,
  ProjectArchiveRefs,
  ProjectDetail,
  Task,
  TaskAgentPool,
} from "../types";
import { api, errorMessage, subscribeEvents } from "../api";
import { AgentPoolPanel } from "../components/AgentPoolPanel";
import { InputBox } from "../components/InputBox";
import { useDraftJson, useDraftValue } from "../components/useDraft";
import { EventTimeline } from "../components/EventTimeline";
import { JobDetailView, JobRecordList } from "../components/JobDetailView";
import { LiveOutput } from "../components/LiveOutput";
import { ProjectOverview } from "../components/ProjectOverview";
import { ProjectTopBlocks } from "../components/ProjectTopBlocks";
import { ProjectHeader } from "../components/ProjectHeader";
import { ProjectTabs, tabButtonId, tabPanelId } from "../components/ProjectTabs";
import { RichContent } from "../components/RichContent";
import { TaskDag } from "../components/TaskDag";
import { isReordableQueueTask, reordableQueueIds } from "../taskQueue";
import {
  ACTIVITY_WAITING_TEXT,
  activityDigest,
  type ActivityDigestItem,
} from "../presentation/activity";
import { analysisThread } from "../presentation/analysisThread";
import {
  TASK_FILTERS,
  type TaskFilter,
  dependencyOptions,
  filterTasks,
  hasActiveQuery,
  nextTaskKey,
  queueSummary,
  replanSummary,
  taskFilterCounts,
  type QueuePositionSummary,
  validateTaskDraft,
} from "../presentation/taskList";
import {
  DRAWER_LEAD_TITLES,
  drawerLayout,
  rawErrorEntry,
  type DrawerTailSection,
} from "../presentation/taskDrawer";
import {
  projectBlockOrder,
  projectTabBadge,
  resolveProjectTab,
} from "../presentation/projectLayout";
import type { GuidanceTab } from "../presentation/projectStatus";
import {
  readProjectView,
  updateProjectView,
  type ProjectViewState,
} from "../presentation/projectView";
import {
  Badge,
  EmptySteps,
  ErrorBox,
  FAILURE_KIND_LABELS,
  ROLE_LABELS,
  TaskStateBadge,
  formatTime,
  failureSummaryLabel,
  jobStatusLabel,
  shortText,
  taskStateLabel,
} from "../ui";

/// Mirrors the backend marker that is written before a worktree is removed.
const ARTIFACTS_RELEASING = "releasing";

interface AddTaskDraft {
  key: string;
  title: string;
  description: string;
  acceptance: string;
  deps: string[];
  milestone: boolean;
}

const EMPTY_TASK_DRAFT: AddTaskDraft = {
  key: "",
  title: "",
  description: "",
  acceptance: "",
  deps: [],
  milestone: false,
};

function isAddTaskDraft(value: unknown): value is AddTaskDraft {
  if (!value || typeof value !== "object") return false;
  const record = value as Record<string, unknown>;
  return (
    typeof record.key === "string" &&
    typeof record.title === "string" &&
    typeof record.description === "string" &&
    typeof record.acceptance === "string" &&
    Array.isArray(record.deps) &&
    record.deps.every((item) => typeof item === "string") &&
    typeof record.milestone === "boolean"
  );
}

function readWindowView(): ProjectViewState {
  if (typeof window === "undefined") return { tab: null, taskId: null };
  return readProjectView(window.location.search);
}

export function ProjectPage({
  id,
  onBack,
}: {
  id: string;
  onBack: () => void;
}) {
  const [detail, setDetail] = useState<ProjectDetail | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const analysisDraft = useDraftValue(
    { projectId: id, field: "analysis-message" },
    "",
  );
  const message = analysisDraft.value;
  const setMessage = analysisDraft.setValue;
  const feedbackDraft = useDraftValue(
    { projectId: id, field: "report-feedback" },
    "",
  );
  const feedback = feedbackDraft.value;
  const setFeedback = feedbackDraft.setValue;
  const [busy, setBusy] = useState(false);
  const initialView = readWindowView();
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(
    initialView.taskId,
  );
  const [activeTab, setActiveTab] = useState<string | null>(initialView.tab);
  const [replanOpen, setReplanOpen] = useState(false);
  const replanDraft = useDraftValue(
    { projectId: id, field: "replan-reason" },
    "",
  );
  const replanReason = replanDraft.value;
  const setReplanReason = replanDraft.setValue;
  const [addTaskOpen, setAddTaskOpen] = useState(false);
  const taskDraft = useDraftJson(
    { projectId: id, field: "add-task-form" },
    EMPTY_TASK_DRAFT,
    isAddTaskDraft,
  );
  const taskForm = taskDraft.value;
  const taskKey = taskForm.key;
  const taskTitle = taskForm.title;
  const taskDescription = taskForm.description;
  const taskAcceptance = taskForm.acceptance;
  const taskDeps = taskForm.deps;
  const taskMilestone = taskForm.milestone;
  /// 任务面板视图只在页面内保持，不写入任务数据。
  const [taskView, setTaskView] = useState<"list" | "dag">("list");
  /// 任务搜索与筛选同样只存在于页面状态，不改变任务数据。
  const [taskQuery, setTaskQuery] = useState("");
  const [taskFilter, setTaskFilter] = useState<TaskFilter>("all");
  const [merging, setMerging] = useState(false);
  const [mergeNotice, setMergeNotice] = useState<{
    tone: "ok" | "error";
    text: string;
  } | null>(null);
  const [archiveRefs, setArchiveRefs] = useState<ProjectArchiveRefs | null>(
    null,
  );

  function setTaskField<K extends keyof AddTaskDraft>(
    field: K,
    value: AddTaskDraft[K],
  ) {
    taskDraft.setValue((current) => ({ ...current, [field]: value }));
  }

  /// 视图状态写入 URL：用户切换选项卡或打开抽屉时新增历史，程序修正时替换当前历史。
  function applyProjectView(
    patch: Partial<ProjectViewState>,
    mode: "push" | "replace" = "push",
  ) {
    if (typeof window === "undefined") return;
    const nextSearch = updateProjectView(window.location.search, patch);
    const next = `${window.location.pathname}${nextSearch}`;
    if (`${window.location.pathname}${window.location.search}` === next) return;
    if (mode === "replace") {
      window.history.replaceState(window.history.state, "", next);
    } else {
      window.history.pushState({}, "", next);
    }
  }

  function selectTask(taskId: string | null) {
    setSelectedTaskId(taskId);
    applyProjectView({ taskId }, taskId ? "push" : "replace");
  }

  function selectTab(tab: string) {
    setActiveTab(tab);
    setSelectedTaskId(null);
    applyProjectView({ tab, taskId: null }, "push");
  }

  // 浏览器前进/后退时同步选项卡与抽屉；路径变化由 App 负责。
  useEffect(() => {
    function handlePop() {
      const view = readWindowView();
      setActiveTab(view.tab);
      setSelectedTaskId(view.taskId);
    }
    window.addEventListener("popstate", handlePop);
    return () => window.removeEventListener("popstate", handlePop);
  }, []);

  async function refresh(silent = false) {
    try {
      const next = await api.getProject(id);
      setDetail(next);
      setError("");
    } catch (err) {
      if (!silent) setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    setLoading(true);
    void refresh();
    const timer = window.setInterval(() => void refresh(true), 2500);
    const unsubscribe = subscribeEvents(() => void refresh(true));
    return () => {
      window.clearInterval(timer);
      unsubscribe();
    };
  }, [id]);

  const selectedTask = useMemo(
    () => detail?.tasks.find((task) => task.id === selectedTaskId) ?? null,
    [detail, selectedTaskId],
  );
  const visibleTasks = useMemo(
    () => filterTasks(detail?.tasks ?? [], taskQuery, taskFilter),
    [detail, taskQuery, taskFilter],
  );
  // 选中的任务被搜索或筛选隐藏时关闭抽屉，避免抽屉与列表状态不一致。
  useEffect(() => {
    if (
      selectedTaskId !== null &&
      detail !== null &&
      !visibleTasks.some((task) => task.id === selectedTaskId)
    ) {
      setSelectedTaskId(null);
      applyProjectView({ taskId: null }, "replace");
    }
  }, [detail, selectedTaskId, visibleTasks]);

  // 选项卡必须属于当前阶段；URL 中缺失或失效时回退到阶段默认值并写回地址栏。
  useEffect(() => {
    if (!detail) return;
    const next = resolveProjectTab(detail.project.stage, activeTab);
    if (next !== activeTab) setActiveTab(next);
    applyProjectView({ tab: next, taskId: selectedTaskId }, "replace");
  }, [detail, activeTab, selectedTaskId]);

  useEffect(() => {
    if (detail?.project.status !== "archived") {
      setArchiveRefs(null);
      return;
    }
    let active = true;
    void api
      .getProjectArchiveRefs(id)
      .then((next) => {
        if (active) setArchiveRefs(next);
      })
      .catch(() => {
        if (active) setArchiveRefs(null);
      });
    return () => {
      active = false;
    };
  }, [id, detail?.project.status]);

  async function run(action: () => Promise<unknown>): Promise<boolean> {
    setBusy(true);
    setError("");
    try {
      await action();
      await refresh();
      return true;
    } catch (err) {
      setError(errorMessage(err));
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function handleArchive() {
    setBusy(true);
    setError("");
    try {
      const preview = await api.getArchivePreview(id);
      const dirty = preview.dirty;
      let prompt = "归档后不会继续推进，也不能恢复。确认归档？";
      if (dirty.length > 0) {
        const shown = dirty
          .slice(0, 5)
          .map((item) => `${item.label}（${item.path}）`)
          .join("\n");
        const more =
          dirty.length > 5 ? `\n…等 ${dirty.length} 个工作区` : "";
        prompt = `以下工作区有未提交改动：\n${shown}${more}\n\n归档后会保留这些工作区，项目工作区可能无法完全清理。确认归档？`;
      }
      if (!window.confirm(prompt)) return;
      await api.archiveProject(id, dirty.length > 0);
      await refresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  if (loading) return <div className="page muted">加载项目…</div>;
  if (!detail) {
    return (
      <div className="page">
        <button className="ghost" onClick={onBack}>
          ← 返回项目
        </button>
        <ErrorBox message={error || "项目不存在"} />
      </div>
    );
  }

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
  const replanSourceLabel = (source: string | null) =>
    source === "executor" ? "执行者" : source === "human" ? "人工" : "未知";
  const agentName = (agentId: string | null) =>
    agentId ? agents.find((agent) => agent.id === agentId)?.name ?? agentId : "—";
  const projectJobs = jobs.filter((job) => !job.task_id);
  const analysisJobs = jobs.filter(
    (job) => job.role === "analysis" && !job.task_id,
  );
  const analysisJob =
    analysisJobs.find((job) => job.status === "running") ?? null;
  const directJob = jobs.find(
    (job) => job.role === "direct" && job.status === "running",
  );
  const directMode = project.completion_mode === "direct";
  const retryableProjectJobId =
    !archived &&
    project.status === "blocked" &&
    !(project.stage === "summarizing" && project.report)
      ? latestFailedJobId(projectJobs)
      : null;
  // 队列位置由可调整任务的 (queue_order, seq) 顺序推导，与服务端重排口径一致。
  const queueIds = reordableQueueIds(tasks, archived);

  const taskCounts = taskFilterCounts(tasks);
  const searchActive = hasActiveQuery(taskQuery, taskFilter);
  // 添加任务草稿的行内校验；活动任务复用 activeTasks，与依赖候选、服务端口径一致。
  const draftCheck = validateTaskDraft(
    {
      key: taskKey,
      title: taskTitle,
      description: taskDescription,
      deps: taskDeps,
    },
    activeTasks,
  );
  const depOptions = dependencyOptions(activeTasks);
  const replanPreview = replanSummary(tasks);
  const blockOrder = projectBlockOrder(project.stage);
  const activeBlock = resolveProjectTab(project.stage, activeTab);
  // 选项卡徽标：任务数、待确认方案与报告就绪标记。
  const reportReady =
    !!project.report &&
    (project.stage === "summarizing" || project.stage === "completed");
  const badgeInput = {
    taskCount: activeTasks.length,
    proposalPending: !!proposal,
    reportReady,
  };

  // 概览里的待办跳转：直接切到待办指向的选项卡（分析或报告）。
  function openAnalysisTab(tab?: GuidanceTab) {
    selectTab(tab ?? "analysis");
  }

  // 打开添加任务表单：切到任务选项卡，key 为空时自动填入下一个可用的 T 编号。
  function openAddTaskForm() {
    selectTab("tasks");
    setAddTaskOpen(true);
    taskDraft.setValue((current) => ({
      ...current,
      key: current.key.trim() || nextTaskKey(tasks),
    }));
  }

  function toggleAddTaskForm() {
    if (addTaskOpen) {
      setAddTaskOpen(false);
      return;
    }
    openAddTaskForm();
  }

  async function handleMerge() {
    if (
      !window.confirm(
        `将项目分支 ${project.branch} rebase 到 ${project.base_branch}，并把 ${project.base_branch} 快进到项目分支提交。确认合入？`,
      )
    ) {
      return;
    }
    setMerging(true);
    setMergeNotice(null);
    try {
      const result = await api.mergeProject(project.id);
      setMergeNotice({ tone: "ok", text: result.message });
      await refresh();
    } catch (err) {
      setMergeNotice({ tone: "error", text: errorMessage(err) });
    } finally {
      setMerging(false);
    }
  }

  async function handleAddTask(event: FormEvent) {
    event.preventDefault();
    if (!draftCheck.valid) return;
    const key = taskKey.trim();
    const ok = await run(() =>
      api.createTask(project.id, {
        key,
        title: taskTitle.trim(),
        description: taskDescription.trim(),
        acceptance: taskAcceptance
          .split("\n")
          .map((item) => item.trim())
          .filter(Boolean),
        deps: taskDeps,
        milestone: taskMilestone,
      }),
    );
    if (!ok) return;
    taskDraft.clear(EMPTY_TASK_DRAFT);
    setAddTaskOpen(false);
  }

  function resetTaskFilter() {
    setTaskQuery("");
    setTaskFilter("all");
  }

  const analysisBlock = project.stage === "analysis" ? (
    <section className="panel">
      <h2>项目分析</h2>
      <p className="subtitle">
        规划者会理解目标与现状，必要时澄清，给出可确认的方案：任务拆分、依赖和验收标准。确认方案后才开始执行。
      </p>
      <label className="goal-policy">
        <input
          type="checkbox"
          checked={project.confirm_mode === "auto"}
          disabled={busy || archived || paused || !!analysisJob}
          onChange={(event) =>
            run(() =>
              api.updateAnalysisPolicy(
                project.id,
                event.target.checked ? "auto" : "manual",
              ),
            )
          }
        />
        <span>
          自动确认：方案足够明确时允许 AI 跳过人工确认，直接开始执行；关闭后每个方案都需要你点击确认。
        </span>
      </label>
      <div className="messages">
        {analysisConversation.messages.map((item) =>
          item.folded ? (
            <details
              key={item.message.id}
              className="message message-planner message-folded"
            >
              <summary>
                <span className="message-role">规划者</span>
                <span className="muted">上一条分析结果 · 已折叠，点击展开</span>
              </summary>
              <RichContent
                content={item.message.content}
                className="message-content"
              />
            </details>
          ) : (
            <div
              key={item.message.id}
              className={`message message-${item.message.role}`}
            >
              <div className="message-role">
                {item.message.role === "user" ? "你" : "规划者"}
              </div>
              <RichContent
                content={item.message.content}
                className="message-content"
              />
            </div>
          ),
        )}
        {analysisConversation.waiting && project.status !== "blocked" ? (
          <div className="message-waiting" role="status">
            <span className="message-role">规划者</span>
            <span className="muted">
              {analysisConversation.waiting === "start"
                ? "等待规划者开始分析…"
                : "已收到你的反馈，等待规划者给出下一次分析结果…"}
            </span>
          </div>
        ) : null}
        {goalMessages.length === 0 ? (
          <div className="muted">还没有输入项目目标。</div>
        ) : null}
      </div>
      {analysisJob ? (
        <AnalysisLiveActivity jobId={analysisJob.id} />
      ) : null}
      {analysisJob ? (
        <div className="goal-running">
          <Badge tone="info">规划者正在分析</Badge>
          <span className="muted">
            {analysisJob.agent_name ?? "规划 Agent"} ·{" "}
            {formatTime(analysisJob.started_at)}
          </span>
        </div>
      ) : null}
      {proposal ? (
        <div className="goal-proposal">
          <div className="goal-proposal-head">
            <Badge tone={proposal.kind === "direct" ? "warn" : "success"}>
              {proposal.kind === "direct" ? "建议直接完成" : "项目方案"}
            </Badge>
            <div className="goal-proposal-summary">
              <RichContent content={proposal.summary} />
            </div>
          </div>
          {proposal.goal ? (
            <div>
              <strong>确认后的目标：</strong>
              <RichContent content={proposal.goal} />
            </div>
          ) : null}
          {proposal.assumptions.length ? (
            <div>
              <strong>关键假设</strong>
              <ul>
                {proposal.assumptions.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </div>
          ) : null}
          {proposal.acceptance.length ? (
            <div>
              <strong>验收口径</strong>
              <ul>
                {proposal.acceptance.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </div>
          ) : null}
          {proposal.tasks.length ? (
            <div>
              <strong>任务方案</strong>
              <ul>
                {proposal.tasks.map((task) => (
                  <li key={task.key}>
                    <div>
                      <strong>
                        {task.key} {task.title}
                      </strong>
                      {task.deps.length ? (
                        <span className="muted">
                          （依赖 {task.deps.join("、")}）
                        </span>
                      ) : null}
                    </div>
                    <RichContent content={task.description} />
                    {task.acceptance.length ? (
                      <ul>
                        {task.acceptance.map((item) => (
                          <li key={item}>验收：{item}</li>
                        ))}
                      </ul>
                    ) : null}
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
          {proposal.plan.length ? (
            <div>
              <strong>执行步骤</strong>
              <ul>
                {proposal.plan.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </div>
          ) : null}
          {proposal.verification.length ? (
            <div>
              <strong>自检方式</strong>
              <ul>
                {proposal.verification.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </div>
          ) : null}
        </div>
      ) : null}
      {!analysisJob ? (
        <div className="confirm-row">
          <InputBox
            value={message}
            onChange={setMessage}
            draftKey={analysisDraft.key}
            disabled={busy || paused || archived}
            rows={3}
            maxAutoGrowChars={520}
            placeholder={
              goalMessages.length === 0
                ? "描述项目目标、边界和验收预期。"
                : "补充信息、回答规划者的问题，或对方案提出修改意见。"
            }
            actions={
              <div className="button-row">
                <button
                  className="secondary"
                  disabled={busy || paused || archived || !message.trim()}
                  onClick={async () => {
                    const ok = await run(() =>
                      api.postAnalysisMessage(
                        project.id,
                        message,
                        project.confirm_mode,
                      ),
                    );
                    if (ok) analysisDraft.clear();
                  }}
                >
                  {goalMessages.length === 0 ? "开始分析" : "发送"}
                </button>
                {proposal ? (
                  <button
                    className="primary"
                    disabled={
                      busy ||
                      paused ||
                      archived ||
                      !!message.trim() ||
                      (proposal.kind === "direct" && activeTasks.length > 0)
                    }
                    title={
                      message.trim()
                        ? "请先发送修改意见或清空输入后再确认"
                        : proposal.kind === "direct" && activeTasks.length > 0
                          ? "项目已有任务，不能直接完成"
                          : undefined
                    }
                    onClick={async () => {
                      const ok = await run(() =>
                        api.confirmAnalysis(project.id),
                      );
                      if (ok) analysisDraft.clear();
                    }}
                  >
                    {proposal.kind === "direct"
                      ? "确认并直接完成"
                      : "确认方案，开始执行"}
                  </button>
                ) : null}
              </div>
            }
          />
        </div>
      ) : null}
    </section>
  ) : goalMessages.length > 1 ? (
    <details className="panel">
      <summary>项目分析记录</summary>
      <div className="messages compact">
        {goalMessages.map((item) => (
          <div key={item.id} className={`message message-${item.role}`}>
            <div className="message-role">{item.role === "user" ? "你" : "规划者"}</div>
            <RichContent content={item.content} className="message-content" />
          </div>
        ))}
      </div>
    </details>
  ) : null;

  const tasksBlock = (
    <section className="panel">
      <div className="panel-head">
        <h2>{directMode ? "直接完成" : "任务"}</h2>
        <div className="task-panel-actions">
          {!directMode ? (
            <div className="view-toggle" role="group" aria-label="任务视图">
              <button
                type="button"
                className={taskView === "list" ? "active" : undefined}
                aria-pressed={taskView === "list"}
                onClick={() => setTaskView("list")}
              >
                列表
              </button>
              <button
                type="button"
                className={taskView === "dag" ? "active" : undefined}
                aria-pressed={taskView === "dag"}
                onClick={() => setTaskView("dag")}
              >
                DAG
              </button>
            </div>
          ) : null}
          {!archived && !directMode && proposal?.kind !== "direct" ? (
            <button
              className="secondary"
              disabled={busy}
              onClick={toggleAddTaskForm}
            >
              {addTaskOpen ? "取消添加" : "添加任务"}
            </button>
          ) : null}
        </div>
      </div>
      {directMode ? (
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
      ) : null}
      {!directMode && tasks.length > 0 ? (
        <div className="task-toolbar">
          <input
            className="task-search"
            type="search"
            value={taskQuery}
            onChange={(event) => setTaskQuery(event.target.value)}
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
                onClick={() => setTaskFilter(filter.id)}
              >
                {filter.label}
                <span className="filter-count">{taskCounts[filter.id]}</span>
              </button>
            ))}
          </div>
        </div>
      ) : null}
      {addTaskOpen && !archived && !directMode && proposal?.kind !== "direct" ? (
        <form className="form add-task-form" onSubmit={handleAddTask}>
          <div className="form-grid">
            <label>
              <span>任务 key</span>
              <input
                value={taskKey}
                onChange={(event) => setTaskField("key", event.target.value)}
                placeholder="例如：T3"
                aria-invalid={draftCheck.errors.key ? true : undefined}
                required
              />
              {draftCheck.errors.key ? (
                <small className="field-error">{draftCheck.errors.key}</small>
              ) : null}
            </label>
            <label>
              <span>标题</span>
              <input
                value={taskTitle}
                onChange={(event) => setTaskField("title", event.target.value)}
                placeholder="一句话说明这个任务做什么"
                required
              />
            </label>
          </div>
          <div className="field">
            <span className="field-label">描述</span>
            <InputBox
              value={taskDescription}
              onChange={(next) => setTaskField("description", next)}
              rows={3}
              placeholder="任务目标、边界和实现要点。"
            />
          </div>
          <div className="field">
            <span className="field-label">验收标准（每行一条，留空使用默认标准）</span>
            <InputBox
              value={taskAcceptance}
              onChange={(next) => setTaskField("acceptance", next)}
              rows={3}
              placeholder={"例如：\ndomo_mock/T3.txt 存在且包含 T3"}
            />
          </div>
          <div className="field">
            <span className="field-label">前置任务（依赖）</span>
            <small className="muted">
              只引用当前活动任务；已完成、执行中的任务都能作为依赖，已取消任务不在此列出。
            </small>
            {depOptions.length === 0 ? (
              <span className="muted">项目还没有可依赖的任务。</span>
            ) : (
              <div className="checkbox-row">
                {depOptions.map((option) => (
                  <label className="checkbox-label" key={option.key}>
                    <input
                      type="checkbox"
                      checked={taskDeps.includes(option.key)}
                      onChange={(event) => {
                        setTaskField(
                          "deps",
                          event.target.checked
                            ? [...taskDeps, option.key]
                            : taskDeps.filter((dep) => dep !== option.key),
                        );
                      }}
                    />
                    <span>
                      {option.key} {option.title}
                      <span className="dep-stage muted"> · {option.stageLabel}</span>
                    </span>
                  </label>
                ))}
              </div>
            )}
            {draftCheck.errors.deps ? (
              <small className="field-error">{draftCheck.errors.deps}</small>
            ) : null}
          </div>
          <label className="checkbox-label">
            <input
              type="checkbox"
              checked={taskMilestone}
              onChange={(event) => setTaskField("milestone", event.target.checked)}
            />
            <span>标记为里程碑（验收通过后需人工确认）</span>
          </label>
          <div className="form-actions">
            <button
              className="primary"
              type="submit"
              disabled={busy || !draftCheck.valid}
              title={draftCheck.valid ? undefined : "请先修正表单中的提示"}
            >
              {busy ? "提交中…" : "添加任务"}
            </button>
            <span className="muted">
              项目在总结或完成阶段时会回到执行阶段，原报告作废待重写。
            </span>
          </div>
        </form>
      ) : null}
      {directMode ? null : tasks.length === 0 ? (
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
          onSelect={selectTask}
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
                  <button className="secondary" onClick={resetTaskFilter}>
                    清除搜索与筛选
                  </button>
                </div>
              ) : null}
            </div>
          ) : (
        <div className="task-list">
          {visibleTasks.map((task) => {
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
            return (
              <div
                key={task.id}
                className={`task-row ${task.id === selectedTaskId ? "selected" : ""}`}
                role="button"
                tabIndex={0}
                onClick={() => selectTask(task.id)}
                onKeyDown={(event) => {
                  if (event.key === " " || event.key === "Enter") {
                    // Space 默认会滚动页面，打开抽屉时阻止滚动。
                    event.preventDefault();
                    selectTask(task.id);
                  }
                }}
              >
                <span className="task-key">{task.key}</span>
                <span className="task-title">{task.title}</span>
                <span className="task-state">
                  <TaskStateBadge stage={task.stage} status={task.status} />
                  {task.queue_position !== null ? (
                    <Badge tone="info">队列 #{task.queue_position}</Badge>
                  ) : null}
                  {task.milestone ? <Badge tone="warn">里程碑</Badge> : null}
                </span>
                <span className="task-deps muted" title={task.waiting_reason ?? ""}>
                  {task.deps.length ? `依赖 ${task.deps.join(", ")}` : "无依赖"}
                  {task.parent_id ? " · 派生任务" : ""}
                  {children.length
                    ? ` · 子任务 ${childCompleted}/${children.length}`
                    : ""}
                  {waitingChildren ? " · 等待子任务" : ""}
                  {task.waiting_reason ? ` · ${task.waiting_reason}` : ""}
                </span>
                <span className="task-agent muted">{agentName(task.agent_id)}</span>
                <span className="task-counts muted">
                  {task.attempts ? `执行 ${task.attempts}` : ""}
                  {task.rejections ? ` 拒绝 ${task.rejections}` : ""}
                  {isReordableQueueTask(task, archived) ? (
                    <QueueControls
                      task={task}
                      summary={queueSummary(queueIds, task.id)}
                      busy={busy}
                      onReorder={(direction) =>
                        run(() =>
                          api.reorderTaskQueue(project.id, task.id, direction),
                        )
                      }
                    />
                  ) : null}
                </span>
              </div>
            );
          })}
        </div>
          )}
        </>
      )}
      {!directMode && tasks.length > 0 && taskView === "list" ? (
        <div className="legend muted">
          阶段：待启动 → 执行 → 验收 → 完成。执行与验收都在独立 worktree 中进行；派生任务跳过验收，完成后直接合入父任务分支。“作业”指一次 Agent 调用，每次执行或验收作业都有独立的输入、输出与用量。上移/下移只调整同一可调整队列内的派发顺序，不改变依赖关系。
        </div>
      ) : null}
    </section>
  );

  const reportBlock = project.stage === "summarizing" || project.report ? (
    <section className="panel">
      <h2>项目报告{reviewMessages.length > 0 ? `（${reviewMessages.length} 条评议）` : ""}</h2>
      {reviewMessages.length > 0 ? (
        <details className="report-review-log" open={!!project.report}>
          <summary>评议记录（{reviewMessages.length}）</summary>
          <div className="messages compact">
            {reviewMessages.map((item) => (
              <div key={item.id} className="message message-user">
                <div className="message-role">你的意见</div>
                <RichContent content={item.content} className="message-content" />
              </div>
            ))}
          </div>
        </details>
      ) : null}
      {project.report ? (
        <RichContent content={project.report} className="report" />
      ) : project.stage === "summarizing" ? (
        project.blocked_reason ? (
          <p className="muted">报告生成失败，可在上方失败作业上重试。</p>
        ) : (
          <p className="muted">规划者正在依据评议意见重写报告…</p>
        )
      ) : null}
      {project.stage === "summarizing" ? (
        <div className="report-actions">
          <InputBox
            value={feedback}
            onChange={setFeedback}
            draftKey={feedbackDraft.key}
            rows={3}
            maxAutoGrowChars={520}
            placeholder="对报告的意见（提交意见可供多轮讨论；要求重写或重新规划时必须填写）。"
            actions={
              <div className="button-row">
                <button
                  className="primary"
                  disabled={busy || !project.report}
                  onClick={async () => {
                    const ok = await run(() =>
                      api.submitReport(project.id, "accept"),
                    );
                    if (ok) feedbackDraft.clear();
                  }}
                >
                  通过并完成
                </button>
                <button
                  className="secondary"
                  disabled={busy || !feedback.trim()}
                  onClick={async () => {
                    const ok = await run(() =>
                      api.submitReport(project.id, "comment", feedback),
                    );
                    if (ok) feedbackDraft.clear();
                  }}
                >
                  提交意见
                </button>
                <button
                  className="secondary"
                  disabled={busy || !project.report || !feedback.trim()}
                  onClick={async () => {
                    const ok = await run(() =>
                      api.submitReport(project.id, "reject", feedback),
                    );
                    if (ok) feedbackDraft.clear();
                  }}
                  title={
                    project.completion_mode === "direct"
                      ? "记录意见并重新执行直接完成"
                      : "记录意见并要求规划者重新撰写报告"
                  }
                >
                  {project.completion_mode === "direct" ? "重做并重新总结" : "重新撰写"}
                </button>
                <button
                  className="secondary"
                  disabled={busy || !project.report || !feedback.trim()}
                  onClick={async () => {
                    const ok = await run(() =>
                      api.submitReport(project.id, "replan", feedback),
                    );
                    if (ok) feedbackDraft.clear();
                  }}
                  title="记录意见并要求重新规划，项目回到项目分析阶段"
                >
                  要求重新规划
                </button>
                {project.completion_mode !== "direct" ? (
                  <button
                    className="secondary"
                    disabled={busy || archived}
                    onClick={openAddTaskForm}
                    title="通过添加任务表单增加新任务；添加后项目回到执行阶段，原报告作废"
                  >
                    增加新任务
                  </button>
                ) : null}
              </div>
            }
          />
          {project.completion_mode !== "direct" ? (
            <p className="muted">
              “增加新任务”会打开任务面板的添加任务表单（复用人工加任务端点）；提交后项目回到执行阶段，原报告作废待重写。
            </p>
          ) : (
            <p className="muted">
              直接完成模式下，“重做并重新总结”会重新执行并生成新报告。
            </p>
          )}
        </div>
      ) : null}
    </section>
  ) : null;

  // 信息页：参考类信息（重规划记录、用量、项目作业、归档）集中在信息选项卡。
  const infoBlock = (
    <div className="project-info">
      {project.replan_reason || replanEvents.length > 0 ? (
        <section className="panel">
          <h2>重规划记录</h2>
          {project.replan_reason ? (
            <div className="blocked-banner">
              <strong>
                最近一次重规划（来源：{replanSourceLabel(project.replan_source)}）
              </strong>
              <RichContent content={project.replan_reason} />
              {project.replan_requested_at ? (
                <div className="muted">{formatTime(project.replan_requested_at)}</div>
              ) : null}
            </div>
          ) : null}
          <EventTimeline
            events={replanEvents}
            tasks={tasks}
            jobs={jobs}
            defaultFilter="all"
            showFilters={false}
            emptyText="暂无重规划事件"
          />
        </section>
      ) : null}

      {projectJobs.length > 0 ? (
        <section className="panel">
          <h2>项目分析 / 直接完成 / 总结作业</h2>
          <JobRecordList
            jobs={projectJobs}
            renderJob={(job) => (
              <JobDetailView
                key={job.id}
                job={job}
                retryable={job.id === retryableProjectJobId}
                onRetry={() => run(() => api.retryJob(job.id))}
              />
            )}
          />
        </section>
      ) : null}

      {archived && archiveRefs ? (
        <details className="panel">
          <summary>归档历史与备份</summary>
          <p className="muted">
            任务与项目分支的提交保存在 refs/domo-archive。恢复前先确认同名分支不存在。
          </p>
          {archiveRefs.project ? (
            <>
              <div className="kv">
                <span>项目分支</span>
                <span className="mono">
                  {archiveRefs.project.reference} @{" "}
                  {archiveRefs.project.tip.slice(0, 7)}
                </span>
              </div>
              <div className="muted mono">{archiveRefs.project.recovery}</div>
              <div className="muted mono">{archiveRefs.project.log_command}</div>
            </>
          ) : null}
          {archiveRefs.tasks.length > 0 ? (
            <div className="derive-list">
              {archiveRefs.tasks.map((item) => (
                <div className="derive-child" key={item.task_id}>
                  <div className="derive-child-head">
                    <span className="task-key">{item.key}</span>
                    <span className="derive-child-title">{item.title}</span>
                    <Badge tone="muted">{item.tip.slice(0, 7)}</Badge>
                  </div>
                  <div className="muted mono">{item.reference}</div>
                  <div className="muted mono">{item.recovery}</div>
                  <div className="muted mono">{item.log_command}</div>
                </div>
              ))}
            </div>
          ) : (
            <div className="muted">没有任务分支归档记录。</div>
          )}
          <div className="kv">
            <span>备份命令</span>
            <span className="mono">{archiveRefs.backup_command}</span>
          </div>
        </details>
      ) : null}
    </div>
  );

  return (
    <div className="page">
      <div className="back-row">
        <button className="ghost" onClick={onBack}>
          ← 返回项目
        </button>
      </div>

      <ProjectHeader
        detail={detail}
        canReplan={canReplan}
        busy={busy}
        merging={merging}
        mergeNotice={mergeNotice}
        onResume={() => void run(() => api.resumeProject(project.id))}
        onPause={() => void run(() => api.pauseProject(project.id))}
        onForcePause={() => {
          if (
            window.confirm(
              "强制暂停会立即终止所有在途作业，任务进入已暂停；之后点“恢复”继续推进。确认继续？",
            )
          ) {
            void run(() => api.pauseProject(project.id, true));
          }
        }}
        onToggleReplan={() => setReplanOpen((value) => !value)}
        onArchive={() => void handleArchive()}
        onMerge={() => void handleMerge()}
      />

      {error ? <ErrorBox message={error} /> : null}

      {replanOpen && canReplan ? (
        <section className="panel">
          <h2>重新规划</h2>
          <p className="subtitle">
            已完成任务会保留；未完成任务会退出当前 DAG 并停止派发。提交后规划者重新给出方案，确认方案后才继续执行。请说明需要重新规划的原因。
          </p>
          <div className="replan-preview">
            <div className="replan-preview-group">
              <strong>将取消 {replanPreview.cancelCount} 个未完成任务</strong>
              {replanPreview.cancelKeys.length ? (
                <span className="mono replan-keys">
                  {replanPreview.cancelKeys.join("、")}
                </span>
              ) : (
                <span className="muted">没有未完成任务</span>
              )}
            </div>
            <div className="replan-preview-group">
              <strong>保留 {replanPreview.keepCount} 个已完成任务</strong>
              {replanPreview.keepKeys.length ? (
                <span className="mono replan-keys">
                  {replanPreview.keepKeys.join("、")}
                </span>
              ) : (
                <span className="muted">没有已完成任务</span>
              )}
            </div>
          </div>
          <div className="confirm-row">
            <InputBox
              value={replanReason}
              onChange={setReplanReason}
              draftKey={replanDraft.key}
              rows={3}
              maxAutoGrowChars={520}
              placeholder="例如：原计划遗漏了必要的数据迁移步骤，需要调整为……"
              actions={
                <div className="button-row">
                  <button
                    className="primary"
                    disabled={busy || !replanReason.trim()}
                    onClick={async () => {
                      const ok = await run(() =>
                        api.replanProject(project.id, replanReason),
                      );
                      if (!ok) return;
                      replanDraft.clear();
                      setReplanOpen(false);
                    }}
                  >
                    提交重规划
                  </button>
                  <button
                    className="secondary"
                    disabled={busy}
                    onClick={() => {
                      setReplanOpen(false);
                      replanDraft.clear();
                    }}
                  >
                    取消
                  </button>
                </div>
              }
            />
          </div>
        </section>
      ) : null}

      <ProjectTabs
        order={blockOrder}
        active={activeBlock}
        badges={{
          analysis: projectTabBadge("analysis", badgeInput),
          tasks: projectTabBadge("tasks", badgeInput),
          report: projectTabBadge("report", badgeInput),
        }}
        onSelect={(block) => selectTab(block)}
      />

      <section
        className="project-panel"
        role="tabpanel"
        id={tabPanelId(activeBlock)}
        aria-labelledby={tabButtonId(activeBlock)}
        tabIndex={0}
      >
        {activeBlock === "overview" ? (
          <ProjectOverview detail={detail} onOpenTab={openAnalysisTab} />
        ) : activeBlock === "analysis" ? (
          analysisBlock
        ) : activeBlock === "tasks" ? (
          tasksBlock
        ) : activeBlock === "report" ? (
          reportBlock
        ) : (
          infoBlock
        )}
      </section>

      <ProjectTopBlocks detail={detail} onRefresh={() => refresh()} />

      {selectedTask ? (
        <TaskDrawer
          key={selectedTask.id}
          task={selectedTask}
          detail={detail}
          onClose={() => selectTask(null)}
          onRetry={() =>
            run(async () => {
              await api.retryTask(project.id, selectedTask.id);
            })
          }
          onRetryJob={(jobId) => run(() => api.retryJob(jobId))}
          onSaveGuidance={(guidance) =>
            run(async () => {
              await api.saveTaskGuidance(project.id, selectedTask.id, guidance);
            })
          }
          onToggleMilestone={() =>
            run(async () => {
              await api.setTaskMilestone(
                project.id,
                selectedTask.id,
                !selectedTask.milestone,
              );
            })
          }
          onRefresh={() => refresh()}
          archived={archived}
          busy={busy}
          agentName={agentName}
        />
      ) : null}
    </div>
  );
}

function latestFailedJobId(jobs: JobSummary[]): string | null {
  const last = jobs[jobs.length - 1];
  return last?.status === "failed" ? last.id : null;
}

/// 实时区默认展示的消息条数；关键状态（失败与进行中的工具）另计。
export const LIVE_MESSAGE_LIMIT = 4;
const LIVE_ACTIVITY_REFRESH_MS = 1500;

export interface LiveActivityFeed {
  /// 最新一条可读活动，用于“正在做什么”的一句话摘要。
  headline: string | null;
  /// 最近的消息，最新在后。
  messages: ActivityDigestItem[];
  /// 最近的关键状态：失败与进行中的工具，最新在后。
  keys: ActivityDigestItem[];
  /// 未展开时被省略的条数；0 表示全部可见。
  hidden: number;
  /// 没有任何可读摘要时展示的等待文案。
  waiting: string | null;
}

/// 从活动摘要派生实时区内容：消息给读者看，失败与进行中的工具作为关键状态。
/// 纯派生，不发请求，不改变事件轮询频率。
export function liveActivityFeed(
  items: ActivityDigestItem[],
  expanded = false,
  limit: number = LIVE_MESSAGE_LIMIT,
): LiveActivityFeed {
  const messages = items.filter((entry) => entry.kind === "message");
  const keys = items.filter((entry) => entry.tone !== "normal");
  const all = expanded || limit <= 0;
  const visibleMessages = all
    ? messages
    : messages.slice(Math.max(0, messages.length - limit));
  const visibleKeys = all ? keys : keys.slice(Math.max(0, keys.length - limit));
  const shown = visibleMessages.length + visibleKeys.length;
  return {
    // 优先取最新消息；只有工具活动时退回到最新一条活动，避免误报“等待活动”。
    headline: messages.at(-1)?.text ?? items.at(-1)?.text ?? null,
    messages: visibleMessages,
    keys: visibleKeys,
    hidden: Math.max(0, items.length - shown),
    waiting: items.length === 0 ? ACTIVITY_WAITING_TEXT : null,
  };
}

/**
 * 项目分析运行中的实时区：展示 JobActivity 的可读摘要，而不是原始 stdout。
 * 原始输出收在默认收起的“原始输出”折叠里，展开才加载完整 stdout / stderr。
 */
function AnalysisLiveActivity({ jobId }: { jobId: string }) {
  const [activity, setActivity] = useState<JobActivity | null>(null);
  const [error, setError] = useState("");
  const [expanded, setExpanded] = useState(false);
  const [rawOpen, setRawOpen] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;

    async function tick() {
      try {
        const next = await api.getJobActivity(jobId);
        if (cancelled) return;
        setActivity(next);
        setError("");
      } catch (err) {
        if (cancelled) return;
        setError(errorMessage(err));
      }
      if (!cancelled) {
        timer = window.setTimeout(() => void tick(), LIVE_ACTIVITY_REFRESH_MS);
      }
    }

    void tick();
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [jobId]);

  const items = useMemo(
    () => activityDigest(activity, { limit: 0 }).items,
    [activity],
  );
  const feed = useMemo(() => liveActivityFeed(items, expanded), [items, expanded]);
  const summary = activity?.summary ?? null;

  return (
    <LiveActivitySummary
      feed={feed}
      summary={summary}
      loading={activity === null && !error}
      error={error}
      rawOpen={rawOpen}
      onRawToggle={(open) => setRawOpen(open)}
      onExpand={() => setExpanded(true)}
      jobId={jobId}
    />
  );
}

/**
 * 实时区的纯展示部分：可读摘要列表 + 默认收起的“原始输出”折叠。
 * 折叠未展开时不挂载 LiveOutput，DOM 里不会出现 stdout / stderr。
 */
export function LiveActivitySummary({
  feed,
  summary,
  loading,
  error,
  rawOpen,
  onRawToggle,
  onExpand,
  jobId,
}: {
  feed: LiveActivityFeed;
  summary: JobActivity["summary"] | null;
  loading: boolean;
  error: string;
  rawOpen: boolean;
  onRawToggle: (open: boolean) => void;
  onExpand: () => void;
  jobId: string;
}) {
  return (
    <div className="message message-planner live-message">
      <div className="message-role">规划者（实时进展）</div>
      <div className="live-activity">
        <div className="live-activity-head">
          {feed.headline ? (
            <div className="live-activity-headline">{feed.headline}</div>
          ) : (
            <div className="muted">{feed.waiting ?? ACTIVITY_WAITING_TEXT}</div>
          )}
          <span className="live-activity-meta muted">
            运行中 · 每 1.5 秒刷新
            {summary
              ? ` · 消息 ${summary.messages} · 工具 ${summary.tool_calls} · 失败 ${summary.tool_failures}`
              : ""}
          </span>
        </div>
        {loading ? <div className="muted">加载活动…</div> : null}
        {error ? <div className="error-box">读取活动失败：{error}</div> : null}
        {feed.messages.length > 0 || feed.keys.length > 0 ? (
          <ul className="live-activity-list">
            {feed.keys.map((entry) => (
              <li
                key={`key-${entry.index}`}
                className={`live-activity-item live-activity-${entry.tone}`}
              >
                <Badge tone={entry.tone === "failure" ? "danger" : "info"}>
                  {entry.tone === "failure" ? "失败" : "进行中"}
                </Badge>
                <span>{entry.text}</span>
              </li>
            ))}
            {feed.messages.map((entry) => (
              <li key={`msg-${entry.index}`} className="live-activity-item">
                <span className="live-activity-index">消息</span>
                <span>{entry.text}</span>
              </li>
            ))}
          </ul>
        ) : null}
        {feed.hidden > 0 ? (
          <div className="button-row live-activity-more">
            <button className="ghost" type="button" onClick={onExpand}>
              展开全部（还有 {feed.hidden} 条）
            </button>
          </div>
        ) : null}
        <details
          className="activity-raw live-activity-raw"
          onToggle={(event) => onRawToggle(event.currentTarget.open)}
        >
          <summary>原始输出</summary>
          <div className="activity-raw-body">
            {rawOpen ? (
              <LiveOutput jobId={jobId} running />
            ) : (
              <div className="muted">展开后加载完整 stdout / stderr。</div>
            )}
          </div>
        </details>
      </div>
    </div>
  );
}

/// 任务行的可调整队列控件：展示位置摘要，按钮使用明确文案，首尾按 queueEdge 禁用。
function QueueControls({
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

export function TaskDrawer({
  task,
  detail,
  onClose,
  onRetry,
  onRetryJob,
  onSaveGuidance,
  onToggleMilestone,
  onRefresh,
  archived,
  busy,
  agentName,
}: {
  task: Task;
  detail: ProjectDetail;
  onClose: () => void;
  onRetry: () => void;
  onRetryJob: (jobId: string) => void;
  onSaveGuidance: (guidance: string) => Promise<boolean>;
  onToggleMilestone: () => void;
  onRefresh: () => Promise<void>;
  archived: boolean;
  busy: boolean;
  agentName: (id: string | null) => string;
}) {
  const jobs = detail.jobs.filter((job) => job.task_id === task.id);
  const artifactsReleasing = task.artifacts_released_at === ARTIFACTS_RELEASING;
  const artifactsReleased = Boolean(task.artifacts_released_at);
  const archiveReference = task.branch
    ? `refs/domo-archive/heads/${task.branch}`
    : null;
  const archiveRecovery =
    task.branch && archiveReference
      ? `git branch ${task.branch} ${archiveReference}`
      : null;
  const retryableJobId =
    task.status === "blocked" && task.human_gate !== "milestone"
      ? latestFailedJobId(jobs)
      : null;
  const children = detail.tasks.filter((item) => item.parent_id === task.id);
  const parent = task.parent_id
    ? detail.tasks.find((item) => item.id === task.parent_id) ?? null
    : null;
  const childCompleted = children.filter(
    (item) => item.stage === "completed",
  ).length;
  const waitingChildren =
    children.length > 0 &&
    task.stage === "pending" &&
    childCompleted < children.length;
  const guidanceDraft = useDraftValue(
    { projectId: task.project_id, taskId: task.id, field: "guidance" },
    task.guidance,
  );
  const guidance = guidanceDraft.value;
  const setGuidance = guidanceDraft.setValue;
  const [controlBusy, setControlBusy] = useState(false);
  const [controlError, setControlError] = useState("");
  const guidanceLocked = archived || task.stage === "completed" || task.status === "canceled";
  const taskPaused = task.status === "paused" || task.status === "pausing";
  // 首区块按状态决定：blocked 与里程碑人工验收先裁决，已完成/已取消先给结果，其余进行中先给控制条。
  const layout = drawerLayout({ ...task, archived });
  const resultFirst = layout.lead === "result";
  const canControl =
    !archived && task.stage !== "completed" && task.status !== "canceled";
  const canReorder = isReordableQueueTask(task, archived);
  const edge = queueSummary(reordableQueueIds(detail.tasks, archived), task.id);
  const titleId = useId();
  const drawerRef = useRef<HTMLElement | null>(null);
  const closeRef = useRef<HTMLButtonElement | null>(null);
  // 打开时记录焦点来源，关闭后把焦点还给打开前的任务行或卡片。
  const returnFocusRef = useRef<HTMLElement | null>(null);
  useEffect(() => {
    const active = document.activeElement;
    returnFocusRef.current = active instanceof HTMLElement ? active : null;
    closeRef.current?.focus();
    return () => {
      const target = returnFocusRef.current;
      if (target?.isConnected) target.focus();
      returnFocusRef.current = null;
    };
  }, []);
  // 抽屉内 Tab 循环，Esc 关闭；焦点不会被背后的页面接走。
  function handleDrawerKeyDown(event: React.KeyboardEvent<HTMLElement>) {
    if (event.key === "Escape") {
      event.stopPropagation();
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const drawer = drawerRef.current;
    if (!drawer) return;
    const focusable = Array.from(
      drawer.querySelectorAll<HTMLElement>(
        'a[href], button, input, select, textarea, [tabindex]:not([tabindex="-1"])',
      ),
    ).filter(
      (element) =>
        !element.hasAttribute("disabled") &&
        element.getAttribute("aria-hidden") !== "true" &&
        element.tabIndex !== -1 &&
        element.getClientRects().length > 0,
    );
    if (focusable.length === 0) {
      event.preventDefault();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const current = document.activeElement;
    if (event.shiftKey) {
      if (current === first || !drawer.contains(current)) {
        event.preventDefault();
        last.focus();
      }
    } else if (current === last || !drawer.contains(current)) {
      event.preventDefault();
      first.focus();
    }
  }
  async function control(action: () => Promise<unknown>) {
    setControlBusy(true);
    setControlError("");
    try {
      await action();
      await onRefresh();
    } catch (err) {
      setControlError(errorMessage(err));
    } finally {
      setControlBusy(false);
    }
  }
  const resultSection = (
    <section className="task-result">
      <h3>结果摘要</h3>
      <RichContent content={task.result_summary} emptyText="没有记录执行摘要。" />
    </section>
  );
  const jobsSection = (
    <section>
      <h3>作业（{jobs.length}）</h3>
      <JobRecordList
        jobs={jobs}
        renderJob={(job) => (
          <JobDetailView
            key={job.id}
            job={job}
            retryable={job.id === retryableJobId}
            onRetry={() => onRetryJob(job.id)}
          />
        )}
      />
    </section>
  );
  const acceptanceSection = (
    <section className="task-acceptance">
      <h3>验收</h3>
      <h4>验收标准</h4>
      <ul>
        {task.acceptance.map((item) => (
          <li key={item}>{item}</li>
        ))}
      </ul>
      {task.verifier_issues.length > 0 ? (
        <>
          <h4>验收反馈</h4>
          <ul>
            {task.verifier_issues.map((issue) => (
              <li key={issue}>
                <RichContent content={issue} />
              </li>
            ))}
          </ul>
        </>
      ) : null}
    </section>
  );
  const descriptionSection = (
    <section>
      <h3>任务描述</h3>
      <RichContent content={task.description} />
    </section>
  );
  const deriveSection =
    parent || children.length > 0 ? (
      <section>
        <h3>派生关系</h3>
        {parent ? (
          <>
            <div className="kv">
              <span>父任务</span>
              <span>
                {parent.key} {parent.title}
              </span>
            </div>
            <p className="muted">
              派生任务跳过验收，执行成功后直接提交并合入父任务分支。
            </p>
          </>
        ) : null}
        {children.length > 0 ? (
          <>
            <div className="kv">
              <span>子任务</span>
              <span>
                {childCompleted}/{children.length} 已完成
                {waitingChildren ? "，父任务等待中" : ""}
              </span>
            </div>
            <div className="derive-list">
              {children.map((child) => (
                <div className="derive-child" key={child.id}>
                  <div className="derive-child-head">
                    <span className="task-key">{child.key}</span>
                    <span className="derive-child-title">{child.title}</span>
                    <TaskStateBadge stage={child.stage} status={child.status} />
                  </div>
                  <div className="muted">
                    <RichContent
                      content={child.result_summary || child.last_error}
                      emptyText="尚无结果"
                    />
                  </div>
                </div>
              ))}
            </div>
          </>
        ) : null}
      </section>
    ) : null;
  const depsSection = (
    <section>
      <h3>依赖与位置</h3>
      <div className="kv">
        <span>依赖</span>
        <span>{task.deps.length ? task.deps.join(", ") : "无"}</span>
      </div>
      <div className="kv">
        <span>分支</span>
        <span className="mono">{task.branch ?? "—"}</span>
      </div>
      <div className="kv">
        <span>工作区</span>
        <span className="badge-row">
          <span className="mono">
            {task.worktree ??
              (artifactsReleasing
                ? "回收中"
                : artifactsReleased
                  ? "已回收"
                  : "尚未创建")}
          </span>
          {task.worktree && artifactsReleased ? (
            <Badge
              tone="muted"
              title="工作区已回收，分支提交保留在 refs/domo-archive"
            >
              {artifactsReleasing ? "回收中" : "已回收"}
            </Badge>
          ) : null}
        </span>
      </div>
      {archiveReference && artifactsReleased && !artifactsReleasing ? (
        <div className="kv">
          <span>归档 ref</span>
          <span className="mono">{archiveReference}</span>
        </div>
      ) : null}
      {artifactsReleased && !artifactsReleasing && archiveRecovery ? (
        <div className="kv">
          <span>恢复命令</span>
          <span className="mono">{archiveRecovery}</span>
        </div>
      ) : null}
      <div className="kv">
        <span>排队位置</span>
        <span>
          {edge.position > 0
            ? `可调整队列第 ${edge.position}/${edge.total} 位`
            : task.queue_position !== null
              ? `派发队列第 ${task.queue_position} 位`
              : "未在队列中"}
        </span>
      </div>
      <div className="kv">
        <span>等待原因</span>
        <span>{task.waiting_reason ?? "—"}</span>
      </div>
      <div className="kv">
        <span>失败分类</span>
        <span>
          {task.last_failure_kind
            ? FAILURE_KIND_LABELS[task.last_failure_kind] ?? task.last_failure_kind
            : "—"}
        </span>
      </div>
      <div className="kv">
        <span>Agent</span>
        <span>{agentName(task.agent_id)}</span>
      </div>
    </section>
  );
  const rawErrorSection =
    rawErrorEntry(layout) === "collapsed" && task.last_error ? (
      <details className="raw-error drawer-fold" key="rawError">
        <summary>查看原始错误</summary>
        <div className="error-box">
          <RichContent content={task.last_error} />
        </div>
      </details>
    ) : null;
  const saveGuidance = async () => {
    const ok = await onSaveGuidance(guidance);
    if (ok) guidanceDraft.clear(guidance);
  };
  const guidanceEditor = (
    <section>
      <h3>人工指导</h3>
      <InputBox
        value={guidance}
        onChange={setGuidance}
        draftKey={guidanceDraft.key}
        rows={4}
        maxAutoGrowChars={520}
        disabled={guidanceLocked || busy}
        placeholder="指导会在该任务下一次执行作业时生效；保存后，执行者会带着这段说明开始下一次作业。"
        actions={
          <div className="button-row guidance-actions">
            <button
              className="primary"
              disabled={
                guidanceLocked ||
                busy ||
                !guidance.trim() ||
                guidance.trim() === task.guidance.trim()
              }
              onClick={() => void saveGuidance()}
            >
              保存指导
            </button>
            {guidanceLocked ? (
              <span className="muted">
                {task.status === "canceled"
                  ? "任务已取消，不能再编辑指导。"
                  : task.stage === "completed"
                    ? "任务已完成，不能再编辑指导。"
                    : "项目已归档，不能再编辑指导。"}
              </span>
            ) : null}
          </div>
        }
      />
    </section>
  );
  const guidanceReadonly = (
    <details className="drawer-fold">
      <summary>人工指导</summary>
      <RichContent content={task.guidance} emptyText="没有人工指导。" />
    </details>
  );
  const agentPoolSection = (
    <TaskAgentPoolPanel
      task={task}
      onRefresh={onRefresh}
      disabled={archived || task.stage === "completed" || task.status === "canceled"}
    />
  );
  const controlSection = canControl ? (
    <div className="task-control-bar">
      {taskPaused ? (
        <button
          className="secondary"
          disabled={controlBusy}
          onClick={() => void control(() => api.resumeTask(task.project_id, task.id))}
        >
          继续
        </button>
      ) : (
        <>
          <button
            className="secondary"
            disabled={controlBusy}
            onClick={() => void control(() => api.pauseTask(task.project_id, task.id))}
          >
            暂停
          </button>
          <button
            className="danger"
            disabled={controlBusy}
            onClick={() => {
              if (window.confirm("强制暂停会终止当前作业；之后点“继续”重新派发。确认继续？")) {
                void control(() => api.pauseTask(task.project_id, task.id, true));
              }
            }}
          >
            强制暂停
          </button>
        </>
      )}
      {canReorder ? (
        <>
          <button
            className="ghost"
            disabled={controlBusy || !edge.canMoveUp}
            aria-label={`${task.key} 排队上移`}
            title={edge.canMoveUp ? "上移一位" : "已经在可调整队列首位"}
            onClick={() =>
              void control(() => api.reorderTaskQueue(task.project_id, task.id, "up"))
            }
          >
            上移
          </button>
          <button
            className="ghost"
            disabled={controlBusy || !edge.canMoveDown}
            aria-label={`${task.key} 排队下移`}
            title={edge.canMoveDown ? "下移一位" : "已经在可调整队列末位"}
            onClick={() =>
              void control(() => api.reorderTaskQueue(task.project_id, task.id, "down"))
            }
          >
            下移
          </button>
        </>
      ) : null}
    </div>
  ) : null;
  const milestoneSection = (
    <div className="milestone-bar">
      <span className="muted">
        {task.milestone
          ? "验收者通过后不会自动合并，需要人工确认。"
          : "把任务标记为里程碑，验收通过后交给人工确认。"}
      </span>
      <button
        className={task.milestone ? "secondary" : "ghost"}
        disabled={
          archived ||
          busy ||
          task.stage === "completed" ||
          task.status === "canceled" ||
          task.human_gate === "milestone"
        }
        onClick={onToggleMilestone}
      >
        {task.milestone ? "取消里程碑" : "标记为里程碑"}
      </button>
    </div>
  );
  const gateSection =
    task.human_gate === "milestone" ? (
      <HumanAcceptancePanel
        task={task}
        projectId={task.project_id}
        onRefresh={onRefresh}
        disabled={archived}
      />
    ) : task.status === "blocked" ? (
      <ArbitrationPanel
        projectId={task.project_id}
        task={task}
        onRefresh={onRefresh}
        disabled={archived}
        onLegacyRetry={onRetry}
      />
    ) : null;
  // 首区块：gate 由裁决/人工验收面板承载，result 由结果摘要承载，control 由控制条承载。
  const controlBlock = (
    <section className="task-control" aria-label={DRAWER_LEAD_TITLES.control}>
      <h3>控制</h3>
      {controlSection ?? <p className="muted">该任务当前没有可用的控制操作。</p>}
      {controlError ? <div className="error-box">{controlError}</div> : null}
    </section>
  );
  const guidanceSection = guidanceLocked ? guidanceReadonly : guidanceEditor;
  const tailBlock = (key: DrawerTailSection) => {
    switch (key) {
      case "control":
        return <div key={key}>{controlBlock}</div>;
      case "milestone":
        return <div key={key}>{milestoneSection}</div>;
      case "result":
        return <div key={key}>{resultSection}</div>;
      case "acceptance":
        return <div key={key}>{acceptanceSection}</div>;
      case "description":
        return <div key={key}>{descriptionSection}</div>;
      case "derive":
        return deriveSection ? <div key={key}>{deriveSection}</div> : null;
      case "deps":
        return <div key={key}>{depsSection}</div>;
      case "rawError":
        return rawErrorSection;
      case "guidance":
        return <div key={key}>{guidanceSection}</div>;
      case "jobs":
        return <div key={key}>{jobsSection}</div>;
      case "agentPool":
        return (
          <section className="drawer-config" key={key}>
            <h3>配置</h3>
            {agentPoolSection}
          </section>
        );
    }
  };
  return (
    <div className="drawer-backdrop" onClick={onClose}>
      <aside
        className="drawer"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        data-drawer-lead={layout.lead}
        ref={drawerRef}
        onKeyDown={handleDrawerKeyDown}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="drawer-header">
          <div>
            <div className="muted">{task.key}</div>
            <h2 id={titleId}>{task.title}</h2>
          </div>
          <button className="ghost" ref={closeRef} onClick={onClose}>
            关闭
          </button>
        </div>
        <div className="drawer-body">
          <div className="badge-row">
            <TaskStateBadge stage={task.stage} status={task.status} />
            {task.attempts ? <Badge tone="warn">执行 {task.attempts} 次</Badge> : null}
            {task.rejections ? <Badge tone="warn">拒绝 {task.rejections} 次</Badge> : null}
            {!resultFirst && task.last_failure_kind ? (
              <Badge tone="danger">
                失败分类 ·{" "}
                {FAILURE_KIND_LABELS[task.last_failure_kind] ?? task.last_failure_kind}
              </Badge>
            ) : null}
            {!resultFirst && task.parent_id ? (
              <Badge tone="info">派生任务 · 跳过验收</Badge>
            ) : null}
            {!resultFirst && task.milestone ? (
              <Badge tone="warn">里程碑 · 需人工验收</Badge>
            ) : null}
            {!resultFirst && layout.gateCarriesError ? (
              <Badge tone="danger">等待人工验收</Badge>
            ) : null}
            {!resultFirst && waitingChildren ? (
              <Badge tone="warn">
                等待子任务 {childCompleted}/{children.length}
              </Badge>
            ) : null}
          </div>
          <div className="drawer-lead" data-lead={layout.lead}>
            {layout.lead === "gate" ? gateSection : null}
            {layout.lead === "result" ? resultSection : null}
            {layout.lead === "control" ? controlBlock : null}
          </div>
          {layout.tail.map((key) => tailBlock(key))}
        </div>
      </aside>
    </div>
  );
}

/** 任务 Agent 池：从项目池对应角色派生，复用项目池的 AgentPoolPanel 交互。 */
function TaskAgentPoolPanel({
  task,
  onRefresh,
  disabled,
}: {
  task: Task;
  onRefresh: () => Promise<void>;
  disabled: boolean;
}) {
  const [pool, setPool] = useState<TaskAgentPool | null>(null);
  const [loadError, setLoadError] = useState("");

  async function refreshPool() {
    try {
      const next = await api.getTaskAgentPool(task.project_id, task.id);
      setPool(next);
      setLoadError("");
    } catch (err) {
      setLoadError(errorMessage(err));
    }
  }

  useEffect(() => {
    setPool(null);
    void refreshPool();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [task.id, task.project_id]);

  if (!pool) {
    if (!loadError) return null;
    return <div className="error-box">{loadError}</div>;
  }
  return (
    <AgentPoolPanel
      key={task.id}
      title="任务 Agent 池"
      subtitle="从项目 Agent 池对应角色快照初始化，只作用于本任务。顺序越靠前优先级越高，按严格优先级派发，池不可用时不回退项目池。"
      roles={pool.roles}
      activeRole="executor"
      disabled={disabled}
      scope="task"
      onReorder={(role, agentId, position) =>
        api.updateTaskAgent(task.project_id, task.id, role, agentId, { position })
      }
      onMove={(role, agentId, direction) =>
        api.updateTaskAgent(task.project_id, task.id, role, agentId, { direction })
      }
      onToggle={(role, agentId, enabled) =>
        api.updateTaskAgent(task.project_id, task.id, role, agentId, { enabled })
      }
      onRefresh={async () => {
        await refreshPool();
        await onRefresh();
      }}
    />
  );
}

function HumanAcceptancePanel({
  task,
  projectId,
  onRefresh,
  disabled,
}: {
  task: Task;
  projectId: string;
  onRefresh: () => Promise<void>;
  disabled: boolean;
}) {
  const reasonDraft = useDraftValue(
    { projectId: task.project_id, taskId: task.id, field: "acceptance-reason" },
    "",
  );
  const guidanceDraft = useDraftValue(
    { projectId: task.project_id, taskId: task.id, field: "acceptance-guidance" },
    "",
  );
  const reason = reasonDraft.value;
  const guidance = guidanceDraft.value;
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  async function submit(action: "accept" | "retry" | "replan") {
    if (action !== "accept" && !reason.trim()) {
      setError("拒绝时必须填写理由。");
      return;
    }
    setBusy(true);
    setError("");
    try {
      if (action === "accept") {
        await api.acceptMilestone(projectId, task.id, reason);
      } else {
        await api.rejectMilestone(projectId, task.id, reason, guidance, action);
      }
      reasonDraft.clear();
      guidanceDraft.clear();
      await onRefresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="arbitration-panel">
      <h3>人工验收</h3>
      <div className="blocked-banner">
        <strong>验收者已通过，等待人工确认</strong>
        <RichContent
          content={task.last_error}
          emptyText="里程碑任务必须由人工确认。"
        />
      </div>
      <InputBox
        value={reason}
        onChange={reasonDraft.setValue}
        draftKey={reasonDraft.key}
        rows={3}
        disabled={disabled || busy}
        placeholder="通过意见（可选）；拒绝时必须填写理由。"
      />
      <InputBox
        value={guidance}
        onChange={guidanceDraft.setValue}
        draftKey={guidanceDraft.key}
        rows={3}
        disabled={disabled || busy}
        placeholder="拒绝时可选：给执行者或规划者的补充指导。"
        actions={
          <div className="button-row">
            <button
              className="primary"
              disabled={disabled || busy}
              onClick={() => void submit("accept")}
            >
              人工通过并合并
            </button>
            <button
              className="secondary"
              disabled={disabled || busy}
              onClick={() => void submit("retry")}
            >
              拒绝并退回执行
            </button>
            <button
              className="secondary"
              disabled={disabled || busy}
              onClick={() => void submit("replan")}
            >
              拒绝并要求重新规划
            </button>
          </div>
        }
      />
      <small>
        退回执行会把验收意见和指导交给执行者；要求重新规划会作废未完成任务，并把意见交给规划者。
      </small>
      {error ? <div className="error-box">{error}</div> : null}
    </section>
  );
}

function ArbitrationPanel({
  projectId,
  task,
  onRefresh,
  disabled,
  onLegacyRetry,
}: {
  projectId: string;
  task: Task;
  onRefresh: () => Promise<void>;
  disabled: boolean;
  onLegacyRetry: () => void;
}) {
  const [context, setContext] = useState<ArbitrationContext | null>(null);
  const [contextError, setContextError] = useState("");
  const [agentId, setAgentId] = useState("");
  const guidanceDraft = useDraftValue(
    { projectId, taskId: task.id, field: "arbitration-guidance" },
    "",
  );
  const guidance = guidanceDraft.value;
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let active = true;
    setContextError("");
    api
      .getTaskArbitration(projectId, task.id)
      .then((next) => {
        if (!active) return;
        setContext(next);
        setAgentId((current) => current || next.agents[0]?.id || "");
      })
      .catch((err) => {
        if (active) setContextError(errorMessage(err));
      });
    return () => {
      active = false;
    };
  }, [projectId, task.id, task.status, task.stage, task.human_gate]);

  async function submit(input: {
    action: "retry" | "switch_agent" | "guide";
    agent_id?: string;
    guidance?: string;
  }) {
    setBusy(true);
    setError("");
    try {
      await api.submitTaskArbitration(projectId, task.id, input);
      guidanceDraft.clear();
      await onRefresh();
      const next = await api.getTaskArbitration(projectId, task.id);
      setContext(next);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  const roleLabel = context?.role === "verifier" ? "验收" : "执行";

  return (
    <section className="arbitration-panel">
      <h3>人工裁决</h3>
      {contextError ? (
        <>
          <div className="error-box">{contextError}</div>
          <button className="primary" disabled={disabled || busy} onClick={onLegacyRetry}>
            重试此任务
          </button>
        </>
      ) : null}
      {context ? (
        <>
          <div className="kv">
            <span>阶段</span>
            <span>
              {taskStateLabel(context.task.stage, context.task.status)}
            </span>
          </div>
          <div className="kv">
            <span>失败原因</span>
            <div>
              {context.task.failure_summary ? (
                <div className="failure-summary">
                  <div className="failure-summary-head">
                    <Badge tone="danger">
                      {failureSummaryLabel(context.task.failure_summary.kind)}
                    </Badge>
                    <span>{shortText(context.task.failure_summary.message, 160)}</span>
                  </div>
                </div>
              ) : null}
              {context.task.last_error ? (
                <details className="raw-error">
                  <summary>查看原始错误</summary>
                  <div className="error-box">
                    <RichContent content={context.task.last_error} />
                  </div>
                </details>
              ) : (
                <span className="muted">—</span>
              )}
            </div>
          </div>
          <div className="kv">
            <span>失败统计</span>
            <span>
              执行 {context.task.attempts} 次 · 验收拒绝 {context.task.rejections} 次
            </span>
          </div>
          <div className="kv">
            <span>对应角色</span>
            <span>{roleLabel}</span>
          </div>
          {context.jobs.length > 0 ? (
            <div className="arbitration-jobs">
              {context.jobs.slice(-4).map((job) => (
                <div className="arbitration-job" key={job.id}>
                  <div className="muted">
                    {ROLE_LABELS[job.role] ?? job.role} · {job.agent_name ?? "—"} ·{" "}
                    {jobStatusLabel(job.status)}
                    {job.failure_summary
                      ? ` · ${failureSummaryLabel(job.failure_summary.kind)} · ${shortText(
                          job.failure_summary.message,
                          100,
                        )}`
                      : job.error
                        ? " · 失败 · 摘要不可用"
                        : ""}
                  </div>
                  {job.error ? (
                    <details className="raw-error">
                      <summary>查看原始错误</summary>
                      <div className="error-box">
                        <RichContent content={job.error} />
                      </div>
                    </details>
                  ) : null}
                </div>
              ))}
            </div>
          ) : null}
          <div className="button-row">
            <button
              className="primary"
              disabled={disabled || busy}
              onClick={() => void submit({ action: "retry" })}
            >
              重试
            </button>
          </div>
          <div className="arbitration-switch">
            <select
              value={agentId}
              onChange={(event) => setAgentId(event.target.value)}
              disabled={disabled || busy || context.agents.length === 0}
            >
              {context.agents.map((agent) => (
                <option key={agent.id} value={agent.id}>
                  {agent.name}
                  {agent.enabled ? "" : "（已停用）"}
                  {agent.cooldown_until ? "（冷却中）" : ""}
                </option>
              ))}
            </select>
            <button
              className="secondary"
              disabled={disabled || busy || !agentId}
              onClick={() =>
                void submit({ action: "switch_agent", agent_id: agentId })
              }
            >
              换 Agent 执行
            </button>
          </div>
          <InputBox
            value={guidance}
            onChange={guidanceDraft.setValue}
            draftKey={guidanceDraft.key}
            rows={3}
            disabled={disabled || busy}
            placeholder="补充指导：说明下一步应如何处理；提交后任务重新排队，等待派发新作业。"
            actions={
              <button
                className="secondary"
                disabled={disabled || busy || !guidance.trim()}
                onClick={() =>
                  void submit({ action: "guide", guidance: guidance.trim() })
                }
              >
                记录指导并重新排队
              </button>
            }
          />
        </>
      ) : null}
      {error ? <div className="error-box">{error}</div> : null}
    </section>
  );
}
