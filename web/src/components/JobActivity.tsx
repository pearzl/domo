import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { SyntheticEvent } from "react";
import type { JobActivity, JobActivityItem } from "../types";
import { api, errorMessage } from "../api";
import { Badge } from "../ui";
import { LiveOutput } from "./LiveOutput";
import { Markdown } from "./markdown";

const ACTIVITY_REFRESH_MS = 1500;
const ACTIVITY_PREVIEW_LIMIT = 300;
/** 作业记录默认展示的最近条数；更早的记录通过“展开全部”显示。 */
export const RECENT_ACTIVITY_LIMIT = 20;
/** 距离底部多近仍视为跟随最新活动。 */
const FOLLOW_THRESHOLD_PX = 24;

interface ActivityGroup {
  tool: boolean;
  items: JobActivityItem[];
  /// 分组在完整活动列表中的起始位置，用作稳定 key。
  start: number;
}

/**
 * Renders the job's activity transcript. Only mounted while the job is
 * expanded; running jobs refresh on a timer that stops when the job ends.
 * Records default to the most recent entries and older ones stay reachable
 * through “展开全部”. Raw stdout/stderr live in a collapsed fold.
 */
export function JobActivity({ jobId, running }: { jobId: string; running: boolean }) {
  const [activity, setActivity] = useState<JobActivity | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [rawOpen, setRawOpen] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [following, setFollowing] = useState(true);
  /// 活动内容的变化计数；每条活动条数都可能不变而文本增长。
  const [revision, setRevision] = useState(0);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const followingRef = useRef(true);

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;

    async function tick() {
      try {
        const next = await api.getJobActivity(jobId);
        if (cancelled) return;
        setActivity(next);
        setRevision((value) => value + 1);
        setError("");
      } catch (err) {
        if (cancelled) return;
        setError(errorMessage(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
      if (!cancelled && running) {
        timer = window.setTimeout(() => void tick(), ACTIVITY_REFRESH_MS);
      }
    }

    void tick();
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [jobId, running]);

  const allItems = activity?.items ?? [];
  const shownItems = recentActivityItems(allItems, RECENT_ACTIVITY_LIMIT, expanded);
  const hiddenCount = allItems.length - shownItems.length;
  const groups = useMemo(
    () => groupItems(shownItems, allItems.length - shownItems.length),
    // 每次刷新都会产生新的 activity 对象，按引用重算分组。
    [activity, expanded],
  );
  const summary = activity?.summary;

  useLayoutEffect(() => {
    if (!followingRef.current) return;
    const element = scrollRef.current;
    if (element) element.scrollTop = element.scrollHeight;
  }, [revision, running, expanded]);

  function handleScroll() {
    const element = scrollRef.current;
    if (!element) return;
    const atBottom =
      element.scrollHeight - element.scrollTop - element.clientHeight <=
      FOLLOW_THRESHOLD_PX;
    followingRef.current = atBottom;
    setFollowing(atBottom);
  }

  function followLatest() {
    const element = scrollRef.current;
    followingRef.current = true;
    setFollowing(true);
    if (element) element.scrollTop = element.scrollHeight;
  }

  function handleRawToggle(event: SyntheticEvent<HTMLDetailsElement>) {
    setRawOpen(event.currentTarget.open);
  }

  return (
    <div className="job-activity">
      <div className="activity-head">
        <span className="job-block-title">作业活动</span>
        {running ? <span className="activity-live">运行中 · 每 1.5 秒刷新</span> : null}
        {activity ? (
          <span className="muted">
            {activity.format === "text"
              ? `纯文本日志 ${activity.items.length} 行`
              : `${formatLabel(activity.format)} · 消息 ${summary?.messages ?? 0} · 工具 ${summary?.tool_calls ?? 0} · 失败 ${summary?.tool_failures ?? 0} · 步骤 ${summary?.steps ?? 0}`}
            {" · "}
            {formatBytes(activity.bytes)}
          </span>
        ) : null}
      </div>
      {summary?.truncated ? (
        <div className="activity-note">
          仅显示最近的 {ACTIVITY_PREVIEW_LIMIT} 条，完整内容见“原始输出”。
        </div>
      ) : null}
      {loading && !activity ? <div className="muted">加载活动…</div> : null}
      {error ? <div className="error-box">读取活动失败：{error}</div> : null}
      {activity && activity.items.length === 0 ? (
        <div className="muted">还没有可展示的活动。</div>
      ) : null}
      <div className={`activity-scroll${running ? " activity-scroll-live" : ""}`}>
        <div className="activity-list" ref={scrollRef} onScroll={handleScroll}>
          {groups.map((group) =>
            group.tool ? (
              <ToolGroup key={`tools-${group.start}`} items={group.items} />
            ) : (
              group.items.map((item, itemIndex) => (
                <ActivityRow key={`row-${group.start + itemIndex}`} item={item} />
              ))
            ),
          )}
        </div>
        {running && !following ? (
          <button
            type="button"
            className="ghost activity-follow"
            onClick={followLatest}
          >
            回到最新
          </button>
        ) : null}
      </div>
      {hiddenCount > 0 ? (
        <div className="button-row activity-more">
          <button className="secondary" type="button" onClick={() => setExpanded(true)}>
            展开全部（还有 {hiddenCount} 条）
          </button>
        </div>
      ) : null}
      <details className="activity-raw" onToggle={handleRawToggle}>
        <summary>原始输出</summary>
        <div className="activity-raw-body">
          {rawOpen ? (
            <LiveOutput jobId={jobId} running={running} />
          ) : (
            <div className="muted">展开后加载完整 stdout / stderr。</div>
          )}
        </div>
      </details>
    </div>
  );
}

/// 活动记录的可见切片：展开后返回全部，否则只取最近若干条。
export function recentActivityItems<T>(
  items: T[],
  limit: number,
  expanded: boolean,
): T[] {
  if (expanded || limit <= 0 || items.length <= limit) return items;
  return items.slice(items.length - limit);
}

function groupItems(items: JobActivityItem[], offset = 0): ActivityGroup[] {
  const groups: ActivityGroup[] = [];
  items.forEach((item, index) => {
    const tool = item.kind === "tool";
    const last = groups[groups.length - 1];
    if (tool && last?.tool) {
      last.items.push(item);
    } else {
      groups.push({ tool, items: [item], start: offset + index });
    }
  });
  return groups;
}

function ActivityRow({ item }: { item: JobActivityItem }) {
  if (item.kind === "message") {
    return (
      <div className="activity-message">
        <Markdown source={item.text ?? ""} />
      </div>
    );
  }
  if (item.kind === "step") {
    return (
      <div className="activity-step">
        步骤完成{item.text ? ` · ${item.text}` : ""}
      </div>
    );
  }
  return <pre className="activity-line">{item.text ?? ""}</pre>;
}

function ToolGroup({ items }: { items: JobActivityItem[] }) {
  const failures = items.filter((item) => item.error || isFailure(item.status)).length;
  return (
    <details className="activity-tool-group">
      <summary>
        <span className="activity-tool-title">工具调用 ×{items.length}</span>
        {failures > 0 ? <Badge tone="danger">{failures} 失败</Badge> : null}
      </summary>
      <div className="activity-tool-list">
        {items.map((item, index) => (
          <div
            className={`activity-tool${item.error ? " activity-tool-failed" : ""}`}
            key={index}
          >
            <div className="activity-tool-head">
              <span className="activity-tool-name">{item.tool ?? "工具"}</span>
              {item.status ? (
                <Badge tone={toolTone(item.status)}>{toolStatusLabel(item.status)}</Badge>
              ) : null}
            </div>
            {item.target ? <div className="activity-tool-target">{item.target}</div> : null}
            {item.error ? <div className="activity-tool-error">{item.error}</div> : null}
          </div>
        ))}
      </div>
    </details>
  );
}

function isFailure(status: string | null): boolean {
  return status === "error" || status === "failed" || status === "canceled";
}

function toolTone(status: string): string {
  if (isFailure(status)) return "danger";
  if (status === "completed") return "success";
  if (status === "running" || status === "in_progress") return "info";
  return "muted";
}

function toolStatusLabel(status: string): string {
  switch (status) {
    case "completed":
      return "完成";
    case "running":
    case "in_progress":
      return "运行中";
    case "error":
    case "failed":
      return "失败";
    case "canceled":
      return "已取消";
    case "pending":
      return "等待";
    default:
      return status;
  }
}

function formatLabel(format: JobActivity["format"]): string {
  switch (format) {
    case "codex":
      return "Codex";
    case "workbuddy":
      return "WorkBuddy";
    case "opencode":
      return "OpenCode";
    default:
      return "文本";
  }
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} 字节`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
