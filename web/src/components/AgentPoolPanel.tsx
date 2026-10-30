import { useEffect, useState } from "react";
import type { DragEvent } from "react";
import type { ProjectRolePool, Role } from "../types";
import {
  Badge,
  ErrorBox,
  FAILURE_KIND_LABELS,
  ROLE_LABELS,
  failureSummaryLabel,
  formatTime,
  formatUsageAmount,
  shortText,
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
  /** 把条目移动到该角色内 0 基目标位置；拖动排序经由这里持久化。 */
  onReorder?: (role: Role, agentId: string, position: number) => Promise<unknown>;
  /** 切换条目启停。 */
  onToggle: (role: Role, agentId: string, enabled: boolean) => Promise<unknown>;
  /** 任一操作成功后刷新数据。 */
  onRefresh?: () => Promise<unknown>;
  /** 初始展开状态。 */
  defaultOpen?: boolean;
}

interface DragItem {
  role: Role;
  agentId: string;
}

interface DropTarget {
  role: Role;
  index: number;
  edge: "before" | "after";
}

/** 落点换算成移动后的 0 基目标位置；落点不改变顺序时返回 null。 */
function resolveDropPosition(from: number, target: DropTarget): number | null {
  const { index, edge } = target;
  if (index === from) {
    return null;
  }
  if (edge === "before") {
    if (index > from) {
      return index - 1 === from ? null : index - 1;
    }
    return index;
  }
  if (index < from) {
    return index + 1 === from ? null : index + 1;
  }
  return index;
}

export function AgentPoolPanel({
  title,
  subtitle,
  roles,
  disabled = false,
  onMove,
  onReorder,
  onToggle,
  onRefresh,
  defaultOpen,
}: AgentPoolPanelProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [drag, setDrag] = useState<DragItem | null>(null);
  const [drop, setDrop] = useState<DropTarget | null>(null);
  const initialOpen = defaultOpen ?? false;
  const [open, setOpen] = useState(initialOpen);
  useEffect(() => {
    setOpen(initialOpen);
  }, [initialOpen]);

  async function act(action: () => Promise<unknown>) {
    setBusy(true);
    setError("");
    try {
      await action();
      await onRefresh?.();
    } catch (err) {
      setError(errorMessage(err));
      // 操作失败后回到服务端顺序，避免面板显示与持久化结果不一致。
      try {
        await onRefresh?.();
      } catch {
        // 刷新失败时保留操作错误提示。
      }
    } finally {
      setBusy(false);
    }
  }

  function endDrag() {
    setDrag(null);
    setDrop(null);
  }

  const dragEnabled = Boolean(onReorder) && !disabled && !busy;

  return (
    <details
      className="panel agent-pool"
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>{title}</summary>
      <p className="subtitle">{subtitle}</p>
      {roles.map((group) => {
        return (
          <div
            className="agent-pool-role"
            key={group.role}
            onDragLeave={(event) => {
              if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
                setDrop(null);
              }
            }}
          >
            <h3>{ROLE_LABELS[group.role] ?? group.role}</h3>
            {group.entries.length === 0 ? (
              <div className="muted">这个角色还没有 Agent。</div>
            ) : (
              <div className="agent-pool-list">
                {group.entries.map((entry, index) => {
                  const isDragSource =
                    dragEnabled &&
                    drag?.role === group.role &&
                    drag.agentId === entry.agent_id;
                  const dropMark =
                    dragEnabled &&
                    drop?.role === group.role &&
                    drop.index === index
                      ? drop.edge
                      : null;
                  return (
                  <div
                    className={`agent-pool-row${entry.available ? "" : " unavailable"}${
                      isDragSource ? " dragging" : ""
                    }${dropMark === "before" ? " drop-above" : ""}${
                      dropMark === "after" ? " drop-below" : ""
                    }`}
                    key={entry.agent_id}
                    draggable={dragEnabled}
                    title={dragEnabled ? "拖动调整顺序，或使用上移/下移按钮" : undefined}
                    onDragStart={(event: DragEvent<HTMLDivElement>) => {
                      const item: DragItem = {
                        role: group.role,
                        agentId: entry.agent_id,
                      };
                      setDrag(item);
                      event.dataTransfer.effectAllowed = "move";
                      event.dataTransfer.setData("text/plain", entry.agent_id);
                    }}
                    onDragEnd={endDrag}
                    onDragOver={(event: DragEvent<HTMLDivElement>) => {
                      if (!drag || !onReorder || drag.role !== group.role) {
                        return;
                      }
                      event.preventDefault();
                      event.dataTransfer.dropEffect = "move";
                      const rect = event.currentTarget.getBoundingClientRect();
                      const edge =
                        event.clientY < rect.top + rect.height / 2
                          ? "before"
                          : "after";
                      setDrop({ role: group.role, index, edge });
                    }}
                    onDrop={(event: DragEvent<HTMLDivElement>) => {
                      event.preventDefault();
                      const item = drag;
                      const target = drop;
                      endDrag();
                      if (!item || !target || !onReorder) {
                        return;
                      }
                      const from = group.entries.findIndex(
                        (it) => it.agent_id === item.agentId,
                      );
                      const position = resolveDropPosition(from, target);
                      if (position === null) {
                        return;
                      }
                      void act(() => onReorder(group.role, item.agentId, position));
                    }}
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
                          title={`已用 ${formatUsageAmount(entry.usage_window.used, entry.usage_window.metric)} / 限额 ${formatUsageAmount(entry.usage_window.limit, entry.usage_window.metric)}`}
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
                    </span>
                    {entry.unavailable_reason ? (
                      <span className="agent-pool-reason muted">
                        {entry.unavailable_reason}
                      </span>
                    ) : entry.failure_summary ? (
                      <span className="agent-pool-reason muted">
                        {failureSummaryLabel(entry.failure_summary.kind)} ·{" "}
                        {shortText(entry.failure_summary.message, 100)}
                      </span>
                    ) : null}
                  </div>
                  );
                })}
              </div>
            )}
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
