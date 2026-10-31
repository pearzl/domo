import { useEffect, useMemo, useState } from "react";
import type { DragEvent, KeyboardEvent } from "react";
import type { ProjectRolePool, Role } from "../types";
import {
  Badge,
  ErrorBox,
  FAILURE_KIND_LABELS,
  failureSummaryLabel,
  formatTime,
  formatUsageAmount,
  shortText,
} from "../ui";
import { nextRoleTab, resolveActiveRole, roleTabs } from "../presentation/agentPool";

export interface AgentPoolPanelProps {
  /** 面板标题，例如"项目 Agent 池"或"默认团队"。 */
  title: string;
  /** 标题下方的说明文字。 */
  subtitle: string;
  roles: ProjectRolePool[];
  disabled?: boolean;
  /** 面板控制范围：默认团队、项目池或任务池，决定停用提示与按钮可用性。 */
  scope?: "default" | "project" | "task";
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
  /** 期望默认选中的角色；缺省或不存在时回落到第一个有 Agent 的角色。 */
  activeRole?: Role;
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
  scope = "project",
  onMove,
  onReorder,
  onToggle,
  onRefresh,
  defaultOpen,
  activeRole,
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

  // 角色选项卡只承载选择状态；角色数据仍完整保存在 roles 中，非当前角色只是不渲染。
  const tabs = useMemo(() => roleTabs(roles), [roles]);
  const defaultRole = useMemo(
    () => resolveActiveRole(tabs, activeRole),
    [tabs, activeRole],
  );
  const [selectedRole, setSelectedRole] = useState<Role | null>(defaultRole);
  useEffect(() => {
    setSelectedRole(defaultRole);
  }, [defaultRole]);

  const currentRole = resolveActiveRole(tabs, selectedRole ?? activeRole);
  const currentGroup = roles.find((group) => group.role === currentRole) ?? null;

  function selectRole(role: Role | null) {
    if (!role || role === currentRole) {
      return;
    }
    endDrag();
    setSelectedRole(role);
  }

  function onTabListKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") {
      return;
    }
    // 方向键在 tablist 内只切换角色：阻止默认行为，避免页面横向滚动或触发提交。
    event.preventDefault();
    const direction = event.key === "ArrowRight" ? "next" : "previous";
    selectRole(nextRoleTab(tabs, currentRole, direction));
  }

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
      <div
        className="agent-pool-tabs"
        role="tablist"
        aria-label={`${title}角色`}
        onKeyDown={onTabListKeyDown}
      >
        {tabs.map((tab) => {
          const selected = tab.role === currentRole;
          return (
            <button
              key={tab.role}
              type="button"
              role="tab"
              id={`agent-pool-tab-${tab.role}`}
              aria-selected={selected}
              aria-controls={`agent-pool-panel-${tab.role}`}
              tabIndex={selected ? 0 : -1}
              className={`agent-pool-tab${selected ? " active" : ""}`}
              onClick={() => selectRole(tab.role)}
            >
              <span className="agent-pool-tab-label">{tab.label}</span>
              <span className="agent-pool-tab-counts muted">
                {tab.count} 个
                {tab.unavailableCount > 0 ? ` · ${tab.unavailableCount} 不可用` : ""}
              </span>
            </button>
          );
        })}
      </div>
      {currentGroup ? (
        <div
          className="agent-pool-role"
          role="tabpanel"
          id={`agent-pool-panel-${currentGroup.role}`}
          aria-labelledby={`agent-pool-tab-${currentGroup.role}`}
          onDragLeave={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
              setDrop(null);
            }
          }}
        >
          {currentGroup.entries.length === 0 ? (
            <div className="muted">这个角色还没有 Agent。</div>
          ) : (
            <div className="agent-pool-list">
              {currentGroup.entries.map((entry, index) => {
                const globallyBlocked = !entry.agent_enabled;
                const projectBlocked = scope === "task" && !entry.project_enabled;
                const upperBlocked = globallyBlocked || projectBlocked;
                const isDragSource =
                  dragEnabled &&
                  drag?.role === currentGroup.role &&
                  drag.agentId === entry.agent_id;
                const dropMark =
                  dragEnabled && drop?.role === currentGroup.role && drop.index === index
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
                        role: currentGroup.role,
                        agentId: entry.agent_id,
                      };
                      setDrag(item);
                      event.dataTransfer.effectAllowed = "move";
                      event.dataTransfer.setData("text/plain", entry.agent_id);
                    }}
                    onDragEnd={endDrag}
                    onDragOver={(event: DragEvent<HTMLDivElement>) => {
                      if (!drag || !onReorder || drag.role !== currentGroup.role) {
                        return;
                      }
                      event.preventDefault();
                      event.dataTransfer.dropEffect = "move";
                      const rect = event.currentTarget.getBoundingClientRect();
                      const edge =
                        event.clientY < rect.top + rect.height / 2
                          ? "before"
                          : "after";
                      setDrop({ role: currentGroup.role, index, edge });
                    }}
                    onDrop={(event: DragEvent<HTMLDivElement>) => {
                      event.preventDefault();
                      const item = drag;
                      const target = drop;
                      endDrag();
                      if (!item || !target || !onReorder) {
                        return;
                      }
                      const from = currentGroup.entries.findIndex(
                        (it) => it.agent_id === item.agentId,
                      );
                      const position = resolveDropPosition(from, target);
                      if (position === null) {
                        return;
                      }
                      void act(() => onReorder(currentGroup.role, item.agentId, position));
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
                      {globallyBlocked ? (
                        <Badge tone="danger" title="在 Agent 团队页全局停用">
                          全局已停用
                        </Badge>
                      ) : null}
                      {projectBlocked ? (
                        <Badge tone="warn" title="项目 Agent 池已停用该 Agent">
                          项目已停用
                        </Badge>
                      ) : null}
                      {!entry.enabled ? (
                        <Badge tone="warn">
                          {scope === "task" ? "任务池已停用" : "已停用"}
                        </Badge>
                      ) : null}
                      {entry.enabled ? (
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
                        type="button"
                        className="ghost"
                        disabled={disabled || busy || index === 0}
                        title="上移"
                        onClick={() =>
                          act(() => onMove(currentGroup.role, entry.agent_id, "up"))
                        }
                      >
                        ↑
                      </button>
                      <button
                        type="button"
                        className="ghost"
                        disabled={disabled || busy || index === currentGroup.entries.length - 1}
                        title="下移"
                        onClick={() =>
                          act(() => onMove(currentGroup.role, entry.agent_id, "down"))
                        }
                      >
                        ↓
                      </button>
                      <button
                        type="button"
                        className={entry.enabled ? "secondary" : "ghost"}
                        disabled={disabled || busy || !entry.available || upperBlocked}
                        title={
                          globallyBlocked
                            ? "全局已停用，请先在 Agent 团队页启用"
                            : projectBlocked
                              ? "项目 Agent 池已停用该 Agent，请先在项目页启用"
                              : undefined
                        }
                        onClick={() =>
                          act(() => onToggle(currentGroup.role, entry.agent_id, !entry.enabled))
                        }
                      >
                        {scope === "task"
                          ? entry.enabled
                            ? "任务停用"
                            : "任务启用"
                          : scope === "project"
                            ? entry.enabled
                              ? "本项目停用"
                              : "本项目启用"
                            : entry.enabled
                              ? "默认团队停用"
                              : "默认团队启用"}
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
      ) : null}
      {error ? <ErrorBox message={error} /> : null}
    </details>
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
