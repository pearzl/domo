import { useEffect, useMemo, useState } from "react";
import type { ProjectDetail, Task } from "../types";
import { api, errorMessage, subscribeEvents } from "../api";
import { JobDetailView } from "../components/JobDetailView";
import {
  Badge,
  Empty,
  ErrorBox,
  Progress,
  StageBadge,
  StatusBadge,
  TASK_STAGE_LABELS,
  TaskStateBadge,
  formatTime,
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
  const completed = tasks.filter((task) => task.stage === "completed").length;
  const archived = project.status === "archived";
  const paused = project.status === "paused" || project.status === "pausing";
  const waitingReport = project.stage === "summarizing" && project.report;
  const agentName = (agentId: string | null) =>
    agentId ? agents.find((agent) => agent.id === agentId)?.name ?? agentId : "—";
  const projectJobs = jobs.filter((job) => !job.task_id);

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
                <button
                  className="secondary"
                  disabled={busy}
                  onClick={() => run(() => api.pauseProject(project.id))}
                >
                  暂停
                </button>
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
            <Progress total={tasks.length} completed={completed} />
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

      {error ? <ErrorBox message={error} /> : null}

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
            {tasks.map((task) => (
              <button
                key={task.id}
                className={`task-row ${task.id === selectedTaskId ? "selected" : ""}`}
                onClick={() => setSelectedTaskId(task.id)}
              >
                <span className="task-key">{task.key}</span>
                <span className="task-title">{task.title}</span>
                <span className="task-state">
                  <TaskStateBadge stage={task.stage} status={task.status} />
                </span>
                <span className="task-deps muted">
                  {task.deps.length ? `依赖 ${task.deps.join(", ")}` : "无依赖"}
                </span>
                <span className="task-agent muted">{agentName(task.agent_id)}</span>
                <span className="task-counts muted">
                  {task.attempts ? `执行 ${task.attempts}` : ""}
                  {task.rejections ? ` 拒绝 ${task.rejections}` : ""}
                </span>
              </button>
            ))}
          </div>
        )}
        {tasks.length > 0 ? (
          <div className="legend muted">
            阶段：待启动 → 执行 → 验收 → 完成。执行与验收都在独立 worktree 中进行。
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
          busy={busy}
          agentName={agentName}
        />
      ) : null}
    </div>
  );
}

function TaskDrawer({
  task,
  detail,
  onClose,
  onRetry,
  busy,
  agentName,
}: {
  task: Task;
  detail: ProjectDetail;
  onClose: () => void;
  onRetry: () => void;
  busy: boolean;
  agentName: (id: string | null) => string;
}) {
  const jobs = detail.jobs.filter((job) => job.task_id === task.id);
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
        </div>
        {task.status === "blocked" ? (
          <button className="primary" disabled={busy} onClick={onRetry}>
            重试此任务
          </button>
        ) : null}
        {task.last_error ? (
          <div className="error-box pre-wrap">{task.last_error}</div>
        ) : null}
        <section>
          <h3>任务描述</h3>
          <p className="pre-wrap">{task.description}</p>
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

function eventTone(kind: string): string {
  if (kind.includes("failed") || kind.includes("blocked") || kind.includes("conflict")) {
    return "danger";
  }
  if (kind.includes("completed") || kind.includes("passed") || kind.includes("ready")) {
    return "success";
  }
  if (
    kind.includes("job") ||
    kind.includes("task") ||
    kind.includes("plan") ||
    kind.includes("project")
  ) {
    return "info";
  }
  return "muted";
}
