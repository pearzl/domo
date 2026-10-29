import { useMemo, useState } from "react";
import type { AuditEvent, JobSummary, Task } from "../types";
import { api, errorMessage } from "../api";
import { RichContent } from "./RichContent";
import { Badge, JOB_STATUS_LABELS, ROLE_LABELS, formatMillis, formatTime } from "../ui";

export type EventFilter = "key" | "all" | "jobs" | "config";

const FILTERS: Array<{ key: EventFilter; label: string }> = [
  { key: "key", label: "关键" },
  { key: "all", label: "全部" },
  { key: "jobs", label: "作业" },
  { key: "config", label: "配置" },
];

const PREVIEW_CHARS = 400;

const JOB_KINDS = new Set([
  "job_queued",
  "job_started",
  "job_finished",
  "job_failed",
  "job_stalled",
  "job_stall_recovered",
  "job_recovered",
]);

/// 配置类事件在“关键”视图里属于噪音，只保留在“配置”视图。
const CONFIG_KINDS = new Set([
  "project_agents_updated",
  "task_agents_updated",
  "default_team_updated",
  "agent_usage_reset",
  "analysis_policy_updated",
  "agent_unavailable",
]);

/// 连续出现的同类配置事件折叠成一组，展开后仍逐条可读。
const GROUP_KINDS = new Set([
  "project_agents_updated",
  "task_agents_updated",
  "default_team_updated",
  "agent_usage_reset",
  "analysis_policy_updated",
  "agent_unavailable",
]);

const KIND_LABELS: Record<string, string> = {
  goal_confirmed: "目标已确认",
  goal_updated: "目标已更新",
  project_created: "项目创建",
  project_reopened: "项目重新打开",
  project_completed: "项目完成",
  project_merged: "项目已合并",
  project_blocked: "项目阻塞",
  project_retry: "项目作业自动重试",
  job_retry: "作业重试",
  project_paused: "项目暂停",
  project_paused_forced: "项目强制暂停",
  project_pausing: "项目暂停中",
  project_resumed: "项目恢复",
  project_archived: "项目归档",
  project_agents_updated: "项目 Agent 池更新",
  default_team_updated: "默认团队更新",
  task_agents_updated: "任务 Agent 池更新",
  agent_usage_reset: "Agent 用量重置",
  agent_unavailable: "Agent 不可用",
  analysis_message: "分析消息",
  analysis_questions: "分析提问",
  analysis_ready: "分析就绪",
  analysis_confirmed: "分析已确认",
  analysis_auto_confirmed: "分析自动确认",
  analysis_direct: "直接完成分析",
  analysis_failed: "分析失败",
  analysis_policy_updated: "分析策略更新",
  plan_created: "计划已生成",
  plan_failed: "计划失败",
  plan_ignored: "计划被忽略",
  plan_superseded: "计划被取代",
  task_created: "任务创建",
  task_derived: "任务派生",
  task_executed: "任务执行",
  task_completed: "任务完成",
  task_retry: "任务重试",
  task_blocked: "任务阻塞",
  task_rejected: "任务验收未通过",
  task_queue_reordered: "任务队列调整",
  task_paused: "任务暂停",
  task_paused_forced: "任务强制暂停",
  task_pausing: "任务暂停中",
  task_resumed: "任务恢复",
  milestone_set: "里程碑设置",
  milestone_cleared: "里程碑取消",
  milestone_accepted: "里程碑通过",
  milestone_rejected: "里程碑未通过",
  milestone_acceptance_required: "里程碑待人工确认",
  milestone_merge_conflict: "里程碑合并冲突",
  milestone_replanning: "里程碑重新规划",
  report_ready: "报告就绪",
  report_opinion: "报告意见",
  report_rejected: "报告未通过",
  report_superseded: "报告被取代",
  summary_failed: "总结失败",
  summarizing: "总结中",
  guidance: "补充指导",
  replan_requested: "重规划请求",
  merge_conflict: "合并冲突",
  job_queued: "作业排队",
  job_started: "作业开始",
  job_finished: "作业结束",
  job_failed: "作业失败",
  job_stalled: "作业卡死",
  job_stall_recovered: "作业自行恢复",
  job_recovered: "作业重新调度",
  direct_completed: "直接完成",
  direct_failed: "直接完成失败",
  up_to_date: "无需合并",
};

const KIND_TONES: Record<string, string> = {
  project_completed: "success",
  project_merged: "success",
  project_created: "info",
  project_reopened: "warn",
  project_blocked: "danger",
  project_retry: "warn",
  job_retry: "warn",
  project_paused: "warn",
  project_paused_forced: "warn",
  project_pausing: "warn",
  project_resumed: "info",
  project_archived: "muted",
  project_agents_updated: "muted",
  default_team_updated: "muted",
  task_agents_updated: "muted",
  agent_usage_reset: "muted",
  agent_unavailable: "warn",
  analysis_ready: "success",
  analysis_confirmed: "success",
  analysis_auto_confirmed: "success",
  analysis_direct: "info",
  analysis_failed: "danger",
  analysis_questions: "info",
  analysis_message: "info",
  analysis_policy_updated: "muted",
  plan_created: "success",
  plan_failed: "danger",
  plan_ignored: "warn",
  plan_superseded: "warn",
  task_created: "info",
  task_derived: "info",
  task_executed: "info",
  task_completed: "success",
  task_retry: "warn",
  task_blocked: "danger",
  task_rejected: "danger",
  task_queue_reordered: "muted",
  task_paused: "warn",
  task_paused_forced: "warn",
  task_pausing: "warn",
  task_resumed: "info",
  milestone_set: "warn",
  milestone_cleared: "muted",
  milestone_accepted: "success",
  milestone_rejected: "danger",
  milestone_acceptance_required: "warn",
  milestone_merge_conflict: "danger",
  milestone_replanning: "warn",
  report_ready: "success",
  report_opinion: "info",
  report_rejected: "danger",
  report_superseded: "warn",
  summary_failed: "danger",
  summarizing: "info",
  guidance: "info",
  replan_requested: "warn",
  merge_conflict: "danger",
  job_queued: "info",
  job_started: "info",
  job_finished: "success",
  job_failed: "danger",
  job_stalled: "danger",
  job_stall_recovered: "warn",
  job_recovered: "warn",
  direct_completed: "success",
  direct_failed: "danger",
  up_to_date: "success",
};

function kindLabel(kind: string): string {
  return KIND_LABELS[kind] ?? kind;
}

function kindTone(kind: string): string {
  return KIND_TONES[kind] ?? (isJobKind(kind) ? "info" : "muted");
}

function isJobKind(kind: string): boolean {
  return JOB_KINDS.has(kind);
}

function isConfigKind(kind: string): boolean {
  return CONFIG_KINDS.has(kind);
}

function matchesFilter(kind: string, filter: EventFilter): boolean {
  if (filter === "all") return true;
  if (filter === "jobs") return isJobKind(kind);
  if (filter === "config") return isConfigKind(kind);
  return !isJobKind(kind) && !isConfigKind(kind);
}

type TimelineItem =
  | { type: "event"; event: AuditEvent; anchor: AuditEvent }
  | { type: "job"; jobId: string; events: AuditEvent[]; anchor: AuditEvent }
  | { type: "group"; kind: string; events: AuditEvent[]; anchor: AuditEvent };

function buildTimeline(events: AuditEvent[]): TimelineItem[] {
  const jobStages = new Map<string, AuditEvent[]>();
  for (const event of events) {
    if (!isJobKind(event.kind) || !event.job_id) continue;
    const stages = jobStages.get(event.job_id) ?? [];
    stages.push(event);
    jobStages.set(event.job_id, stages);
  }

  const emittedJobs = new Set<string>();
  const items: TimelineItem[] = [];
  for (const event of events) {
    if (isJobKind(event.kind) && event.job_id) {
      if (emittedJobs.has(event.job_id)) continue;
      emittedJobs.add(event.job_id);
      const stages = jobStages.get(event.job_id) ?? [event];
      items.push({
        type: "job",
        jobId: event.job_id,
        events: stages,
        anchor: stages[stages.length - 1],
      });
      continue;
    }
    if (GROUP_KINDS.has(event.kind)) {
      const last = items[items.length - 1];
      if (last && last.type === "group" && last.kind === event.kind) {
        last.events.push(event);
        last.anchor = event;
      } else {
        items.push({ type: "group", kind: event.kind, events: [event], anchor: event });
      }
      continue;
    }
    items.push({ type: "event", event, anchor: event });
  }
  return items.sort((left, right) => anchorTime(left) - anchorTime(right));
}

function anchorTime(item: TimelineItem): number {
  const value = stageTime(item.anchor);
  return Number.isNaN(value) ? 0 : value;
}

function taskKeyFor(event: AuditEvent, taskKeys: Map<string, string>): string | null {
  if (!event.task_id) return null;
  return taskKeys.get(event.task_id) ?? null;
}

function stageTime(event: AuditEvent): number {
  return new Date(event.created_at).getTime();
}

function jobRoleLabel(events: AuditEvent[]): string {
  for (const event of events) {
    const matched = event.message.match(/^(.+?) ?作业/);
    if (matched?.[1]) return matched[1].trim();
  }
  return "执行";
}

function jobAgentName(events: AuditEvent[], jobs: Map<string, JobSummary>, jobId: string): string {
  const job = jobs.get(jobId);
  if (job?.agent_name) return job.agent_name;
  for (const event of events) {
    const dispatched = event.message.match(/作业已派发给 (.+?)(?:（|$)/);
    if (dispatched?.[1]) return dispatched[1].trim();
    const started = event.message.match(/作业开始（(.+?)）/);
    if (started?.[1]) return started[1].trim();
  }
  return "未知 Agent";
}

function jobResult(
  events: AuditEvent[],
  jobs: Map<string, JobSummary>,
  jobId: string,
): { text: string; tone: string } {
  const job = jobs.get(jobId);
  if (job) {
    switch (job.status) {
      case "succeeded":
        return { text: "完成", tone: "success" };
      case "failed":
        return { text: "失败", tone: "danger" };
      case "canceled":
        return { text: "已取消", tone: "muted" };
      case "running":
        return { text: "运行中", tone: "info" };
      default:
        return { text: JOB_STATUS_LABELS[job.status] ?? job.status, tone: "info" };
    }
  }
  const kinds = new Set(events.map((event) => event.kind));
  if (kinds.has("job_failed") || kinds.has("job_stalled")) return { text: "失败", tone: "danger" };
  const finished = events.find((event) => event.kind === "job_finished");
  if (finished) {
    return finished.message.includes("失败")
      ? { text: "失败", tone: "danger" }
      : { text: "完成", tone: "success" };
  }
  if (kinds.has("job_started")) return { text: "运行中", tone: "info" };
  return { text: "排队中", tone: "warn" };
}

function jobDurationMs(events: AuditEvent[]): number | null {
  const times = events.map(stageTime).filter((value) => !Number.isNaN(value));
  if (times.length < 2) return null;
  const elapsed = Math.max(...times) - Math.min(...times);
  return elapsed > 0 ? elapsed : null;
}

const STAGE_LABELS: Record<string, string> = {
  job_queued: "排队",
  job_started: "开始",
  job_finished: "结束",
  job_failed: "失败",
  job_stalled: "卡死",
  job_stall_recovered: "自行恢复",
  job_recovered: "重新调度",
};

export function EventTimeline({
  events,
  tasks = [],
  jobs = [],
  defaultFilter = "key",
  showFilters = true,
  emptyText = "暂无事件",
}: {
  events: AuditEvent[];
  tasks?: Task[];
  jobs?: JobSummary[];
  defaultFilter?: EventFilter;
  showFilters?: boolean;
  emptyText?: string;
}) {
  const [filter, setFilter] = useState<EventFilter>(defaultFilter);

  const taskKeys = useMemo(() => {
    const map = new Map<string, string>();
    for (const task of tasks) map.set(task.id, task.key);
    return map;
  }, [tasks]);

  const jobMap = useMemo(() => {
    const map = new Map<string, JobSummary>();
    for (const job of jobs) map.set(job.id, job);
    return map;
  }, [jobs]);

  const counts = useMemo(() => {
    const result: Record<EventFilter, number> = { key: 0, all: 0, jobs: 0, config: 0 };
    for (const definition of FILTERS) {
      const filtered = events.filter((event) => matchesFilter(event.kind, definition.key));
      result[definition.key] = buildTimeline(filtered).length;
    }
    return result;
  }, [events]);

  const items = useMemo(() => {
    const filtered = events.filter((event) => matchesFilter(event.kind, filter));
    return buildTimeline(filtered).reverse();
  }, [events, filter]);

  return (
    <div className="event-timeline">
      {showFilters ? (
        <div className="event-toolbar">
          <div className="event-filters" role="tablist" aria-label="事件过滤">
            {FILTERS.map((definition) => (
              <button
                key={definition.key}
                type="button"
                role="tab"
                aria-selected={filter === definition.key}
                className={`ghost event-filter${filter === definition.key ? " active" : ""}`}
                onClick={() => setFilter(definition.key)}
              >
                {definition.label}
                <span className="event-filter-count">{counts[definition.key]}</span>
              </button>
            ))}
          </div>
          <span className="muted event-result-count">当前 {items.length} 条</span>
        </div>
      ) : null}

      {items.length === 0 ? (
        <div className="muted event-empty">{emptyText}</div>
      ) : (
        <div className="events">
          {items.map((item) => {
            if (item.type === "job") {
              return (
                <JobRow
                  key={`job-${item.jobId}`}
                  item={item}
                  taskKey={taskKeyFor(item.events[0], taskKeys)}
                  jobs={jobMap}
                />
              );
            }
            if (item.type === "group" && item.events.length > 1) {
              return <GroupRow key={`group-${item.kind}-${item.anchor.id}`} item={item} />;
            }
            const event = item.type === "event" ? item.event : item.events[0];
            return (
              <EventRow
                key={`event-${event.id}`}
                event={event}
                taskKey={taskKeyFor(event, taskKeys)}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}

function EventRow({ event, taskKey }: { event: AuditEvent; taskKey: string | null }) {
  return (
    <div className="event-item">
      <div className="event">
        <div className="event-kind">
          <Badge tone={kindTone(event.kind)}>{kindLabel(event.kind)}</Badge>
        </div>
        <div className="event-message">
          <EventMessage event={event} />
        </div>
        <div className="event-meta muted">
          {taskKey ? <span className="event-task">{taskKey}</span> : null}
          <span className="event-time">{formatTime(event.created_at)}</span>
        </div>
      </div>
    </div>
  );
}

function GroupRow({ item }: { item: Extract<TimelineItem, { type: "group" }> }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="event-item">
      <div className="event">
        <div className="event-kind">
          <Badge tone={kindTone(item.kind)}>
            {kindLabel(item.kind)} ×{item.events.length}
          </Badge>
        </div>
        <div className="event-message">
          <button
            type="button"
            className="ghost event-toggle"
            aria-expanded={open}
            onClick={() => setOpen((current) => !current)}
          >
            {open ? "收起分组" : `展开 ${item.events.length} 条记录`}
          </button>
        </div>
        <div className="event-meta muted">
          <span className="event-time">{formatTime(item.anchor.created_at)}</span>
        </div>
      </div>
      {open ? (
        <div className="event-expanded">
          {[...item.events].reverse().map((event) => (
            <div className="event-sub" key={event.id}>
              <span className="event-sub-time muted">{formatTime(event.created_at)}</span>
              <span className="event-sub-message">
                <EventMessage event={event} />
              </span>
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}

function JobRow({
  item,
  taskKey,
  jobs,
}: {
  item: Extract<TimelineItem, { type: "job" }>;
  taskKey: string | null;
  jobs: Map<string, JobSummary>;
}) {
  const [open, setOpen] = useState(false);
  const job = jobs.get(item.jobId);
  const role = job ? (ROLE_LABELS[job.role] ?? job.role) : jobRoleLabel(item.events);
  const agent = jobAgentName(item.events, jobs, item.jobId);
  const result = jobResult(item.events, jobs, item.jobId);
  const duration = job?.duration_ms ?? jobDurationMs(item.events);
  const stages = [...item.events].sort((left, right) => stageTime(left) - stageTime(right));

  return (
    <div className="event-item">
      <div className="event">
        <div className="event-kind">
          <Badge tone={result.tone}>{role}作业</Badge>
        </div>
        <div className="event-message">
          <button
            type="button"
            className="ghost event-toggle event-job-title"
            aria-expanded={open}
            onClick={() => setOpen((current) => !current)}
          >
            {agent} · {result.text} · {duration === null ? "—" : formatMillis(duration)}
          </button>
        </div>
        <div className="event-meta muted">
          {taskKey ? <span className="event-task">{taskKey}</span> : null}
          <span className="event-time">{formatTime(item.anchor.created_at)}</span>
        </div>
      </div>
      {open ? (
        <div className="event-expanded event-stages">
          {stages.map((stage) => (
            <div className="event-stage" key={stage.id}>
              <span className="event-stage-name">{STAGE_LABELS[stage.kind] ?? kindLabel(stage.kind)}</span>
              <span className="event-stage-time muted">{formatTime(stage.created_at)}</span>
              <span className="event-stage-message">
                <EventMessage event={stage} />
              </span>
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}

function EventMessage({ event }: { event: AuditEvent }) {
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [full, setFull] = useState<string | null>(null);
  const [error, setError] = useState("");
  const long = event.message_length > PREVIEW_CHARS;
  const preview = Array.from(event.message).slice(0, PREVIEW_CHARS).join("");

  async function load() {
    setOpen(true);
    if (full !== null || loading) return;
    setLoading(true);
    setError("");
    try {
      const detail = await api.getEvent(event.id);
      setFull(detail.message);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }

  if (!long) {
    return <span className="event-preview">{event.message}</span>;
  }

  return (
    <div className="event-message-body">
      {open ? (
        <>
          {loading ? <span className="muted">加载完整事件…</span> : null}
          {error ? <span className="event-error">{error}</span> : null}
          {full !== null ? <RichContent content={full} format="text" /> : null}
          {full !== null || error ? (
            <button type="button" className="ghost event-toggle" onClick={() => setOpen(false)}>
              收起
            </button>
          ) : null}
        </>
      ) : (
        <>
          <span className="event-preview">
            {preview}
            {long ? "…" : ""}
          </span>
          <button type="button" className="ghost event-toggle" onClick={() => void load()}>
            查看完整（{event.message_length} 字）
          </button>
        </>
      )}
    </div>
  );
}
