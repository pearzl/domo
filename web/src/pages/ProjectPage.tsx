import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";
import type {
  ArbitrationContext,
  Agent,
  ProjectDetail,
  ProjectUsage,
  Role,
  Task,
  TaskAgentPool,
  UsageGroup,
} from "../types";
import { api, errorMessage, subscribeEvents } from "../api";
import { AgentPoolPanel } from "../components/AgentPoolPanel";
import { JobDetailView } from "../components/JobDetailView";
import { LiveOutput } from "../components/LiveOutput";
import { RichContent } from "../components/RichContent";
import { TaskDag } from "../components/TaskDag";
import {
  Badge,
  Empty,
  ErrorBox,
  FAILURE_KIND_LABELS,
  Progress,
  ROLE_LABELS,
  StageBadge,
  StatusBadge,
  TASK_STAGE_LABELS,
  TaskStateBadge,
  formatCost,
  formatMillis as formatUsageDuration,
  formatTime,
  formatTokens as formatUsageAmount,
} from "../ui";

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
  const [message, setMessage] = useState("");
  const [feedback, setFeedback] = useState("");
  const [busy, setBusy] = useState(false);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [replanOpen, setReplanOpen] = useState(false);
  const [replanReason, setReplanReason] = useState("");
  const [addTaskOpen, setAddTaskOpen] = useState(false);
  const [taskKey, setTaskKey] = useState("");
  const [taskTitle, setTaskTitle] = useState("");
  const [taskDescription, setTaskDescription] = useState("");
  const [taskAcceptance, setTaskAcceptance] = useState("");
  const [taskDeps, setTaskDeps] = useState<string[]>([]);
  const [taskMilestone, setTaskMilestone] = useState(false);
  /// 任务面板视图只在页面内保持，不写入任务数据。
  const [taskView, setTaskView] = useState<"list" | "dag">("list");

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

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError("");
    try {
      await action();
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

  const { project, tasks, jobs, messages, events, agents, goal_proposal } = detail;
  const activeTasks = tasks.filter((task) => task.status !== "canceled");
  const completed = activeTasks.filter((task) => task.stage === "completed").length;
  const archived = project.status === "archived";
  const paused = project.status === "paused" || project.status === "pausing";
  const waitingReport = project.stage === "summarizing" && project.report;
  const goalMessages = messages.filter((item) => item.role !== "review");
  const reviewMessages = messages.filter((item) => item.role === "review");
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
  const confirmJobs = jobs.filter(
    (job) => job.role === "confirm" && !job.task_id,
  );
  const confirmJob =
    confirmJobs.find((job) => job.status === "running") ?? null;
  const latestConfirmJob = confirmJobs.length
    ? confirmJobs[confirmJobs.length - 1]
    : null;
  const directJob = jobs.find(
    (job) => job.role === "direct" && job.status === "running",
  );
  const directMode = project.completion_mode === "direct";
  const lastGoalMessage = goalMessages[goalMessages.length - 1];
  const waitingGoal =
    project.stage === "goal_confirm" &&
    !confirmJob &&
    lastGoalMessage?.role === "planner";
  const canReorder = (task: Task) =>
    !archived &&
    task.stage === "pending" &&
    task.attempts === 0 &&
    task.rejections === 0 &&
    task.status !== "canceled";

  async function handleAddTask(event: FormEvent) {
    event.preventDefault();
    await run(async () => {
      await api.createTask(project.id, {
        key: taskKey.trim(),
        title: taskTitle.trim(),
        description: taskDescription.trim(),
        acceptance: taskAcceptance
          .split("\n")
          .map((item) => item.trim())
          .filter(Boolean),
        deps: taskDeps,
        milestone: taskMilestone,
      });
      setTaskKey("");
      setTaskTitle("");
      setTaskDescription("");
      setTaskAcceptance("");
      setTaskDeps([]);
      setTaskMilestone(false);
      setAddTaskOpen(false);
    });
  }

  return (
    <div className="page">
      <div className="back-row">
        <button className="ghost" onClick={onBack}>
          ← 返回项目
        </button>
      </div>

      <header className="project-header panel">
        <div className="project-heading">
          <div>
            <h1>{project.name}</h1>
            <div className="badge-row">
              <StageBadge stage={project.stage} />
              <StatusBadge status={project.status} />
              {project.status === "blocked" ? <Badge tone="danger">需要处理</Badge> : null}
            </div>
          </div>
          <div className="header-actions">
            {!archived && project.stage !== "completed" ? (
              paused ? (
                <button
                  className="secondary"
                  disabled={busy}
                  onClick={() => run(() => api.resumeProject(project.id))}
                >
                  恢复
                </button>
              ) : (
                <>
                  <button
                    className="secondary"
                    disabled={busy}
                    onClick={() => run(() => api.pauseProject(project.id))}
                  >
                    暂停
                  </button>
                  <button
                    className="danger"
                    disabled={busy}
                    onClick={() => {
                      if (
                        window.confirm(
                          "强制暂停会立即终止所有在途作业，任务进入已暂停。确认继续？",
                        )
                      ) {
                        void run(() => api.pauseProject(project.id, true));
                      }
                    }}
                  >
                    强制暂停
                  </button>
                </>
              )
            ) : null}
            {!archived &&
            project.status === "blocked" &&
            project.stage !== "completed" &&
            !waitingReport &&
            !waitingGoal ? (
              <button
                className="secondary"
                disabled={busy}
                onClick={() => run(() => api.retryProject(project.id))}
              >
                重试项目
              </button>
            ) : null}
            {canReplan ? (
              <button
                className="secondary"
                disabled={busy}
                onClick={() => setReplanOpen((value) => !value)}
              >
                重新规划
              </button>
            ) : null}
            {!archived ? (
              <button
                className="danger"
                disabled={busy}
                onClick={() => {
                  if (window.confirm("归档后不会继续推进，也不能恢复。确认归档？")) {
                    void run(() => api.archiveProject(project.id));
                  }
                }}
              >
                归档
              </button>
            ) : null}
          </div>
        </div>
        <p className="goal-line goal-large">{project.goal}</p>
        <div className="meta-grid">
          <div>
            <span className="meta-label">仓库</span>
            <span className="mono">{project.repo_path}</span>
          </div>
          <div>
            <span className="meta-label">交付分支</span>
            <span className="mono">
              {project.base_branch} → {project.branch}
            </span>
          </div>
          <div>
            <span className="meta-label">工作区</span>
            <span className="mono">{project.workspace}</span>
          </div>
          <div>
            <span className="meta-label">进度</span>
            <Progress total={activeTasks.length} completed={completed} />
          </div>
        </div>
        {project.blocked_reason ? (
          <div className="blocked-banner">
            <strong>{project.status === "blocked" ? "阻塞原因" : "提示"}</strong>
            <RichContent content={project.blocked_reason} />
          </div>
        ) : null}
        {project.stage === "completed" ? (
          <div className="success-banner">
            项目已完成。改动位于 <span className="mono">{project.branch}</span>，请审阅后手动合入{" "}
            <span className="mono">{project.base_branch}</span>。
          </div>
        ) : null}
      </header>

      <AgentPoolPanel
        title="项目 Agent 池"
        subtitle="每个角色按顺序选择已启用的 Agent，顺序越靠前优先级越高。项目池在创建时继承默认团队配置，之后独立维护。"
        roles={detail.agent_pool.roles}
        disabled={archived}
        onReorder={(role, agentId, position) =>
          api.updateProjectAgent(project.id, role, agentId, { position })
        }
        onMove={(role, agentId, direction) =>
          api.updateProjectAgent(project.id, role, agentId, { direction })
        }
        onToggle={(role, agentId, enabled) =>
          api.updateProjectAgent(project.id, role, agentId, { enabled })
        }
        onRefresh={() => refresh()}
      />

      <UsagePanel usage={detail.usage} agents={agents} />

      {error ? <ErrorBox message={error} /> : null}

      {replanOpen && canReplan ? (
        <section className="panel">
          <h2>重新规划</h2>
          <p className="subtitle">
            已完成任务会保留；未完成任务会退出当前 DAG 并停止派发。请说明需要重新规划的原因。
          </p>
          <div className="confirm-row">
            <textarea
              value={replanReason}
              onChange={(event) => setReplanReason(event.target.value)}
              rows={3}
              placeholder="例如：原计划遗漏了必要的数据迁移步骤，需要调整为……"
            />
            <div className="button-row">
              <button
                className="primary"
                disabled={busy || !replanReason.trim()}
                onClick={() =>
                  run(async () => {
                    await api.replanProject(project.id, replanReason);
                    setReplanReason("");
                    setReplanOpen(false);
                  })
                }
              >
                提交重规划
              </button>
              <button
                className="secondary"
                disabled={busy}
                onClick={() => {
                  setReplanOpen(false);
                  setReplanReason("");
                }}
              >
                取消
              </button>
            </div>
          </div>
        </section>
      ) : null}

      {project.replan_reason || replanEvents.length > 0 ? (
        <section className="panel">
          <h2>重规划记录</h2>
          {project.replan_reason ? (
            <div className="blocked-banner">
              <strong>最近一次重规划（来源：{replanSourceLabel(project.replan_source)}）</strong>
              <RichContent content={project.replan_reason} />
              {project.replan_requested_at ? (
                <div className="muted">{formatTime(project.replan_requested_at)}</div>
              ) : null}
            </div>
          ) : null}
          <div className="events">
            {[...replanEvents].reverse().map((event) => (
              <div className="event" key={event.id}>
                <Badge tone={eventTone(event.kind)}>{event.kind}</Badge>
                <span className="event-message">{event.message}</span>
                <span className="event-time muted">{formatTime(event.created_at)}</span>
              </div>
            ))}
          </div>
        </section>
      ) : null}

      {project.stage === "goal_confirm" ? (
        <section className="panel">
          <h2>目标确认</h2>
          <p className="subtitle">
            规划者会分析目标、提出问题并给出自己的理解。你可以多轮补充，确认后再开始规划。
          </p>
          <label className="goal-policy">
            <input
              type="checkbox"
              checked={project.goal_confirm_mode === "auto"}
              disabled={busy || archived || paused || !!confirmJob}
              onChange={(event) =>
                run(() =>
                  api.updateGoalPolicy(
                    project.id,
                    event.target.checked ? "auto" : "manual",
                  ),
                )
              }
            />
            <span>
              自动确认：目标足够清晰时允许 AI 跳过人工确认，直接开始规划或直接完成
            </span>
          </label>
          <div className="messages">
            {goalMessages.map((item) => (
              <div key={item.id} className={`message message-${item.role}`}>
                <div className="message-role">{item.role === "user" ? "你" : "规划者"}</div>
                <RichContent content={item.content} className="message-content" />
              </div>
            ))}
            {goalMessages.length === 0 ? (
              <div className="muted">还没有输入项目目标。</div>
            ) : null}
            {latestConfirmJob ? (
              <div className="message message-planner live-message">
                <div className="message-role">规划者（实时输出）</div>
                <LiveOutput
                  jobId={latestConfirmJob.id}
                  running={latestConfirmJob.status === "running"}
                />
              </div>
            ) : null}
          </div>
          {confirmJob ? (
            <div className="goal-running">
              <Badge tone="info">规划者正在分析</Badge>
              <span className="muted">
                {confirmJob.agent_name ?? "规划 Agent"} ·{" "}
                {formatTime(confirmJob.started_at)}
              </span>
            </div>
          ) : null}
          {goal_proposal ? (
            <div className="goal-proposal">
              <div className="goal-proposal-head">
                <Badge tone={goal_proposal.kind === "direct" ? "warn" : "success"}>
                  {goal_proposal.kind === "direct" ? "建议直接完成" : "目标已明确"}
                </Badge>
                <div className="goal-proposal-summary">
                  <RichContent content={goal_proposal.summary} />
                </div>
              </div>
              {goal_proposal.goal ? (
                <div>
                  <strong>确认后的目标：</strong>
                  <RichContent content={goal_proposal.goal} />
                </div>
              ) : null}
              {goal_proposal.plan.length ? (
                <div>
                  <strong>执行步骤</strong>
                  <ul>
                    {goal_proposal.plan.map((item) => (
                      <li key={item}>{item}</li>
                    ))}
                  </ul>
                </div>
              ) : null}
              {goal_proposal.verification.length ? (
                <div>
                  <strong>自检方式</strong>
                  <ul>
                    {goal_proposal.verification.map((item) => (
                      <li key={item}>{item}</li>
                    ))}
                  </ul>
                </div>
              ) : null}
            </div>
          ) : null}
          {!confirmJob ? (
            <div className="confirm-row">
              <textarea
                value={message}
                onChange={(event) => setMessage(event.target.value)}
                disabled={busy || paused || archived}
                rows={3}
                placeholder={
                  goalMessages.length === 0
                    ? "描述项目目标、边界和验收预期。"
                    : "补充信息或回答规划者的问题。"
                }
              />
              <div className="button-row">
                <button
                  className="secondary"
                  disabled={busy || paused || archived || !message.trim()}
                  onClick={() =>
                    run(async () => {
                      await api.postGoalMessage(
                        project.id,
                        message,
                        project.goal_confirm_mode,
                      );
                      setMessage("");
                    })
                  }
                >
                  {goalMessages.length === 0 ? "开始讨论" : "发送"}
                </button>
                {goal_proposal ? (
                  <button
                    className="primary"
                    disabled={
                      busy ||
                      paused ||
                      archived ||
                      (goal_proposal.kind === "direct" && activeTasks.length > 0)
                    }
                    title={
                      goal_proposal.kind === "direct" && activeTasks.length > 0
                        ? "项目已有任务，不能直接完成"
                        : undefined
                    }
                    onClick={() =>
                      run(async () => {
                        await api.confirmGoal(project.id, message || undefined);
                        setMessage("");
                      })
                    }
                  >
                    {goal_proposal.kind === "direct"
                      ? "确认并直接完成"
                      : "确认目标，开始规划"}
                  </button>
                ) : null}
              </div>
            </div>
          ) : null}
        </section>
      ) : goalMessages.length > 1 ? (
        <details className="panel">
          <summary>目标确认记录</summary>
          <div className="messages compact">
            {goalMessages.map((item) => (
              <div key={item.id} className={`message message-${item.role}`}>
                <div className="message-role">{item.role === "user" ? "你" : "规划者"}</div>
                <RichContent content={item.content} className="message-content" />
              </div>
            ))}
          </div>
        </details>
      ) : null}

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
            {!archived && !directMode && goal_proposal?.kind !== "direct" ? (
              <button
                className="secondary"
                disabled={busy}
                onClick={() => setAddTaskOpen((value) => !value)}
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
        {addTaskOpen && !archived && !directMode && goal_proposal?.kind !== "direct" ? (
          <form className="form add-task-form" onSubmit={handleAddTask}>
            <div className="form-grid">
              <label>
                <span>任务 key</span>
                <input
                  value={taskKey}
                  onChange={(event) => setTaskKey(event.target.value)}
                  placeholder="例如：T3"
                  required
                />
              </label>
              <label>
                <span>标题</span>
                <input
                  value={taskTitle}
                  onChange={(event) => setTaskTitle(event.target.value)}
                  placeholder="一句话说明这个任务做什么"
                  required
                />
              </label>
            </div>
            <label>
              <span>描述</span>
              <textarea
                value={taskDescription}
                onChange={(event) => setTaskDescription(event.target.value)}
                rows={3}
                placeholder="任务目标、边界和实现要点。"
                required
              />
            </label>
            <label>
              <span>验收标准（每行一条，留空使用默认标准）</span>
              <textarea
                value={taskAcceptance}
                onChange={(event) => setTaskAcceptance(event.target.value)}
                rows={3}
                placeholder={"例如：\ndomo_mock/T3.txt 存在且包含 T3"}
              />
            </label>
            <div className="field">
              <span className="field-label">前置任务（依赖）</span>
              {activeTasks.length === 0 ? (
                <span className="muted">项目还没有可依赖的任务。</span>
              ) : (
                <div className="checkbox-row">
                  {activeTasks.map((task) => (
                    <label className="checkbox-label" key={task.id}>
                      <input
                        type="checkbox"
                        checked={taskDeps.includes(task.key)}
                        onChange={(event) => {
                          setTaskDeps((current) =>
                            event.target.checked
                              ? [...current, task.key]
                              : current.filter((dep) => dep !== task.key),
                          );
                        }}
                      />
                      <span>
                        {task.key} {task.title}
                      </span>
                    </label>
                  ))}
                </div>
              )}
            </div>
            <label className="checkbox-label">
              <input
                type="checkbox"
                checked={taskMilestone}
                onChange={(event) => setTaskMilestone(event.target.checked)}
              />
              <span>标记为里程碑（验收通过后需人工确认）</span>
            </label>
            <div className="form-actions">
              <button
                className="primary"
                type="submit"
                disabled={
                  busy ||
                  !taskKey.trim() ||
                  !taskTitle.trim() ||
                  !taskDescription.trim()
                }
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
          <Empty title="还没有任务" hint="确认目标后，规划者会输出带依赖关系的任务列表。" />
        ) : taskView === "dag" ? (
          <TaskDag
            tasks={tasks}
            selectedTaskId={selectedTaskId}
            onSelect={setSelectedTaskId}
          />
        ) : (
          <div className="task-list">
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
              return (
                <div
                  key={task.id}
                  className={`task-row ${task.id === selectedTaskId ? "selected" : ""}`}
                  role="button"
                  tabIndex={0}
                  onClick={() => setSelectedTaskId(task.id)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      setSelectedTaskId(task.id);
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
                    {canReorder(task) ? (
                      <span className="queue-controls">
                        <button
                          className="ghost"
                          title="排队上移"
                          disabled={busy || task.queue_position === 1}
                          onClick={(event) => {
                            event.stopPropagation();
                            void run(() =>
                              api.reorderTaskQueue(project.id, task.id, "up"),
                            );
                          }}
                        >
                          ↑
                        </button>
                        <button
                          className="ghost"
                          title="排队下移"
                          disabled={busy}
                          onClick={(event) => {
                            event.stopPropagation();
                            void run(() =>
                              api.reorderTaskQueue(project.id, task.id, "down"),
                            );
                          }}
                        >
                          ↓
                        </button>
                      </span>
                    ) : null}
                  </span>
                </div>
              );
            })}
          </div>
        )}
        {!directMode && tasks.length > 0 && taskView === "list" ? (
          <div className="legend muted">
            阶段：待启动 → 执行 → 验收 → 完成。执行与验收都在独立 worktree 中进行；派生任务跳过验收，完成后直接合入父任务分支。
          </div>
        ) : null}
      </section>

      {projectJobs.length > 0 ? (
        <section className="panel">
          <h2>目标确认 / 规划 / 总结作业</h2>
          <div className="jobs">
            {projectJobs.map((job) => (
              <JobDetailView key={job.id} job={job} />
            ))}
          </div>
        </section>
      ) : null}

      {project.stage === "summarizing" || project.report ? (
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
              <p className="muted">报告生成失败，可点击上方“重试项目”。</p>
            ) : (
              <p className="muted">规划者正在依据评议意见重写报告…</p>
            )
          ) : null}
          {project.stage === "summarizing" ? (
            <div className="report-actions">
              <textarea
                value={feedback}
                onChange={(event) => setFeedback(event.target.value)}
                rows={3}
                placeholder="对报告的意见（提交意见可供多轮讨论；要求重写或重新规划时必须填写）。"
              />
              <div className="button-row">
                <button
                  className="primary"
                  disabled={busy || !project.report}
                  onClick={() => run(() => api.submitReport(project.id, "accept"))}
                >
                  通过并完成
                </button>
                <button
                  className="secondary"
                  disabled={busy || !feedback.trim()}
                  onClick={() =>
                    run(async () => {
                      await api.submitReport(project.id, "comment", feedback);
                      setFeedback("");
                    })
                  }
                >
                  提交意见
                </button>
                <button
                  className="secondary"
                  disabled={busy || !project.report || !feedback.trim()}
                  onClick={() =>
                    run(async () => {
                      await api.submitReport(project.id, "reject", feedback);
                      setFeedback("");
                    })
                  }
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
                  onClick={() =>
                    run(async () => {
                      await api.submitReport(project.id, "replan", feedback);
                      setFeedback("");
                    })
                  }
                  title="记录意见并要求重新规划，项目回到规划阶段"
                >
                  要求重新规划
                </button>
                {project.completion_mode !== "direct" ? (
                  <button
                    className="secondary"
                    disabled={busy || archived}
                    onClick={() => setAddTaskOpen(true)}
                    title="通过添加任务表单增加新任务；添加后项目回到执行阶段，原报告作废"
                  >
                    增加新任务
                  </button>
                ) : null}
              </div>
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
      ) : null}

      <section className="panel">
        <details>
          <summary>活动与审计（{events.length}）</summary>
          <div className="events">
            {[...events].reverse().map((event) => (
              <div className="event" key={event.id}>
                <Badge tone={eventTone(event.kind)}>{event.kind}</Badge>
                <span className="event-message">{event.message}</span>
                <span className="event-time muted">{formatTime(event.created_at)}</span>
              </div>
            ))}
          </div>
        </details>
      </section>

      {selectedTask ? (
        <TaskDrawer
          task={selectedTask}
          detail={detail}
          onClose={() => setSelectedTaskId(null)}
          onRetry={() =>
            run(async () => {
              await api.retryTask(project.id, selectedTask.id);
            })
          }
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

function UsagePanel({
  usage,
  agents,
}: {
  usage: ProjectUsage;
  agents: ProjectDetail["agents"];
}) {
  const groupName = (group: UsageGroup) => {
    if (group.agent_id) {
      return (
        group.agent_name ??
        agents.find((agent) => agent.id === group.agent_id)?.name ??
        group.agent_id
      );
    }
    if (group.role) return ROLE_LABELS[group.role] ?? group.role;
    return "未记录";
  };
  return (
    <details className="panel usage-panel">
      <summary>项目用量（{usage.totals.jobs} 个作业）</summary>
      <div className="usage-cards">
        <div>
          <span className="meta-label">输入 token</span>
          <strong>{formatUsageAmount(usage.totals.input_tokens)}</strong>
        </div>
        <div>
          <span className="meta-label">输出 token</span>
          <strong>{formatUsageAmount(usage.totals.output_tokens)}</strong>
        </div>
        <div>
          <span className="meta-label">费用</span>
          <strong>{formatCost(usage.totals.cost)}</strong>
        </div>
        <div>
          <span className="meta-label">时长</span>
          <strong>{formatUsageDuration(usage.totals.duration_ms)}</strong>
        </div>
        <div>
          <span className="meta-label">未知用量</span>
          <strong>{usage.totals.unknown_jobs} 个作业</strong>
        </div>
      </div>
      {usage.totals.unknown_jobs > 0 ? (
        <div className="warn-banner">
          {usage.totals.unknown_jobs} 个作业未提供 token 或费用，已单独计数，不参与限额扣减。
        </div>
      ) : null}
      <div className="usage-tables">
        <UsageTable title="按 Agent" groups={usage.by_agent} nameOf={groupName} />
        <UsageTable title="按角色" groups={usage.by_role} nameOf={groupName} />
      </div>
    </details>
  );
}

function UsageTable({
  title,
  groups,
  nameOf,
}: {
  title: string;
  groups: UsageGroup[];
  nameOf: (group: UsageGroup) => string;
}) {
  return (
    <div>
      <h3>{title}</h3>
      {groups.length === 0 ? (
        <div className="muted">还没有用量记录。</div>
      ) : (
        <table className="usage-table">
          <thead>
            <tr>
              <th>名称</th>
              <th>作业</th>
              <th>输入</th>
              <th>输出</th>
              <th>费用</th>
              <th>时长</th>
              <th>未知</th>
            </tr>
          </thead>
          <tbody>
            {groups.map((group) => (
              <tr key={group.key}>
                <td>{nameOf(group)}</td>
                <td>{group.jobs}</td>
                <td>{formatUsageAmount(group.input_tokens)}</td>
                <td>{formatUsageAmount(group.output_tokens)}</td>
                <td>{formatCost(group.cost)}</td>
                <td>{formatUsageDuration(group.duration_ms)}</td>
                <td>{group.unknown_jobs}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

function TaskDrawer({
  task,
  detail,
  onClose,
  onRetry,
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
  onSaveGuidance: (guidance: string) => void;
  onToggleMilestone: () => void;
  onRefresh: () => Promise<void>;
  archived: boolean;
  busy: boolean;
  agentName: (id: string | null) => string;
}) {
  const jobs = detail.jobs.filter((job) => job.task_id === task.id);
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
  const [guidance, setGuidance] = useState(task.guidance);
  const [controlBusy, setControlBusy] = useState(false);
  const [controlError, setControlError] = useState("");
  useEffect(() => {
    setGuidance(task.guidance);
  }, [task.id, task.guidance]);
  const guidanceLocked = archived || task.stage === "completed" || task.status === "canceled";
  const taskPaused = task.status === "paused" || task.status === "pausing";
  const canControl =
    !archived && task.stage !== "completed" && task.status !== "canceled";
  const canReorder =
    canControl &&
    task.stage === "pending" &&
    task.attempts === 0 &&
    task.rejections === 0;
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
  return (
    <div className="drawer-backdrop" onClick={onClose}>
      <aside className="drawer" onClick={(event) => event.stopPropagation()}>
        <div className="drawer-header">
          <div>
            <div className="muted">{task.key}</div>
            <h2>{task.title}</h2>
          </div>
          <button className="ghost" onClick={onClose}>
            关闭
          </button>
        </div>
        <div className="badge-row">
          <TaskStateBadge stage={task.stage} status={task.status} />
          <Badge tone="muted">{TASK_STAGE_LABELS[task.stage] ?? task.stage}</Badge>
          {task.attempts ? <Badge tone="warn">执行 {task.attempts} 次</Badge> : null}
          {task.rejections ? <Badge tone="warn">拒绝 {task.rejections} 次</Badge> : null}
          {task.last_failure_kind ? (
            <Badge tone="danger">
              失败分类 ·{" "}
              {FAILURE_KIND_LABELS[task.last_failure_kind] ?? task.last_failure_kind}
            </Badge>
          ) : null}
          {task.parent_id ? <Badge tone="info">派生任务 · 跳过验收</Badge> : null}
          {task.milestone ? <Badge tone="warn">里程碑 · 需人工验收</Badge> : null}
          {task.human_gate === "milestone" ? (
            <Badge tone="danger">等待人工验收</Badge>
          ) : null}
          {waitingChildren ? (
            <Badge tone="warn">
              等待子任务 {childCompleted}/{children.length}
            </Badge>
          ) : null}
        </div>
        {canControl ? (
          <div className="task-control-bar">
            {taskPaused ? (
              <button
                className="secondary"
                disabled={controlBusy}
                onClick={() =>
                  void control(() => api.resumeTask(task.project_id, task.id))
                }
              >
                继续
              </button>
            ) : (
              <>
                <button
                  className="secondary"
                  disabled={controlBusy}
                  onClick={() =>
                    void control(() => api.pauseTask(task.project_id, task.id))
                  }
                >
                  暂停
                </button>
                <button
                  className="danger"
                  disabled={controlBusy}
                  onClick={() => {
                    if (window.confirm("强制暂停会终止当前作业，确认继续？")) {
                      void control(() =>
                        api.pauseTask(task.project_id, task.id, true),
                      );
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
                  disabled={controlBusy}
                  onClick={() =>
                    void control(() =>
                      api.reorderTaskQueue(task.project_id, task.id, "up"),
                    )
                  }
                >
                  排队上移
                </button>
                <button
                  className="ghost"
                  disabled={controlBusy}
                  onClick={() =>
                    void control(() =>
                      api.reorderTaskQueue(task.project_id, task.id, "down"),
                    )
                  }
                >
                  排队下移
                </button>
              </>
            ) : null}
          </div>
        ) : null}
        {controlError ? <div className="error-box">{controlError}</div> : null}
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
        <TaskAgentPoolPanel
          task={task}
          detail={detail}
          onRefresh={onRefresh}
          disabled={archived || task.stage === "completed" || task.status === "canceled"}
        />
        {task.human_gate === "milestone" ? (
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
        ) : null}
        {task.last_error && task.human_gate !== "milestone" ? (
          <div className="error-box">
            <RichContent content={task.last_error} />
          </div>
        ) : null}
        <section>
          <h3>任务描述</h3>
          <RichContent content={task.description} />
        </section>
        {parent || children.length > 0 ? (
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
        ) : null}
        <section>
          <h3>人工指导</h3>
          <textarea
            value={guidance}
            onChange={(event) => setGuidance(event.target.value)}
            rows={4}
            disabled={guidanceLocked || busy}
            placeholder="指导会在该任务下一次执行作业时生效。"
          />
          <div className="button-row guidance-actions">
            <button
              className="primary"
              disabled={
                guidanceLocked ||
                busy ||
                !guidance.trim() ||
                guidance.trim() === task.guidance.trim()
              }
              onClick={() => onSaveGuidance(guidance)}
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
        </section>
        <section>
          <h3>验收标准</h3>
          <ul>
            {task.acceptance.map((item) => (
              <li key={item}>{item}</li>
            ))}
          </ul>
        </section>
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
            <span className="mono">{task.worktree ?? "尚未创建"}</span>
          </div>
          <div className="kv">
            <span>排队位置</span>
            <span>
              {task.queue_position !== null ? `第 ${task.queue_position} 位` : "未在队列中"}
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
                ? FAILURE_KIND_LABELS[task.last_failure_kind] ??
                  task.last_failure_kind
                : "—"}
            </span>
          </div>
          <div className="kv">
            <span>Agent</span>
            <span>{agentName(task.agent_id)}</span>
          </div>
        </section>
        {task.verifier_issues.length > 0 ? (
          <section>
            <h3>验收反馈</h3>
            <ul>
              {task.verifier_issues.map((issue) => (
                <li key={issue}>
                  <RichContent content={issue} />
                </li>
              ))}
            </ul>
          </section>
        ) : null}
        {task.result_summary ? (
          <section>
            <h3>执行摘要</h3>
            <RichContent content={task.result_summary} />
          </section>
        ) : null}
        <section>
          <h3>作业（{jobs.length}）</h3>
          {jobs.length === 0 ? (
            <div className="muted">还没有作业。</div>
          ) : (
            <div className="jobs">
              {jobs.map((job) => (
                <JobDetailView key={job.id} job={job} />
              ))}
            </div>
          )}
        </section>
      </aside>
    </div>
  );
}

/** 任务 Agent 池：从项目池对应角色派生，复用项目池的 AgentPoolPanel 交互。 */
function TaskAgentPoolPanel({
  task,
  detail,
  onRefresh,
  disabled,
}: {
  task: Task;
  detail: ProjectDetail;
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

  // 加入候选只来自项目池中具备该角色、全局仍存在且尚未出现在任务池里的 Agent。
  const candidates = (role: Role): Agent[] =>
    (detail.agent_pool.roles.find((group) => group.role === role)?.entries ?? [])
      .filter((entry) => entry.kind !== "missing")
      .map((entry) => ({
        id: entry.agent_id,
        name: entry.name,
        kind: entry.kind === "missing" ? "mock" : entry.kind,
        model: entry.model,
        roles: [role],
        max_concurrency: entry.max_concurrency,
        enabled: entry.enabled,
        command: [],
        model_args: [],
        cooldown_until: entry.cooldown_until,
        last_error: entry.last_error,
        failure_kind: entry.failure_kind,
        transient_failures: entry.transient_failures,
        availability: entry.availability,
        usage_limit: entry.usage_limit,
        usage_window: entry.usage_window,
        created_at: "",
        updated_at: "",
      }));

  if (!pool) {
    if (!loadError) return null;
    return <div className="error-box">{loadError}</div>;
  }
  return (
    <AgentPoolPanel
      title="任务 Agent 池"
      subtitle="从项目 Agent 池对应角色快照初始化，只作用于本任务。顺序越靠前优先级越高，按严格优先级派发，池不可用时不回退项目池。"
      roles={pool.roles}
      disabled={disabled}
      addCandidates={candidates}
      onReorder={(role, agentId, position) =>
        api.updateTaskAgent(task.project_id, task.id, role, agentId, { position })
      }
      onMove={(role, agentId, direction) =>
        api.updateTaskAgent(task.project_id, task.id, role, agentId, { direction })
      }
      onToggle={(role, agentId, enabled) =>
        api.updateTaskAgent(task.project_id, task.id, role, agentId, { enabled })
      }
      onAdd={(role, agentId) => api.addTaskAgent(task.project_id, task.id, role, agentId)}
      onRemove={(role, agentId) =>
        api.removeTaskAgent(task.project_id, task.id, role, agentId)
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
  const [reason, setReason] = useState("");
  const [guidance, setGuidance] = useState("");
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
      setReason("");
      setGuidance("");
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
      <textarea
        value={reason}
        onChange={(event) => setReason(event.target.value)}
        rows={3}
        disabled={disabled || busy}
        placeholder="通过意见（可选）；拒绝时必须填写理由。"
      />
      <textarea
        value={guidance}
        onChange={(event) => setGuidance(event.target.value)}
        rows={3}
        disabled={disabled || busy}
        placeholder="拒绝时可选：给执行者或规划者的补充指导。"
      />
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
  const [guidance, setGuidance] = useState("");
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
      setGuidance("");
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
              {TASK_STAGE_LABELS[context.task.stage] ?? context.task.stage}
              {context.task.status ? ` · ${context.task.status}` : ""}
            </span>
          </div>
          <div className="kv">
            <span>失败原因</span>
            <div>
              <RichContent content={context.task.last_error} emptyText="—" />
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
                <div className="muted" key={job.id}>
                  {ROLE_LABELS[job.role] ?? job.role} · {job.agent_name ?? "—"} ·{" "}
                  {job.status}
                  {job.error ? ` · ${job.error}` : ""}
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
          <textarea
            value={guidance}
            onChange={(event) => setGuidance(event.target.value)}
            rows={3}
            disabled={disabled || busy}
            placeholder="补充指导：说明下一步应如何处理。"
          />
          <button
            className="secondary"
            disabled={disabled || busy || !guidance.trim()}
            onClick={() =>
              void submit({ action: "guide", guidance: guidance.trim() })
            }
          >
            记录指导并重新排队
          </button>
        </>
      ) : null}
      {error ? <div className="error-box">{error}</div> : null}
    </section>
  );
}

function eventTone(kind: string): string {
  if (kind.includes("failed") || kind.includes("blocked") || kind.includes("conflict")) {
    return "danger";
  }
  if (kind.includes("completed") || kind.includes("passed") || kind.includes("ready")) {
    return "success";
  }
  if (kind.includes("replan") || kind.includes("superseded")) {
    return "warn";
  }
  if (
    kind.includes("job") ||
    kind.includes("task") ||
    kind.includes("plan") ||
    kind.includes("project") ||
    kind.includes("guidance")
  ) {
    return "info";
  }
  return "muted";
}
