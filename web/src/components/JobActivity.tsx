import { useLayoutEffect, useMemo, useRef, useState } from "react";
import type { RefObject } from "react";
import type {
  JobActivity as JobActivityData,
  JobActivityItem,
  JobActivitySummary,
} from "../types";
import { Badge } from "../ui";
import { useAlignedNow } from "../clock";
import { useJobActivity } from "../useJobActivity";
import {
  ACTIVITY_WAITING_TEXT,
  activityBlocks,
  activityProgressText,
  activitySignature,
  formatClockTime,
  isNearBottom,
  relativeTimeLabel,
  type ActivityBlock,
} from "../presentation/activity";
import { Markdown } from "./markdown";
import { SkeletonLines } from "./Skeleton";

/** 作业记录默认展示的最近条数；更早的记录通过“展开全部”显示。 */
export const RECENT_ACTIVITY_LIMIT = 20;

/**
 * 作业活动面板：以思维链为主视图，工具与步骤收进紧凑分组。
 * 界面只从活动投影渲染，不提供原始输出（stdout / stderr）入口；接口与存储不变。
 *
 * 人工核对清单（对一个运行中的作业）：
 * 1. 连续观察 20 秒，“最近更新”的绝对时间与相对时间、消息/思维链/工具计数只前进、不回退；
 *    相对时间随固定节拍推进，不因轮询间隔变化而跳动。
 * 2. 新活动出现时列表自动滚到底部；上滚后出现“回到最新”，点回或重新贴底后恢复跟随。
 * 3. 思维链默认展开、可折叠；工具与步骤分组默认收起，失败项保持高亮。
 * 4. 依次切换项目选项卡并展开活动时间线，条目淡入、自动跟随到底部，滚动高度不跳变；
 *    开启系统“减少动效”后条目只淡入、不位移，也不再有状态点循环脉冲。
 */
export function JobActivity({
  jobId,
  running,
}: {
  jobId: string;
  running: boolean;
}) {
  const { activity, loading, error, lastUpdatedAt } = useJobActivity(
    jobId,
    running,
  );
  const now = useAlignedNow();
  const [expanded, setExpanded] = useState(false);
  const [following, setFollowing] = useState(true);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const followingRef = useRef(true);
  const signature = activitySignature(activity);

  // 切换作业时先恢复跟随，再执行贴底滚动，保证新作业从最新内容开始展示。
  useLayoutEffect(() => {
    followingRef.current = true;
    setFollowing(true);
    setExpanded(false);
  }, [jobId]);

  useLayoutEffect(() => {
    if (!followingRef.current) return;
    const element = scrollRef.current;
    if (element) element.scrollTop = element.scrollHeight;
  }, [signature, expanded, running, jobId]);

  function handleScroll() {
    const element = scrollRef.current;
    if (!element) return;
    const atBottom = isNearBottom(
      element.scrollHeight,
      element.scrollTop,
      element.clientHeight,
    );
    followingRef.current = atBottom;
    setFollowing(atBottom);
  }

  function followLatest() {
    followingRef.current = true;
    setFollowing(true);
    const element = scrollRef.current;
    if (element) element.scrollTop = element.scrollHeight;
  }

  return (
    <ActivityStream
      activity={activity}
      running={running}
      following={following}
      now={now}
      lastUpdatedAt={lastUpdatedAt}
      loading={loading}
      error={error}
      expanded={expanded}
      onExpand={() => setExpanded(true)}
      onFollow={followLatest}
      scrollRef={scrollRef}
      onScroll={handleScroll}
    />
  );
}

export interface ActivityStreamProps {
  activity: JobActivityData | null;
  running: boolean;
  /// 是否正在跟随最新活动；false 时展示“回到最新”。
  following?: boolean;
  /// 当前时刻，由固定节拍推进。
  now: number;
  /// 活动内容最近一次变化的本地时刻。
  lastUpdatedAt: number | null;
  loading?: boolean;
  error?: string;
  /// 未展开时展示的最近条数；<= 0 表示不截断。
  limit?: number;
  expanded?: boolean;
  onExpand?: () => void;
  onFollow?: () => void;
  variant?: "job" | "live";
  scrollRef?: RefObject<HTMLDivElement | null>;
  onScroll?: () => void;
}

/**
 * 活动流的纯展示部分：头部时间与计数、思维链主视图、工具与步骤分组。
 * 完整活动由调用方先按 limit 截取，这里只负责渲染与跟随按钮。
 */
export function ActivityStream({
  activity,
  running,
  following = true,
  now,
  lastUpdatedAt,
  loading = false,
  error = "",
  limit = RECENT_ACTIVITY_LIMIT,
  expanded = false,
  onExpand,
  onFollow,
  variant = "job",
  scrollRef,
  onScroll,
}: ActivityStreamProps) {
  const allItems = activity?.items ?? [];
  const shownItems = recentActivityItems(allItems, limit, expanded);
  const hiddenCount = allItems.length - shownItems.length;
  const offset = hiddenCount;
  const blocks = useMemo(
    () => activityBlocks(shownItems, offset),
    // activity 每次轮询都会产生新对象，按引用重算分块。
    [activity, expanded, limit],
  );
  const summary = activity?.summary ?? null;
  const progress = activityProgressText(allItems);

  return (
    <div className="job-activity" data-activity-view={variant}>
      <div className="activity-head">
        {variant === "job" ? (
          <span className="job-block-title">作业活动</span>
        ) : null}
        {running ? (
          <span
            className="activity-progress"
            role="status"
            aria-label={`进展 · ${progress}`}
          >
            <span className="activity-dot activity-dot-live" aria-hidden="true" />
            <span className="activity-progress-window" aria-hidden="true">
              <span className="activity-progress-track">
                <span className="activity-progress-text">进展 · {progress}</span>
                <span className="activity-progress-text activity-progress-dup">
                  进展 · {progress}
                </span>
              </span>
            </span>
          </span>
        ) : null}
        {lastUpdatedAt !== null ? (
          <span className="activity-updated muted" data-last-updated={lastUpdatedAt}>
            最近更新 {formatClockTime(lastUpdatedAt)} ·{" "}
            {relativeTimeLabel(lastUpdatedAt, now)}
          </span>
        ) : null}
        {activity ? <ActivityCounts activity={activity} summary={summary} /> : null}
      </div>
      {summary?.truncated ? (
        <div className="activity-note">
          仅展示最近的 {ACTIVITY_PREVIEW_LIMIT} 条活动，更早的记录已省略。
        </div>
      ) : null}
      {loading && !activity ? <SkeletonLines label="加载活动…" /> : null}
      {error ? <div className="error-box">读取活动失败：{error}</div> : null}
      {activity && activity.items.length === 0 ? (
        <div className="muted">
          {running ? ACTIVITY_WAITING_TEXT : "还没有可展示的活动。"}
        </div>
      ) : null}
      <div className="activity-scroll">
        <div
          className="activity-list"
          ref={scrollRef}
          onScroll={onScroll}
        >
          {blocks.map((block) => (
            <ActivityBlockView
              key={`${block.type}-${block.start}`}
              block={block}
            />
          ))}
        </div>
        {!following && onFollow ? (
          <button
            type="button"
            className="ghost activity-follow"
            onClick={onFollow}
          >
            回到最新
          </button>
        ) : null}
      </div>
      {hiddenCount > 0 && onExpand ? (
        <div className="button-row activity-more">
          <button className="secondary" type="button" onClick={onExpand}>
            展开全部（还有 {hiddenCount} 条）
          </button>
        </div>
      ) : null}
    </div>
  );
}

/// 活动条目的可见切片：展开后返回全部，否则只取最近若干条。
export function recentActivityItems<T>(
  items: T[],
  limit: number,
  expanded: boolean,
): T[] {
  if (expanded || limit <= 0 || items.length <= limit) return items;
  return items.slice(items.length - limit);
}

function ActivityCounts({
  activity,
  summary,
}: {
  activity: JobActivityData;
  summary: JobActivitySummary | null;
}) {
  return (
    <span className="muted activity-counts">
      {formatLabel(activity.format)}
      {activity.format === "text"
        ? ` · ${activity.items.length} 条`
        : summary
          ? ` · 消息 ${summary.messages} · 思维链 ${summary.reasoning} · 工具 ${summary.tool_calls} · 失败 ${summary.tool_failures} · 步骤 ${summary.steps}`
          : ""}
      {` · ${formatBytes(activity.bytes)}`}
    </span>
  );
}

function ActivityBlockView({ block }: { block: ActivityBlock }) {
  switch (block.type) {
    case "reasoning":
      return (
        <details
          className="activity-reasoning"
          open
          data-activity-block="reasoning"
        >
          <summary>
            <span className="activity-reasoning-title">思维链</span>
            <span className="muted">×{block.items.length}</span>
          </summary>
          <div className="activity-reasoning-list">
            {block.items.map((item, index) => (
              <div
                className="activity-reasoning-item"
                data-activity-kind="reasoning"
                key={`reasoning-${block.start + index}`}
              >
                {item.text ?? ""}
              </div>
            ))}
          </div>
        </details>
      );
    case "compact":
      return <CompactGroup items={block.items} start={block.start} />;
    case "message":
      return (
        <div
          className="activity-message"
          data-activity-kind="message"
          key={`message-${block.start}`}
        >
          <Markdown source={block.item.text ?? ""} />
        </div>
      );
    default:
      return (
        <pre
          className="activity-line"
          data-activity-kind="line"
          key={`line-${block.start}`}
        >
          {block.item.text ?? ""}
        </pre>
      );
  }
}

/// 工具调用与步骤合并成的紧凑分组；失败项在摘要与明细里都保持高亮。
function CompactGroup({
  items,
  start,
}: {
  items: JobActivityItem[];
  start: number;
}) {
  const failures = items.filter(
    (item) => item.kind === "tool" && (item.error || isFailure(item.status)),
  ).length;
  const steps = items.filter((item) => item.kind === "step").length;
  const tools = items.length - steps;

  return (
    <details className="activity-tool-group activity-compact" data-activity-block="compact">
      <summary>
        <span className="activity-tool-title">工具与步骤 ×{items.length}</span>
        <span className="muted">
          {tools} 次工具{steps > 0 ? ` · ${steps} 步` : ""}
        </span>
        {failures > 0 ? <Badge tone="danger">{failures} 失败</Badge> : null}
      </summary>
      <div className="activity-tool-list">
        {items.map((item, index) =>
          item.kind === "step" ? (
            <div
              className="activity-step activity-step-item"
              data-activity-kind="step"
              key={`step-${start + index}`}
            >
              步骤完成{item.text ? ` · ${item.text}` : ""}
            </div>
          ) : (
            <ToolRow item={item} key={`tool-${start + index}`} />
          ),
        )}
      </div>
    </details>
  );
}

function ToolRow({ item }: { item: JobActivityItem }) {
  const failed = Boolean(item.error) || isFailure(item.status);
  return (
    <div
      className={`activity-tool${failed ? " activity-tool-failed" : ""}`}
      data-activity-kind="tool"
    >
      <div className="activity-tool-head">
        <span className="activity-tool-name">{item.tool ?? "工具"}</span>
        {item.status ? (
          <Badge tone={toolTone(item.status)}>{toolStatusLabel(item.status)}</Badge>
        ) : null}
        {failed && !item.status ? <Badge tone="danger">失败</Badge> : null}
      </div>
      {item.target ? (
        <div className="activity-tool-target">{item.target}</div>
      ) : null}
      {item.error ? <div className="activity-tool-error">{item.error}</div> : null}
    </div>
  );
}

function isFailure(status: string | null): boolean {
  if (!status) return false;
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

const ACTIVITY_PREVIEW_LIMIT = 300;

function formatLabel(format: JobActivityData["format"]): string {
  switch (format) {
    case "codex":
      return "Codex";
    case "codebuddy":
      return "CodeBuddy";
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
