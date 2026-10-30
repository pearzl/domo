import type { JobActivity, JobActivityItem } from "../types";

/// 活动摘要只从已有的 JobActivity 投影派生，不发请求、不改事件轮询频率。
/// 原始 stdout/stderr 仍由“原始输出”折叠承载，这里只负责可读摘要。

/// 摘要列表默认展示的条数；活动更多时由调用方提供展开入口。
export const ACTIVITY_DIGEST_LIMIT = 6;
/// 单条摘要的可见字符上限，超出以省略号收尾。
export const ACTIVITY_HEADLINE_MAX = 120;
/// 没有活动时的占位文案。
export const ACTIVITY_WAITING_TEXT = "等待活动…";
/// 结构化片段（原始 JSON）进入摘要时的占位说明。
const JSON_PLACEHOLDER = "（结构化内容，见原始输出）";

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
  /// 关键模式：隐去步骤与成功工具明细，只保留消息、失败工具与进行中的工具。
  keyOnly?: boolean;
  /// 返回的摘要条数上限；<= 0 表示不截断。
  limit?: number;
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

/// 关键模式过滤：消息永远保留；工具只保留失败与进行中；步骤与纯文本行视为噪音。
export function isKeyActivity(item: JobActivityItem): boolean {
  switch (item.kind) {
    case "message":
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
