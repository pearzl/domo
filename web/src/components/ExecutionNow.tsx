import type { ProjectDetail } from "../types";
import {
  executionNow,
  formatElapsedLabel,
  type ExecutionEventItem,
  type ExecutionJobItem,
  type ExecutionQueueItem,
} from "../presentation/execution";
import { Badge, formatTime, shortText } from "../ui";

/// 项目概览里的“现在在做什么”：只展示详情中已有的运行中作业、排队任务与关键事件。
export function ExecutionNow({ detail }: { detail: ProjectDetail }) {
  const view = executionNow({
    project: detail.project,
    tasks: detail.tasks,
    jobs: detail.jobs,
    events: detail.events,
    now: Date.now(),
  });

  return (
    <div className="execution-now">
      <div className="execution-now-head">
        <span className="meta-label">现在在做什么</span>
        <span className="muted">
          {view.running.length > 0
            ? `${view.running.length} 个作业运行中`
            : "没有作业运行"}
          {view.queue.length > 0 ? ` · ${view.queue.length} 个任务等待` : ""}
        </span>
      </div>

      {view.running.length > 0 ? (
        <div className="execution-jobs">
          {view.running.map((job) => (
            <RunningJob key={job.id} job={job} />
          ))}
        </div>
      ) : (
        <div className={`execution-idle execution-idle-${view.idle.tone}`}>
          <strong>{view.idle.title}</strong>
          {view.idle.detail ? <span>{view.idle.detail}</span> : null}
        </div>
      )}

      {view.queue.length > 0 ? (
        <div className="execution-queue">
          <span className="execution-section-title">排队与待启动</span>
          <ol className="execution-queue-list">
            {view.queue.map((item) => (
              <QueueRow key={item.id} item={item} />
            ))}
          </ol>
        </div>
      ) : null}

      {view.events.length > 0 ? (
        <div className="execution-events">
          <span className="execution-section-title">最近关键事件</span>
          <ul className="execution-event-list">
            {view.events.map((event) => (
              <RecentEvent key={event.id} event={event} />
            ))}
          </ul>
        </div>
      ) : null}
    </div>
  );
}

function RunningJob({ job }: { job: ExecutionJobItem }) {
  const target =
    job.taskKey !== null
      ? `${job.taskKey}${job.taskTitle ? ` · ${job.taskTitle}` : ""}`
      : "项目级作业";
  return (
    <div className="execution-job">
      <div className="execution-job-head">
        <Badge tone="info">{job.roleLabel}作业</Badge>
        <span className="execution-job-target" title={job.taskTitle ?? undefined}>
          {target}
        </span>
        {job.attempt > 1 ? <Badge tone="warn">第 {job.attempt} 次</Badge> : null}
      </div>
      <div className="execution-job-meta muted">
        <span>{job.agentName ?? "Agent 未知"}</span>
        <span>开始 {job.startedAt ? formatTime(job.startedAt) : "未知"}</span>
        <span>已运行 {formatElapsedLabel(job.elapsedMs)}</span>
      </div>
    </div>
  );
}

function QueueRow({ item }: { item: ExecutionQueueItem }) {
  return (
    <li className="execution-queue-item">
      <div className="execution-queue-head">
        <span className="mono">{item.key}</span>
        <span className="execution-queue-title" title={item.title}>
          {shortText(item.title, 60)}
        </span>
        <Badge tone={item.stage === "pending" ? "muted" : "warn"}>
          {item.stageLabel}
        </Badge>
      </div>
      <div className="execution-queue-meta muted">
        <span>{item.positionLabel}</span>
        <span>{shortText(item.waitingReason, 80)}</span>
      </div>
    </li>
  );
}

function RecentEvent({ event }: { event: ExecutionEventItem }) {
  return (
    <li className="execution-event">
      <span className="execution-event-message" title={event.message}>
        {shortText(event.message, 80)}
      </span>
      <span className="muted execution-event-meta">
        {event.taskKey ? <span className="mono">{event.taskKey}</span> : null}
        <span>{formatTime(event.createdAt)}</span>
      </span>
    </li>
  );
}
