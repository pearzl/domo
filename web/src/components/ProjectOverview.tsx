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
import { UsageSummary } from "./UsageSummary";

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

/// 项目概览集中展示当前进展或待办、下一步动作、统计与用量摘要；
/// 运行中作业、排队任务与最近关键事件折叠进紧凑摘要，面板与作业跳转由外层承接。
/// 宽屏布局为上排待办占满整行、下排统计区与用量摘要等宽并排；
/// 960px 及以下依次纵向堆叠为待办、用量摘要、统计区。
/// 作业数与用量记录覆盖只由用量摘要给出，统计区保留任务完成与项目用时。
///
/// 人工核对清单：
/// - 1280px：用量摘要与统计区等宽并排，用量卡片不被拉伸，左侧无大面积空白。
/// - 900px：待办、用量摘要、统计区依次纵向堆叠，用量卡片仍是两列自适应。
/// - 两种宽度下作业数与用量记录覆盖都只出现一次。
export function ProjectOverview({
  detail,
  onOpenTab,
  onRetryJob,
  onMerge,
  onResume,
  busy = false,
  merging = false,
}: {
  detail: ProjectDetail;
  onOpenTab?: (tab: GuidanceTab, jobId?: string) => void;
  onRetryJob?: (jobId: string) => void;
  onMerge?: () => void;
  onResume?: () => void;
  busy?: boolean;
  merging?: boolean;
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
        <Guidance
          guidance={status.guidance}
          needsAttention={status.attention !== null}
          activity={activity}
          onOpenTab={onOpenTab}
          onRetryJob={onRetryJob}
          onMerge={onMerge}
          onResume={onResume}
          busy={busy}
          merging={merging}
        />
        <div className="overview-stats" aria-label="项目统计">
          <TaskStat stats={status.stats} />
          <Stat
            label="项目用时"
            value={formatElapsed(status.stats.elapsedMs)}
            note="自项目创建起"
          />
        </div>
        <UsageSummary projectId={project.id} usage={detail.usage} />
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

/// 统计卡只展示数值与一句说明；说明为空时不留空行。
function Stat({
  label,
  value,
  note,
}: {
  label: string;
  value: string;
  note: string;
}) {
  return (
    <div className="overview-stat">
      <span className="meta-label">{label}</span>
      <strong className="overview-value">{value}</strong>
      {note ? <span className="overview-note">{note}</span> : null}
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
  needsAttention,
  activity,
  onOpenTab,
  onRetryJob,
  onMerge,
  onResume,
  busy,
  merging,
}: {
  guidance: ProjectGuidance;
  needsAttention: boolean;
  activity: ActivitySummary;
  onOpenTab?: (tab: GuidanceTab, jobId?: string) => void;
  onRetryJob?: (jobId: string) => void;
  onMerge?: () => void;
  onResume?: () => void;
  busy: boolean;
  merging: boolean;
}) {
  const groups = guidance.tasks;
  const taskCount = groups ? groupCount(groups) : 0;
  const hasNow =
    activity.running.length > 0 ||
    activity.queue.length > 0 ||
    activity.events.length > 0;
  const hasActions = Boolean(
    (guidance.tab && guidance.action) ||
      guidance.retryJobId ||
      guidance.merge ||
      guidance.resume,
  );

  return (
    <div className={`overview-todo overview-tone-${guidance.tone}`}>
      <div className="overview-todo-head">
        <span className="meta-label">
          {needsAttention ? "当前需要你处理" : "当前进展"}
        </span>
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
      {hasActions ? (
        <div className="overview-jump-row">
          {guidance.retryJobId ? (
            <button
              type="button"
              className="primary overview-jump"
              disabled={busy}
              onClick={() => onRetryJob?.(guidance.retryJobId!)}
            >
              重试失败作业
            </button>
          ) : null}
          {guidance.merge ? (
            <button
              type="button"
              className="primary overview-jump"
              disabled={busy || merging}
              onClick={onMerge}
            >
              {merging ? "合入中…" : "合入项目"}
            </button>
          ) : null}
          {guidance.resume ? (
            <button
              type="button"
              className="primary overview-jump"
              disabled={busy}
              onClick={onResume}
            >
              恢复项目
            </button>
          ) : null}
          {guidance.tab && guidance.action ? (
            <button
              type="button"
              className="secondary overview-jump"
              onClick={() => onOpenTab?.(guidance.tab!, guidance.jobId)}
            >
              {guidance.action}
            </button>
          ) : null}
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
