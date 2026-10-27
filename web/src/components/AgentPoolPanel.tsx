import { useState } from "react";
import type { Agent, ProjectRolePool, Role } from "../types";
import {
  Badge,
  ErrorBox,
  FAILURE_KIND_LABELS,
  ROLE_LABELS,
  formatTime,
  formatTokens as formatUsageAmount,
} from "../ui";

export interface AgentPoolPanelProps {
  /** 面板标题，例如"项目 Agent 池"或"默认团队"。 */
  title: string;
  /** 标题下方的说明文字。 */
  subtitle: string;
  roles: ProjectRolePool[];
  disabled?: boolean;
  /** 交换相邻两个 Agent 的顺序。 */
  onMove: (role: Role, agentId: string, direction: "up" | "down") => Promise<unknown>;
  /** 切换条目启停。 */
  onToggle: (role: Role, agentId: string, enabled: boolean) => Promise<unknown>;
  /** 提供时展示移除按钮。 */
  onRemove?: (role: Role, agentId: string) => Promise<unknown>;
  /** 提供时展示加入控件；候选列表为该角色可加入的全局 Agent。 */
  onAdd?: (role: Role, agentId: string) => Promise<unknown>;
  /** 任一操作成功后刷新数据。 */
  onRefresh?: () => Promise<unknown>;
  addCandidates?: (role: Role) => Agent[];
}

export function AgentPoolPanel({
  title,
  subtitle,
  roles,
  disabled = false,
  onMove,
  onToggle,
  onRemove,
  onAdd,
  onRefresh,
  addCandidates,
}: AgentPoolPanelProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [adding, setAdding] = useState<Partial<Record<Role, string>>>({});

  async function act(action: () => Promise<unknown>) {
    setBusy(true);
    setError("");
    try {
      await action();
      await onRefresh?.();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <details className="panel agent-pool" open={typeof onAdd === "function"}>
      <summary>{title}</summary>
      <p className="subtitle">{subtitle}</p>
      {roles.map((group) => {
        const candidates = (addCandidates?.(group.role) ?? []).filter(
          (agent) =>
            !group.entries.some((entry) => entry.agent_id === agent.id),
        );
        return (
          <div className="agent-pool-role" key={group.role}>
            <h3>{ROLE_LABELS[group.role] ?? group.role}</h3>
            {group.entries.length === 0 ? (
              <div className="muted">这个角色还没有 Agent。</div>
            ) : (
              <div className="agent-pool-list">
                {group.entries.map((entry, index) => (
                  <div
                    className={`agent-pool-row${entry.available ? "" : " unavailable"}`}
                    key={entry.agent_id}
                  >
                    <span className="agent-pool-rank">{index + 1}</span>
                    <span className="agent-pool-name">
                      <span>{entry.name}</span>
                      <span className="muted mono">
                        {entry.kind === "missing" ? entry.agent_id : entry.kind}
                        {entry.model ? ` · ${entry.model}` : ""}
                      </span>
                    </span>
                    <span className="badge-row">
                      {!entry.available ? (
                        <Badge tone="danger" title={entry.unavailable_reason ?? ""}>
                          不可用
                        </Badge>
                      ) : null}
                      {entry.available && !entry.enabled ? (
                        <Badge tone="warn">已停用</Badge>
                      ) : null}
                      {entry.available && entry.enabled ? (
                        <Badge tone="success">已启用</Badge>
                      ) : null}
                      {entry.cooldown_until &&
                      new Date(entry.cooldown_until).getTime() > Date.now() ? (
                        <Badge
                          tone="warn"
                          title={`冷却至 ${formatTime(entry.cooldown_until)}`}
                        >
                          冷却中 · {formatTime(entry.cooldown_until)}
                        </Badge>
                      ) : null}
                      {entry.failure_kind ? (
                        <Badge tone="danger">
                          {FAILURE_KIND_LABELS[entry.failure_kind] ?? entry.failure_kind}
                          {entry.transient_failures
                            ? ` · 波动 ${entry.transient_failures} 次`
                            : ""}
                        </Badge>
                      ) : null}
                      {entry.availability_reason ? (
                        <Badge tone="warn" title={entry.availability_reason}>
                          不在可用时段
                        </Badge>
                      ) : null}
                      {entry.usage_window?.exceeded ? (
                        <Badge
                          tone="danger"
                          title={`已用 ${formatUsageAmount(entry.usage_window.used)} / 限额 ${formatUsageAmount(entry.usage_window.limit)}`}
                        >
                          用量限额已触发
                        </Badge>
                      ) : null}
                    </span>
                    <span className="agent-pool-actions">
                      <button
                        className="ghost"
                        disabled={disabled || busy || index === 0}
                        title="上移"
                        onClick={() =>
                          act(() => onMove(group.role, entry.agent_id, "up"))
                        }
                      >
                        ↑
                      </button>
                      <button
                        className="ghost"
                        disabled={disabled || busy || index === group.entries.length - 1}
                        title="下移"
                        onClick={() =>
                          act(() => onMove(group.role, entry.agent_id, "down"))
                        }
                      >
                        ↓
                      </button>
                      <button
                        className={entry.enabled ? "secondary" : "ghost"}
                        disabled={disabled || busy || !entry.available}
                        onClick={() =>
                          act(() => onToggle(group.role, entry.agent_id, !entry.enabled))
                        }
                      >
                        {entry.enabled ? "停用" : "启用"}
                      </button>
                      {onRemove ? (
                        <button
                          className="ghost danger-text"
                          disabled={disabled || busy}
                          title="从团队中移除"
                          onClick={() =>
                            act(() => onRemove(group.role, entry.agent_id))
                          }
                        >
                          移除
                        </button>
                      ) : null}
                    </span>
                    {entry.unavailable_reason ? (
                      <span className="agent-pool-reason muted">
                        {entry.unavailable_reason}
                      </span>
                    ) : entry.last_error ? (
                      <span className="agent-pool-reason muted" title={entry.last_error}>
                        {entry.last_error}
                      </span>
                    ) : null}
                  </div>
                ))}
              </div>
            )}
            {onAdd ? (
              <div className="agent-pool-add">
                <select
                  value={adding[group.role] ?? ""}
                  disabled={disabled || busy || candidates.length === 0}
                  onChange={(event) =>
                    setAdding((current) => ({
                      ...current,
                      [group.role]: event.target.value,
                    }))
                  }
                >
                  <option value="">
                    {candidates.length === 0
                      ? "该角色下没有可加入的 Agent"
                      : "选择要加入的 Agent…"}
                  </option>
                  {candidates.map((agent) => (
                    <option key={agent.id} value={agent.id}>
                      {agent.name}
                    </option>
                  ))}
                </select>
                <button
                  className="ghost"
                  disabled={
                    disabled ||
                    busy ||
                    !(adding[group.role] ?? "") ||
                    candidates.length === 0
                  }
                  onClick={() => {
                    const agentId = adding[group.role] ?? "";
                    if (!agentId) return;
                    setAdding((current) => ({ ...current, [group.role]: "" }));
                    act(() => onAdd(group.role, agentId));
                  }}
                >
                  加入
                </button>
              </div>
            ) : null}
          </div>
        );
      })}
      {error ? <ErrorBox message={error} /> : null}
    </details>
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
