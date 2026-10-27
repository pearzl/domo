import { useEffect, useMemo, useState } from "react";
import type {
  ArbitrationContext,
  ProjectAgentPool,
  ProjectDetail,
  ProjectUsage,
  Role,
  Task,
  UsageGroup,
} from "../types";
import { api, errorMessage, subscribeEvents } from "../api";
import { JobDetailView } from "../components/JobDetailView";
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

  const { project, tasks, jobs, messages, events, agents } = detail;
  const activeTasks = tasks.filter((task) => task.status !== "canceled");
  const completed = activeTasks.filter((task) => task.stage === "completed").length;
  const archived = project.status === "archived";
  const paused = project.status === "paused" || project.status === "pausing";
  const waitingReport = project.stage === "summarizing" && project.report;
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
  const canReorder = (task: Task) =>
    !archived &&
    task.stage === "pending" &&
    task.attempts === 0 &&
    task.rejections === 0 &&
    task.status !== "canceled";

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
            !waitingReport ? (
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
            <div className="pre-wrap">{project.blocked_reason}</div>
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
        pool={detail.agent_pool}
        projectId={project.id}
        onRefresh={() => refresh()}
        disabled={archived}
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
              <div className="pre-wrap">{project.replan_reason}</div>
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
          <p className="subtitle">规划者会先判断目标是否清晰；不确定时可以在这里补充信息再确认。</p>
          <div className="messages">
            {messages.map((item) => (
              <div key={item.id} className={`message message-${item.role}`}>
                <div className="message-role">{item.role === "user" ? "你" : "规划者"}</div>
                <div className="pre-wrap">{item.content}</div>
              </div>
            ))}
          </div>
          <div className="confirm-row">
            <textarea
              value={message}
              onChange={(event) => setMessage(event.target.value)}
              rows={3}
              placeholder="补充说明（可选）。如果规划者已经提问，请在这里回答。"
            />
            <button
              className="primary"
              disabled={busy}
              onClick={() =>
                run(async () => {
                  await api.confirmGoal(project.id, message);
                  setMessage("");
                })
              }
            >
              {messages.length > 1 ? "发送并继续" : "确认目标并规划"}
            </button>
          </div>
        </section>
      ) : messages.length > 1 ? (
        <details className="panel">
          <summary>目标确认记录</summary>
          <div className="messages compact">
            {messages.map((item) => (
              <div key={item.id} className={`message message-${item.role}`}>
                <div className="message-role">{item.role === "user" ? "你" : "规划者"}</div>
                <div className="pre-wrap">{item.content}</div>
              </div>
            ))}
          </div>
        </details>
      ) : null}

      <section className="panel">
        <h2>任务</h2>
        {tasks.length === 0 ? (
          <Empty title="还没有任务" hint="确认目标后，规划者会输出带依赖关系的任务列表。" />
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
        {tasks.length > 0 ? (
          <div className="legend muted">
            阶段：待启动 → 执行 → 验收 → 完成。执行与验收都在独立 worktree 中进行；派生任务跳过验收，完成后直接合入父任务分支。
          </div>
        ) : null}
      </section>

      {projectJobs.length > 0 ? (
        <section className="panel">
          <h2>规划与总结作业</h2>
          <div className="jobs">
            {projectJobs.map((job) => (
              <JobDetailView key={job.id} job={job} />
            ))}
          </div>
        </section>
      ) : null}

      {project.report ? (
        <section className="panel">
          <h2>项目报告</h2>
          {project.report_feedback ? (
            <div className="muted">上一轮意见：{project.report_feedback}</div>
          ) : null}
          <pre className="report">{project.report}</pre>
          {project.stage === "summarizing" ? (
            <div className="report-actions">
              <textarea
                value={feedback}
                onChange={(event) => setFeedback(event.target.value)}
                rows={3}
                placeholder="拒绝时填写原因（通过可留空）"
              />
              <div className="button-row">
                <button
                  className="primary"
                  disabled={busy}
                  onClick={() => run(() => api.submitReport(project.id, "accept"))}
                >
                  通过并完成
                </button>
                <button
                  className="secondary"
                  disabled={busy}
                  onClick={() =>
                    run(async () => {
                      await api.submitReport(project.id, "reject", feedback);
                      setFeedback("");
                    })
                  }
                >
                  拒绝并重写
                </button>
              </div>
            </div>
          ) : null}
        </section>
      ) : null}

      {project.stage === "summarizing" && !project.report && project.blocked_reason ? (
        <section className="panel">
          <h2>项目报告</h2>
          <p className="muted">报告生成失败，可点击上方“重试项目”。</p>
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

function AgentPoolPanel({
  pool,
  projectId,
  onRefresh,
  disabled,
}: {
  pool: ProjectAgentPool;
  projectId: string;
  onRefresh: () => Promise<void>;
  disabled: boolean;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  async function act(action: () => Promise<unknown>) {
    setBusy(true);
    setError("");
    try {
      await action();
      await onRefresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <details className="panel agent-pool">
      <summary>项目 Agent 池</summary>
      <p className="subtitle">
        每个角色按顺序选择已启用的 Agent，顺序越靠前优先级越高。项目池在创建时继承全局
        Agent 配置，之后独立维护。
      </p>
      {pool.roles.map((group) => (
        <div className="agent-pool-role" key={group.role}>
          <h3>{ROLE_LABELS[group.role] ?? group.role}</h3>
          {group.entries.length === 0 ? (
            <div className="muted">这个角色还没有 Agent。</div>
          ) : (
            <div className="agent-pool-list">
              {group.entries.map((entry, index) => (
                <div
                  className={`agent-pool-row${entry.available ? "" : " unavailable"}`}
                  key={entry.agent_id}
                >
                  <span className="agent-pool-rank">{index + 1}</span>
                  <span className="agent-pool-name">
                    <span>{entry.name}</span>
                    <span className="muted mono">
                      {entry.kind === "missing" ? entry.agent_id : entry.kind}
                      {entry.model ? ` · ${entry.model}` : ""}
                    </span>
                  </span>
                  <span className="badge-row">
                    {!entry.available ? (
                      <Badge tone="danger" title={entry.unavailable_reason ?? ""}>
                        不可用
                      </Badge>
                    ) : null}
                    {entry.available && !entry.enabled ? (
                      <Badge tone="warn">已停用</Badge>
                    ) : null}
                    {entry.available && entry.enabled ? (
                      <Badge tone="success">已启用</Badge>
                    ) : null}
                    {entry.cooldown_until &&
                    new Date(entry.cooldown_until).getTime() > Date.now() ? (
                      <Badge
                        tone="warn"
                        title={`冷却至 ${formatTime(entry.cooldown_until)}`}
                      >
                        冷却中 · {formatTime(entry.cooldown_until)}
                      </Badge>
                    ) : null}
                    {entry.failure_kind ? (
                      <Badge tone="danger">
                        {FAILURE_KIND_LABELS[entry.failure_kind] ?? entry.failure_kind}
                        {entry.transient_failures
                          ? ` · 波动 ${entry.transient_failures} 次`
                          : ""}
                      </Badge>
                    ) : null}
                    {entry.availability_reason ? (
                      <Badge tone="warn" title={entry.availability_reason}>
                        不在可用时段
                      </Badge>
                    ) : null}
                    {entry.usage_window?.exceeded ? (
                      <Badge
                        tone="danger"
                        title={`已用 ${formatUsageAmount(entry.usage_window.used)} / 限额 ${formatUsageAmount(entry.usage_window.limit)}`}
                      >
                        用量限额已触发
                      </Badge>
                    ) : null}
                  </span>
                  <span className="agent-pool-actions">
                    <button
                      className="ghost"
                      disabled={disabled || busy || index === 0}
                      title="上移"
                      onClick={() =>
                        void act(() =>
                          api.updateProjectAgent(projectId, group.role, entry.agent_id, {
                            direction: "up",
                          }),
                        )
                      }
                    >
                      ↑
                    </button>
                    <button
                      className="ghost"
                      disabled={disabled || busy || index === group.entries.length - 1}
                      title="下移"
                      onClick={() =>
                        void act(() =>
                          api.updateProjectAgent(projectId, group.role, entry.agent_id, {
                            direction: "down",
                          }),
                        )
                      }
                    >
                      ↓
                    </button>
                    <button
                      className={entry.enabled ? "secondary" : "ghost"}
                      disabled={disabled || busy || !entry.available}
                      onClick={() =>
                        void act(() =>
                          api.updateProjectAgent(projectId, group.role, entry.agent_id, {
                            enabled: !entry.enabled,
                          }),
                        )
                      }
                    >
                      {entry.enabled ? "停用" : "启用"}
                    </button>
                  </span>
                  {entry.unavailable_reason ? (
                    <span className="agent-pool-reason muted">
                      {entry.unavailable_reason}
                    </span>
                  ) : entry.last_error ? (
                    <span className="agent-pool-reason muted" title={entry.last_error}>
                      {entry.last_error}
                    </span>
                  ) : null}
                </div>
              ))}
            </div>
          )}
        </div>
      ))}
      {error ? <ErrorBox message={error} /> : null}
    </details>
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
        <TaskAgentOverridePanel
          task={task}
          pool={detail.agent_pool}
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
          <div className="error-box pre-wrap">{task.last_error}</div>
        ) : null}
        <section>
          <h3>任务描述</h3>
          <p className="pre-wrap">{task.description}</p>
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
                      <div className="muted pre-wrap">
                        {child.result_summary || child.last_error || "尚无结果"}
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
                <li key={issue}>{issue}</li>
              ))}
            </ul>
          </section>
        ) : null}
        {task.result_summary ? (
          <section>
            <h3>执行摘要</h3>
            <p className="pre-wrap">{task.result_summary}</p>
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

function TaskAgentOverridePanel({
  task,
  pool,
  onRefresh,
  disabled,
}: {
  task: Task;
  pool: ProjectAgentPool;
  onRefresh: () => Promise<void>;
  disabled: boolean;
}) {
  const [executor, setExecutor] = useState(task.executor_agent_id ?? "");
  const [verifier, setVerifier] = useState(task.verifier_agent_id ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    setExecutor(task.executor_agent_id ?? "");
    setVerifier(task.verifier_agent_id ?? "");
  }, [task.id, task.executor_agent_id, task.verifier_agent_id]);

  const dirty =
    executor !== (task.executor_agent_id ?? "") ||
    verifier !== (task.verifier_agent_id ?? "");

  async function save(nextExecutor: string, nextVerifier: string) {
    setBusy(true);
    setError("");
    try {
      await api.setTaskAgents(task.project_id, task.id, {
        executor_agent_id: nextExecutor || null,
        verifier_agent_id: nextVerifier || null,
      });
      await onRefresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  const options = (role: Role) =>
    pool.roles.find((group) => group.role === role)?.entries ?? [];

  return (
    <section>
      <h3>任务 Agent 覆盖</h3>
      <p className="muted">
        指定后优先于项目 Agent 池；模型跟随所选 Agent。留空表示跟随项目池。
      </p>
      <div className="task-agent-grid">
        <label>
          <span>执行 Agent</span>
          <select
            value={executor}
            disabled={disabled || busy}
            onChange={(event) => setExecutor(event.target.value)}
          >
            <option value="">跟随项目池</option>
            {options("executor").map((entry) => (
              <option key={entry.agent_id} value={entry.agent_id} disabled={!entry.available}>
                {entry.name}
                {entry.available ? "" : "（不可用）"}
              </option>
            ))}
          </select>
        </label>
        <label>
          <span>验收 Agent</span>
          <select
            value={verifier}
            disabled={disabled || busy}
            onChange={(event) => setVerifier(event.target.value)}
          >
            <option value="">跟随项目池</option>
            {options("verifier").map((entry) => (
              <option key={entry.agent_id} value={entry.agent_id} disabled={!entry.available}>
                {entry.name}
                {entry.available ? "" : "（不可用）"}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="button-row">
        <button
          className="primary"
          disabled={disabled || busy || !dirty}
          onClick={() => void save(executor, verifier)}
        >
          保存 Agent 覆盖
        </button>
        <button
          className="secondary"
          disabled={
            disabled ||
            busy ||
            (!task.executor_agent_id && !task.verifier_agent_id)
          }
          onClick={() => {
            setExecutor("");
            setVerifier("");
            void save("", "");
          }}
        >
          清除覆盖
        </button>
      </div>
      {error ? <div className="error-box">{error}</div> : null}
    </section>
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

  async function submit(action: "accept" | "reject") {
    if (action === "reject" && !reason.trim()) {
      setError("拒绝时必须填写理由。");
      return;
    }
    setBusy(true);
    setError("");
    try {
      if (action === "accept") {
        await api.acceptMilestone(projectId, task.id, reason);
      } else {
        await api.rejectMilestone(projectId, task.id, reason, guidance);
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
        <div className="pre-wrap">{task.last_error ?? "里程碑任务必须由人工确认。"}</div>
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
        placeholder="拒绝时可选：给执行者的补充指导。"
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
          onClick={() => void submit("reject")}
        >
          拒绝并退回执行
        </button>
      </div>
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
            <span className="pre-wrap">{context.task.last_error ?? "—"}</span>
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
