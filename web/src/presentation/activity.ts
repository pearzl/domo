import type { JobActivity, JobActivityItem } from "../types";

/// 活动摘要与时间显示只从已有的 JobActivity 投影派生，不发请求、不改事件轮询频率。
/// stdout/stderr 的存储与接口保持不变，这里只负责可读摘要、分块与稳定的相对时间。

/// 摘要列表默认展示的条数；活动更多时由调用方提供展开入口。
export const ACTIVITY_DIGEST_LIMIT = 6;
/// 单条摘要的可见字符上限，超出以省略号收尾。
export const ACTIVITY_HEADLINE_MAX = 120;
/// 没有活动时的占位文案。
export const ACTIVITY_WAITING_TEXT = "等待活动…";
/// 相对时间与进度提示的固定节拍；显示只在节拍边界推进，不随轮询时刻抖动。
export const ACTIVITY_CLOCK_MS = 1000;
/// 活动接口的固定轮询节拍；每个请求对齐到节拍边界，避免累计漂移。
export const ACTIVITY_POLL_MS = 1500;
/// 结构化片段（原始 JSON）进入摘要时的占位说明。
const JSON_PLACEHOLDER = "（结构化内容，已省略）";

/// 只在“关键模式”下保留的状态：失败与仍在进行的工具调用。
const FAILURE_STATUSES = new Set(["error", "failed", "canceled"]);
const IN_PROGRESS_STATUSES = new Set(["running", "in_progress", "pending"]);

export interface ActivityDigestItem {
  /// 在原活动列表中的位置，用作稳定的渲染 key。
  index: number;
  kind: JobActivityItem["kind"];
  /// 单行、定长的可读摘要。
  text: string;
  /// 失败或进行中的工具调用需要醒目标记。
  tone: "normal" | "failure" | "live";
  tool: string | null;
}

export interface ActivityDigest {
  items: ActivityDigestItem[];
  /// 过滤后被省略的条数；0 表示全部可见。
  hidden: number;
  /// 没有可见摘要时展示的文案。
  waiting: string | null;
}

export interface ActivityDigestOptions {
  /// 关键模式：隐去步骤与成功工具明细，只保留消息、思维链、失败工具与进行中的工具。
  keyOnly?: boolean;
  /// 返回的摘要条数上限；<= 0 表示不截断。
  limit?: number;
}

/// 距下一个固定节拍边界的毫秒数；`now` 恰好落在边界时返回完整间隔。
/// 轮询与时钟都按该值排程，使刷新时刻始终落在同一组节拍上。
export function nextBeatDelay(now: number, interval: number): number {
  if (!Number.isFinite(now) || !Number.isFinite(interval) || interval <= 0) return 0;
  const remainder = ((now % interval) + interval) % interval;
  return remainder === 0 ? interval : interval - remainder;
}

/// 相对于参照时刻的稳定标签：只按整秒向下取整，时间只会单调向前。
export function relativeTimeLabel(timestamp: number, now: number): string {
  if (!Number.isFinite(timestamp) || !Number.isFinite(now)) return "—";
  const seconds = Math.max(0, Math.floor((now - timestamp) / 1000));
  if (seconds < 1) return "刚刚";
  if (seconds < 60) return `${seconds} 秒前`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  return `${Math.floor(hours / 24)} 天前`;
}

/// 事件时刻的本地时钟显示（时:分:秒），用于“最近更新”的绝对时间。
export function formatClockTime(timestamp: number): string {
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleTimeString("zh-CN", { hour12: false });
}

/// 活动内容的可见指纹：条数、类型、文本长度与状态变化都会改变指纹，
/// 仅字节数增长不改变指纹，避免把不可见的心跳当成一次内容更新。
export function activitySignature(activity: JobActivity | null): string {
  if (!activity) return "";
  const summary = activity.summary;
  const counts = summary
    ? `${summary.messages}/${summary.reasoning}/${summary.tool_calls}/${summary.tool_failures}/${summary.steps}`
    : "";
  const items = activity.items
    .map(
      (item) =>
        `${item.kind}:${item.text?.length ?? 0}:${item.status ?? ""}:${item.error?.length ?? 0}`,
    )
    .join(",");
  return `${activity.format}|${counts}|${activity.items.length}|${items}`;
}

/// 活动区是否贴近底部：上滚离开底部后返回 false，重新贴底后恢复 true。
export const ACTIVITY_FOLLOW_THRESHOLD_PX = 24;

export function isNearBottom(
  scrollHeight: number,
  scrollTop: number,
  clientHeight: number,
  threshold: number = ACTIVITY_FOLLOW_THRESHOLD_PX,
): boolean {
  return scrollHeight - scrollTop - clientHeight <= threshold;
}

/// 判断工具调用是否失败；error 字段非空同样视为失败。
export function isToolFailure(item: JobActivityItem): boolean {
  if (item.kind !== "tool") return false;
  if (item.error && item.error.trim()) return true;
  return item.status !== null && FAILURE_STATUSES.has(item.status);
}

/// 判断工具调用是否仍在进行。
export function isToolInProgress(item: JobActivityItem): boolean {
  return (
    item.kind === "tool" &&
    item.status !== null &&
    IN_PROGRESS_STATUSES.has(item.status)
  );
}

/// 单条活动的可读摘要：消息优先取首个有内容的行，工具拼接名称与目标，
/// 其余条目沿用原始文本。结果始终为单行，且不会输出原始 JSON 字符串。
export function activityHeadline(
  item: JobActivityItem,
  max: number = ACTIVITY_HEADLINE_MAX,
): string {
  const text = headlineSource(item).replace(/\s+/g, " ").trim();
  if (!text) return "";
  const chars = Array.from(text);
  if (chars.length <= max) return text;
  return `${chars.slice(0, Math.max(0, max - 1)).join("")}…`;
}

/// 关键模式过滤：消息与思维链永远保留；工具只保留失败与进行中；步骤与纯文本行视为噪音。
export function isKeyActivity(item: JobActivityItem): boolean {
  switch (item.kind) {
    case "message":
    case "reasoning":
      return true;
    case "tool":
      return isToolFailure(item) || isToolInProgress(item);
    default:
      return false;
  }
}

/// 过滤活动条目，空摘要的条目一并剔除。
export function filterActivityItems(
  items: JobActivityItem[],
  keyOnly = false,
): JobActivityItem[] {
  return items.filter(
    (item) => (!keyOnly || isKeyActivity(item)) && activityHeadline(item) !== "",
  );
}

export type ActivityBlock =
  | { type: "reasoning"; start: number; items: JobActivityItem[] }
  | { type: "compact"; start: number; items: JobActivityItem[] }
  | { type: "message"; start: number; item: JobActivityItem }
  | { type: "line"; start: number; item: JobActivityItem };

/// 把活动条目分块：连续思维链独立成块，工具调用与步骤合并为紧凑分组，
/// 消息与纯文本行各自成块。start 是块在完整列表中的起始位置，用作稳定 key。
export function activityBlocks(
  items: JobActivityItem[],
  offset = 0,
): ActivityBlock[] {
  const blocks: ActivityBlock[] = [];
  items.forEach((item, index) => {
    const start = offset + index;
    if (item.kind === "reasoning") {
      const last = blocks[blocks.length - 1];
      if (last?.type === "reasoning") last.items.push(item);
      else blocks.push({ type: "reasoning", start, items: [item] });
      return;
    }
    if (item.kind === "tool" || item.kind === "step") {
      const last = blocks[blocks.length - 1];
      if (last?.type === "compact") last.items.push(item);
      else blocks.push({ type: "compact", start, items: [item] });
      return;
    }
    if (item.kind === "message") {
      blocks.push({ type: "message", start, item });
      return;
    }
    blocks.push({ type: "line", start, item });
  });
  return blocks;
}

/// 运行中进展提示的文本：优先最新思维链，其次最新消息，都没有时用通用占位。
export function activityProgressText(
  items: JobActivityItem[],
  fallback = "正在思考…",
): string {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item.kind !== "reasoning" && item.kind !== "message") continue;
    const headline = activityHeadline(item, 80);
    if (headline) return headline;
  }
  return fallback;
}

/// 由 JobActivity 投影生成摘要列表：先过滤，再取最近 limit 条，最新在后。
export function activityDigest(
  activity: JobActivity | null | undefined,
  options: ActivityDigestOptions = {},
): ActivityDigest {
  const { keyOnly = false, limit = ACTIVITY_DIGEST_LIMIT } = options;
  const all = activity?.items ?? [];
  const kept: ActivityDigestItem[] = [];
  all.forEach((item, index) => {
    if (keyOnly && !isKeyActivity(item)) return;
    const text = activityHeadline(item);
    if (!text) return;
    kept.push({ index, kind: item.kind, text, tone: toneFor(item), tool: item.tool });
  });
  const visible = limit > 0 ? kept.slice(Math.max(0, kept.length - limit)) : kept;
  return {
    items: visible,
    hidden: kept.length - visible.length,
    waiting: kept.length === 0 ? ACTIVITY_WAITING_TEXT : null,
  };
}

function toneFor(item: JobActivityItem): ActivityDigestItem["tone"] {
  if (isToolFailure(item)) return "failure";
  if (isToolInProgress(item)) return "live";
  return "normal";
}

function headlineSource(item: JobActivityItem): string {
  const text = item.text ?? "";
  switch (item.kind) {
    case "message":
      return headlineSegment(text);
    case "reasoning": {
      const detail = headlineSegment(text);
      return detail ? `思考 · ${detail}` : "";
    }
    case "tool": {
      const name = headlineSegment((item.tool ?? "").trim()) || "工具";
      const target = headlineSegment(item.target);
      const base = target ? `${name} · ${target}` : name;
      const error = headlineSegment(item.error);
      return error ? `${base} · ${error}` : base;
    }
    case "step": {
      const detail = headlineSegment(text);
      return detail ? `步骤完成 · ${detail}` : "步骤完成";
    }
    default:
      return headlineSegment(text);
  }
}

/// 单段文本的可读摘要：结构化片段整体换成占位说明，其余取首个有内容的行。
function headlineSegment(value: string | null | undefined): string {
  const trimmed = (value ?? "").trim();
  if (!trimmed) return "";
  if (looksLikeJson(trimmed)) return JSON_PLACEHOLDER;
  const line = firstMeaningfulLine(trimmed) ?? "";
  return line.replace(/```/g, "").trim();
}

/// 以 `{` / `[` 开头、以配对符号结尾的整段文本视为原始 JSON。
function looksLikeJson(value: string): boolean {
  const first = value[0];
  const last = value[value.length - 1];
  if (!((first === "{" && last === "}") || (first === "[" && last === "]"))) {
    return false;
  }
  try {
    JSON.parse(value);
    return true;
  } catch {
    return false;
  }
}

function firstMeaningfulLine(value: string | null | undefined): string | null {
  if (!value) return null;
  const lines = value.split(/\r?\n/);
  for (const line of lines) {
    const cleaned = line.replace(/^\s*(?:[-*>#]+|\d+[.)])\s*/, "").trim();
    if (cleaned) return cleaned;
  }
  return null;
}
