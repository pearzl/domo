import type { FailureSummary } from "../types";
import { FAILURE_KIND_LABELS, failureSummaryLabel, formatTime } from "../ui";

/** 失败状态徽标的输入：Agent 列表与 Agent 池条目共用的失败字段子集。 */
export interface AgentFailureInput {
  failure_kind: string | null;
  failure_summary: FailureSummary | null;
  transient_failures: number;
  cooldown_until: string | null;
}

export interface AgentFailureStatus {
  /** 卡片与列表行上唯一一行失败状态徽标的行内文案。 */
  label: string;
  /** 悬停展示的完整摘要：分类、波动次数、冷却截止、错误消息与 HTTP 状态。 */
  title: string;
  tone: "danger" | "warn";
}

function activeCooldown(value: string | null, now: number): string | null {
  if (!value) return null;
  const at = new Date(value).getTime();
  if (Number.isNaN(at) || at <= now) return null;
  return value;
}

/**
 * 把最近失败分类、波动次数、冷却时间与失败摘要合并成一行状态徽标的文案。
 * 摘要正文只进入 title（悬停可见），行内保持单行不换行，避免不同 Agent 的卡片高度分叉。
 * 原始错误不进入 title，由卡片内的折叠区承载。
 * 没有任何失败信号且不在冷却中时返回 null，让调用方统一渲染中性状态。
 * 冷却提示可由调用方通过 options.cooldown 关闭或自行渲染。
 */
export function agentFailureStatus(
  agent: AgentFailureInput,
  options: { now?: number; cooldown?: boolean } = {},
): AgentFailureStatus | null {
  const now = options.now ?? Date.now();
  const cooldown = options.cooldown === false ? null : activeCooldown(agent.cooldown_until, now);
  const kind = agent.failure_kind ?? agent.failure_summary?.kind ?? null;
  const hasFailure = Boolean(kind) || agent.transient_failures > 0;
  if (!hasFailure && !cooldown) return null;

  const kindLabel = agent.failure_kind
    ? (FAILURE_KIND_LABELS[agent.failure_kind] ?? agent.failure_kind)
    : agent.failure_summary
      ? failureSummaryLabel(agent.failure_summary.kind)
      : null;

  const label = [
    kindLabel ?? "冷却",
    agent.transient_failures > 0 ? `波动 ${agent.transient_failures}` : null,
    cooldown ? `冷却至 ${formatTime(cooldown)}` : null,
  ]
    .filter(Boolean)
    .join(" · ");

  const title = [
    agent.failure_summary
      ? `${failureSummaryLabel(agent.failure_summary.kind)}：${agent.failure_summary.message}`
      : kindLabel,
    agent.failure_summary && agent.failure_summary.status !== null
      ? `HTTP ${agent.failure_summary.status}`
      : null,
    agent.transient_failures > 0 ? `供应商波动 ${agent.transient_failures} 次` : null,
    cooldown ? `冷却至 ${formatTime(cooldown)}` : null,
  ]
    .filter(Boolean)
    .join(" · ");

  return { label, title, tone: hasFailure ? "danger" : "warn" };
}
