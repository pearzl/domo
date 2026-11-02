import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ProjectDetail } from "../types";
import {
  formatElapsed,
  projectStatus,
} from "../presentation/projectStatus";
import type {
  ExecutionTaskGroups,
  ExecutionTaskItem,
  GuidanceTab,
  ProjectGuidance,
  ProjectStats,
} from "../presentation/projectStatus";
import {
  executionNow,
  type ExecutionEventItem,
  type ExecutionJobItem,
  type ExecutionQueueItem,
} from "../presentation/execution";
import { Badge } from "../ui";

/// 项目标题下的目标文本固定为三行，超出时由用户主动展开。
export function ProjectGoal({ goal }: { goal: string }) {
  const [expanded, setExpanded] = useState(false);
  const [overflowing, setOverflowing] = useState(false);
  const textRef = useRef<HTMLParagraphElement>(null);

  useEffect(() => {
    setExpanded(false);
  }, [goal]);

  useLayoutEffect(() => {
    const node = textRef.current;
    if (!node || expanded) return;
    const measure = () => {
      setOverflowing(node.scrollHeight > node.clientHeight + 1);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [goal, expanded]);

  return (
    <div className="goal-block">
      <p
        ref={textRef}
        className={`goal-line goal-large goal-header${expanded ? " goal-expanded" : ""}`}
      >
        {goal}
      </p>
      {overflowing || expanded ? (
        <button
          type="button"
          className="ghost goal-toggle"
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
        >
          {expanded ? "收起" : "展开目标"}
        </button>
      ) : null}
    </div>
  );
}

/// 项目概览只保留“当前需要你处理”与统计，不提供操作；
/// 运行中作业、排队任务与最近关键事件折叠进待办的紧凑摘要，跳转由外层选项卡承接。
export function ProjectOverview({
  detail,
  onOpenTab,
}: {
  detail: ProjectDetail;
  onOpenTab?: (tab: GuidanceTab) => void;
}) {
  const { project, tasks, jobs, messages, events, proposal } = detail;
  const now = Date.now();
  const status = projectStatus({
    project,
    tasks,
    jobs,
    messages,
    proposal,
    mergedIntoBase: detail.merged_into_base,
    now,
  });
  // 执行现场：运行中作业、排队任务与最近关键事件，供待办摘要按需展示。
  const activity = useMemo(
    () => executionNow({ project, tasks, jobs, events, now }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [project, tasks, jobs, events],
  );

  return (
    <section className="panel project-overview">
      <div className="overview-head">
        <div>
          <h2>项目概览</h2>
          <p className="subtitle">
            当前状态、下一步与需要你处理的事项。
          </p>
        </div>
      </div>
      <div className="overview-grid">
        <div className="overview-stats">
          <TaskStat stats={status.stats} />
          <Stat
            label="作业数"
            value={`${status.stats.jobTotal} 个`}
            note={
              status.stats.jobFailed > 0
                ? `${status.stats.jobFailed} 个失败`
                : "无失败作业"
            }
            tone={status.stats.jobFailed > 0 ? "danger" : "muted"}
          />
          <Stat
            label="用量记录覆盖"
            value={`${status.stats.usageKnown}/${status.stats.jobTotal}`}
            note={
              status.stats.usageUnknown > 0
                ? `${status.stats.usageUnknown} 个作业用量未知`
                : "全部作业有用量记录"
            }
            tone={status.stats.usageUnknown > 0 ? "warn" : "muted"}
          />
          <Stat
            label="项目用时"
            value={formatElapsed(status.stats.elapsedMs)}
            note="自项目创建起"
            tone="muted"
          />
        </div>
        <Guidance
          guidance={status.guidance}
          activity={activity}
          onOpenTab={onOpenTab}
        />
      </div>
    </section>
  );
}

function TaskStat({ stats }: { stats: ProjectStats }) {
  const percentLabel =
    stats.taskTotal > 0 ? `${stats.taskPercent}%` : "—";
  return (
    <div className="overview-stat">
      <span className="meta-label">任务完成</span>
      <strong className="overview-value">
        {stats.taskCompleted}/{stats.taskTotal}
        <span className="overview-percent">{percentLabel}</span>
      </strong>
      <div className="overview-meter" aria-hidden="true">
        <span style={{ width: `${stats.taskPercent}%` }} />
      </div>
    </div>
  );
}

function Stat({
  label,
  value,
  note,
  tone,
}: {
  label: string;
  value: string;
  note: string;
  tone: "muted" | "warn" | "danger";
}) {
  return (
    <div className="overview-stat">
      <span className="meta-label">{label}</span>
      <strong className="overview-value">{value}</strong>
      <span className={`overview-note overview-note-${tone}`}>{note}</span>
    </div>
  );
}

interface ActivitySummary {
  running: ExecutionJobItem[];
  queue: ExecutionQueueItem[];
  events: ExecutionEventItem[];
}

function Guidance({
  guidance,
  activity,
  onOpenTab,
}: {
  guidance: ProjectGuidance;
  activity: ActivitySummary;
  onOpenTab?: (tab: GuidanceTab) => void;
}) {
  const groups = guidance.tasks;
  const taskCount = groups ? groupCount(groups) : 0;
  const hasNow =
    activity.running.length > 0 ||
    activity.queue.length > 0 ||
    activity.events.length > 0;
  const jump = Boolean(guidance.tab && guidance.action);

  return (
    <div className={`overview-todo overview-tone-${guidance.tone}`}>
      <div className="overview-todo-head">
        <span className="meta-label">当前需要你处理</span>
        <Badge tone={guidance.tone}>{guidance.label}</Badge>
      </div>
      <strong className="overview-title">{guidance.title}</strong>
      {guidance.detail ? (
        <p className="overview-detail">{guidance.detail}</p>
      ) : null}
      {groups ? (
        taskCount > 0 ? (
          <div className="overview-task-groups">
            <TaskGroup label="运行中" items={groups.running} />
            <TaskGroup label="排队中" items={groups.queued} />
            <TaskGroup label="待启动" items={groups.starting} />
          </div>
        ) : (
          <span className="overview-note overview-note-muted">
            没有运行、排队或待启动的任务。
          </span>
        )
      ) : null}
      {hasNow ? (
        <div className="overview-now" aria-label="执行现场摘要">
          {activity.running.length > 0 ? (
            <div className="overview-now-row">
              <span className="overview-now-label">运行中作业</span>
              <span className="overview-now-value">
                {activity.running
                  .map((job) =>
                    job.taskKey
                      ? `${job.taskKey} · ${job.roleLabel}`
                      : `${job.roleLabel}作业`,
                  )
                  .join("，")}
              </span>
            </div>
          ) : null}
          {activity.queue.length > 0 ? (
            <div className="overview-now-row">
              <span className="overview-now-label">排队任务</span>
              <span className="overview-now-value">
                {activity.queue.map((item) => item.key).join("，")}
              </span>
            </div>
          ) : null}
          {activity.events.length > 0 ? (
            <div className="overview-now-row">
              <span className="overview-now-label">最近关键事件</span>
              <span className="overview-now-value">
                {activity.events[0].message}
              </span>
            </div>
          ) : null}
        </div>
      ) : null}
      {jump ? (
        <div className="overview-jump-row">
          <button
            type="button"
            className="secondary overview-jump"
            onClick={() =>
              onOpenTab?.(
                (guidance.tab ?? "analysis") as GuidanceTab,
              )
            }
          >
            {guidance.action}
          </button>
        </div>
      ) : null}
    </div>
  );
}

function TaskGroup({
  label,
  items,
}: {
  label: string;
  items: ExecutionTaskItem[];
}) {
  if (items.length === 0) return null;
  return (
    <div className="overview-task-group">
      <span className="overview-task-label">{label}</span>
      <div className="overview-task-items">
        {items.map((item) => (
          <span className="overview-task" key={`${item.key}-${item.stage}`}>
            <span className="mono">{item.key}</span>
            <span>{item.stageLabel}</span>
          </span>
        ))}
      </div>
    </div>
  );
}

function groupCount(groups: ExecutionTaskGroups): number {
  return (
    groups.running.length + groups.queued.length + groups.starting.length
  );
}
