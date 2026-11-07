import type { Agent, AgentAvailability, UsageMetric, UsageWindowKind } from "../types";
import { agentFailureStatus } from "../presentation/agentFailure";
import { Badge, formatTime, formatUsageAmount } from "../ui";
import { RichContent } from "./RichContent";

const DAY_LABELS = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
const ROLE_TEXT: Record<string, string> = {
  planner: "规划者",
  executor: "执行者",
  verifier: "验收者",
};
const METRIC_LABELS: Record<UsageMetric, string> = {
  tokens: "token（输入+输出）",
  input_tokens: "输入 token",
  output_tokens: "输出 token",
  cost: "费用",
};
const WINDOW_LABELS: Record<UsageWindowKind, string> = {
  day: "每日",
  month: "每月",
};

export interface AgentCardProps {
  agent: Agent;
  /** 该 Agent 的删除或用量重置正在进行。 */
  busy?: boolean;
  /** 判定冷却是否仍在进行的时间基准；缺省取当前时间。 */
  now?: number;
  onEdit: () => void;
  onToggle: () => void;
  onRemove: () => void;
  onResetUsage: () => void;
}

/**
 * 单个 Agent 卡片。失败分类、波动次数、冷却与摘要合并为一行状态徽标，
 * 摘要正文只放在徽标的 title 里悬停查看；原始错误收进卡片内的折叠区。
 * 每张卡片都恰好渲染一个运行状态徽标；能力与启停徽标换行完整呈现，不裁切重要状态。
 */
export function AgentCard({
  agent,
  busy = false,
  now,
  onEdit,
  onToggle,
  onRemove,
  onResetUsage,
}: AgentCardProps) {
  const failureStatus = agentFailureStatus(agent, { now });
  return (
    <article
      className={`agent-card panel ${agent.enabled ? "" : "disabled"}`}
      data-agent-status={failureStatus ? "failure" : "ok"}
    >
      <div className="agent-head">
        <h2>{agent.name}</h2>
        <div className="button-row">
          <button className="ghost" onClick={onEdit}>
            编辑
          </button>
          <button
            className="ghost"
            title="全局开关：影响所有项目和任务"
            onClick={onToggle}
          >
            {agent.enabled ? "全局停用" : "全局启用"}
          </button>
          <button className="ghost danger-text" disabled={busy} onClick={onRemove}>
            删除
          </button>
        </div>
        <div className="badge-row">
          <Badge tone="info">{agent.kind}</Badge>
          <Badge tone={agent.enabled ? "success" : "muted"}>
            {agent.enabled ? "全局已启用" : "全局已停用"}
          </Badge>
          <Badge tone="muted">并发 {agent.max_concurrency}</Badge>
          {agent.allow_priority_fallback ? (
            <Badge
              tone="info"
              title="该 Agent 并发已满时，新作业交给同角色下一优先级 Agent"
            >
              可让位
            </Badge>
          ) : null}
          {agent.usage_window?.exceeded ? (
            <Badge tone="danger">用量限额已触发</Badge>
          ) : null}
          <div className="agent-status">
            {failureStatus ? (
              <Badge tone={failureStatus.tone} title={failureStatus.title}>
                {failureStatus.label}
              </Badge>
            ) : (
              <Badge tone="success">运行正常</Badge>
            )}
          </div>
        </div>
      </div>
      <div className="agent-meta">
        <div>
          <span className="meta-label">模型</span>
          <span className="mono">{agent.model || "CLI 默认"}</span>
        </div>
        <div>
          <span className="meta-label">角色</span>
          <span>
            {agent.roles.map((role) => ROLE_TEXT[role] ?? role).join(" / ")}
          </span>
        </div>
        <div>
          <span className="meta-label">更新</span>
          <span>{formatTime(agent.updated_at)}</span>
        </div>
      </div>
      {agent.availability?.enabled ? (
        <div className="usage-line">
          <span className="meta-label">可用时段</span>
          <span>{scheduleSummary(agent.availability)}</span>
        </div>
      ) : null}
      {agent.usage_limit?.enabled && agent.usage_window ? (
        <div className={agent.usage_window.exceeded ? "warn-banner" : "usage-line"}>
          <span>
            {WINDOW_LABELS[agent.usage_window.window]} ·
            {METRIC_LABELS[agent.usage_window.metric]} 用量{" "}
            <strong>
              {formatUsageAmount(agent.usage_window.used, agent.usage_window.metric)}
            </strong>{" "}
            / {formatUsageAmount(agent.usage_window.limit, agent.usage_window.metric)}
            {agent.usage_window.exceeded
              ? ` · 已跳过，${formatTime(agent.usage_window.window_end)} 后自动恢复`
              : ""}
          </span>
          <button className="ghost" disabled={busy} onClick={onResetUsage}>
            重置窗口用量
          </button>
        </div>
      ) : null}
      {agent.last_error ? (
        <details className="raw-error">
          <summary>查看原始错误</summary>
          <div className="error-box">
            <RichContent content={agent.last_error} />
          </div>
        </details>
      ) : null}
    </article>
  );
}

function scheduleSummary(availability: AgentAvailability): string {
  if (availability.slots.length === 0) return "未设置任何时段（始终不可用）";
  return availability.slots
    .map(
      (slot) =>
        `${DAY_LABELS[slot.weekday - 1] ?? `周${slot.weekday}`} ${slot.start}-${slot.end}`,
    )
    .join("、");
}
