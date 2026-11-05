import { useEffect, useId, useRef } from "react";
import type { ReactNode } from "react";
import type { Agent, AgentAvailability, AgentKind, AgentUsageLimit, Role, UsageMetric, UsageWindowKind } from "../types";
import { InputBox } from "./InputBox";

const ALL_ROLES: Role[] = ["planner", "executor", "verifier"];
const ROLE_TEXT: Record<Role, string> = {
  planner: "规划者",
  executor: "执行者",
  verifier: "验收者",
};
const DAY_LABELS = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
const FOCUSABLE_SELECTOR =
  'a[href], button, input, select, textarea, summary, [tabindex]:not([tabindex="-1"])';

interface SlotState {
  enabled: boolean;
  start: string;
  end: string;
}

export interface AgentFormState {
  name: string;
  kind: AgentKind;
  model: string;
  roles: Role[];
  maxConcurrency: string;
  allowPriorityFallback: boolean;
  enabled: boolean;
  /** 创建时同步追加到现有项目和任务 Agent 池；编辑时固定为 false。 */
  addToExistingPools: boolean;
  command: string;
  modelArgs: string;
  availabilityEnabled: boolean;
  slots: SlotState[];
  usageLimitEnabled: boolean;
  usageMetric: UsageMetric;
  usageWindow: UsageWindowKind;
  usageLimit: string;
}

export function emptySlots(): SlotState[] {
  return DAY_LABELS.map((_, index) => ({
    enabled: false,
    start: index < 5 ? "09:00" : "10:00",
    end: index < 5 ? "18:00" : "16:00",
  }));
}

export function emptyForm(): AgentFormState {
  return {
    name: "",
    kind: "opencode",
    model: "",
    roles: [...ALL_ROLES],
    maxConcurrency: "2",
    allowPriorityFallback: false,
    enabled: true,
    addToExistingPools: false,
    command: "",
    modelArgs: "",
    availabilityEnabled: false,
    slots: emptySlots(),
    usageLimitEnabled: false,
    usageMetric: "tokens",
    usageWindow: "day",
    usageLimit: "100000",
  };
}

/// 校验持久化草稿的形状，字段集合与表单状态一一对应。
export function isAgentFormDraft(value: unknown): value is AgentFormState {
  if (!value || typeof value !== "object") return false;
  const record = value as Record<string, unknown>;
  return (
    typeof record.name === "string" &&
    typeof record.kind === "string" &&
    typeof record.model === "string" &&
    Array.isArray(record.roles) &&
    record.roles.every((role) => ALL_ROLES.includes(role as Role)) &&
    typeof record.maxConcurrency === "string" &&
    typeof record.allowPriorityFallback === "boolean" &&
    typeof record.enabled === "boolean" &&
    typeof record.addToExistingPools === "boolean" &&
    typeof record.command === "string" &&
    typeof record.modelArgs === "string" &&
    typeof record.availabilityEnabled === "boolean" &&
    Array.isArray(record.slots) &&
    record.slots.length === DAY_LABELS.length &&
    record.slots.every(isSlotState) &&
    typeof record.usageLimitEnabled === "boolean" &&
    typeof record.usageMetric === "string" &&
    typeof record.usageWindow === "string" &&
    typeof record.usageLimit === "string"
  );
}

function isSlotState(value: unknown): value is SlotState {
  if (!value || typeof value !== "object") return false;
  const record = value as Record<string, unknown>;
  return (
    typeof record.enabled === "boolean" &&
    typeof record.start === "string" &&
    typeof record.end === "string"
  );
}

export function formForAgent(agent: Agent): AgentFormState {
  const slots = emptySlots();
  agent.availability?.slots.forEach((slot) => {
    if (slot.weekday >= 1 && slot.weekday <= 7) {
      slots[slot.weekday - 1] = {
        enabled: true,
        start: slot.start,
        end: slot.end,
      };
    }
  });
  return {
    name: agent.name,
    kind: agent.kind,
    model: agent.model,
    roles: agent.roles,
    maxConcurrency: String(agent.max_concurrency),
    allowPriorityFallback: Boolean(agent.allow_priority_fallback),
    enabled: agent.enabled,
    addToExistingPools: false,
    command: agent.command.length ? JSON.stringify(agent.command) : "",
    modelArgs: agent.model_args.length ? JSON.stringify(agent.model_args) : "",
    availabilityEnabled: Boolean(agent.availability?.enabled),
    slots,
    usageLimitEnabled: Boolean(agent.usage_limit?.enabled),
    usageMetric: agent.usage_limit?.metric ?? "tokens",
    usageWindow: agent.usage_limit?.window ?? "day",
    usageLimit: agent.usage_limit ? String(agent.usage_limit.limit) : "100000",
  };
}

export interface AgentFormPanelProps {
  /** 正在编辑的 Agent；新建时为 null。 */
  agent: Agent | null;
  form: AgentFormState;
  saving?: boolean;
  error?: string;
  onChange: (next: AgentFormState) => void;
  onSubmit: () => void;
  onClose: () => void;
}

const SECTIONS = [
  { key: "basic", title: "基本" },
  { key: "schedule", title: "调度" },
  { key: "usage", title: "限额" },
  { key: "command", title: "命令" },
] as const;

type SectionKey = (typeof SECTIONS)[number]["key"];

/**
 * Agent 编辑面板：新建与编辑共用同一个侧滑抽屉。
 * 打开时面板贴靠视口右侧并聚焦关闭按钮，关闭后焦点回到触发按钮；
 * Tab 在面板内环绕，Esc 关闭。表单按“基本/调度/限额/命令”分段折叠，长列表下始终整体可见。
 */
export function AgentFormPanel({
  agent,
  form,
  saving = false,
  error,
  onChange,
  onSubmit,
  onClose,
}: AgentFormPanelProps) {
  const titleId = useId();
  const panelRef = useRef<HTMLDivElement | null>(null);
  const closeRef = useRef<HTMLButtonElement | null>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    const active = document.activeElement;
    const overflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    returnFocusRef.current = active instanceof HTMLElement ? active : null;
    closeRef.current?.focus();
    return () => {
      document.body.style.overflow = overflow;
      const target = returnFocusRef.current;
      if (target?.isConnected) target.focus();
      returnFocusRef.current = null;
    };
  }, []);

  function handleKeyDown(event: React.KeyboardEvent<HTMLElement>) {
    if (event.key === "Escape") {
      event.stopPropagation();
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const panel = panelRef.current;
    if (!panel) return;
    const focusable = Array.from(
      panel.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
    ).filter(
      (element) =>
        !element.hasAttribute("disabled") &&
        element.getAttribute("aria-hidden") !== "true" &&
        element.tabIndex !== -1 &&
        element.getClientRects().length > 0,
    );
    if (focusable.length === 0) {
      event.preventDefault();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const current = document.activeElement;
    if (event.shiftKey) {
      if (current === first || !panel.contains(current)) {
        event.preventDefault();
        last.focus();
      }
    } else if (current === last || !panel.contains(current)) {
      event.preventDefault();
      first.focus();
    }
  }

  function update(patch: Partial<AgentFormState>) {
    onChange({ ...form, ...patch });
  }

  function toggleRole(role: Role) {
    onChange({
      ...form,
      roles: form.roles.includes(role)
        ? form.roles.filter((item) => item !== role)
        : [...form.roles, role],
    });
  }

  function updateSlot(index: number, patch: Partial<SlotState>) {
    onChange({
      ...form,
      slots: form.slots.map((slot, position) =>
        position === index ? { ...slot, ...patch } : slot,
      ),
    });
  }

  const sections: Record<SectionKey, ReactNode> = {
    basic: (
      <>
        <div className="form-grid">
          <label>
            <span>名称</span>
            <input
              value={form.name}
              onChange={(event) => update({ name: event.target.value })}
              required
            />
          </label>
          <label>
            <span>类型</span>
            <select
              value={form.kind}
              onChange={(event) => update({ kind: event.target.value as AgentKind })}
            >
              <option value="opencode">opencode CLI</option>
              <option value="codex">Codex CLI</option>
              <option value="workbuddy">WorkBuddy CLI</option>
              <option value="mock">mock（测试）</option>
              <option value="cli">自定义 CLI</option>
            </select>
          </label>
          <label>
            <span>模型（可留空使用 CLI 默认）</span>
            <input
              value={form.model}
              onChange={(event) => update({ model: event.target.value })}
              placeholder="例如 gpt-5-codex、deepseek-v4-pro；留空使用 CLI 默认"
            />
          </label>
          <label>
            <span>最大并发</span>
            <input
              type="number"
              min={1}
              value={form.maxConcurrency}
              onChange={(event) => update({ maxConcurrency: event.target.value })}
            />
          </label>
          <label className="checkbox-label">
            <span>启用</span>
            <input
              type="checkbox"
              checked={form.enabled}
              onChange={(event) => update({ enabled: event.target.checked })}
            />
          </label>
        </div>
        <div className="field">
          <label className="checkbox-label">
            <input
              type="checkbox"
              checked={form.allowPriorityFallback}
              onChange={(event) =>
                update({ allowPriorityFallback: event.target.checked })
              }
            />
            <span>并发已满时让位</span>
          </label>
          <small className="muted">
            开启后该 Agent 达到最大并发时，新作业交给同角色下一优先级 Agent；默认关闭，关闭时排队等待该 Agent 释放。
          </small>
        </div>
        <div className="field">
          <span className="field-label">角色</span>
          <div className="checkbox-row">
            {ALL_ROLES.map((role) => (
              <label key={role} className="checkbox-label">
                <input
                  type="checkbox"
                  checked={form.roles.includes(role)}
                  onChange={() => toggleRole(role)}
                />
                <span>{ROLE_TEXT[role]}</span>
              </label>
            ))}
          </div>
        </div>
        {agent === null ? (
          <div className="field">
            <label className="checkbox-label">
              <input
                type="checkbox"
                checked={form.addToExistingPools}
                onChange={(event) =>
                  update({ addToExistingPools: event.target.checked })
                }
              />
              <span>同时加入所有现有项目和任务的 Agent 池</span>
            </label>
            <small className="muted">
              仅创建时可选：按角色追加到未归档项目的项目池，以及这些项目下未完成、未取消任务的执行者与验收者池末尾。默认关闭。
            </small>
          </div>
        ) : null}
      </>
    ),
    schedule: (
      <div className="field">
        <span className="field-label">可用时段（本地时区）</span>
        <label className="checkbox-label">
          <input
            type="checkbox"
            checked={form.availabilityEnabled}
            onChange={(event) => update({ availabilityEnabled: event.target.checked })}
          />
          <span>按每周时段限制派发</span>
        </label>
        {form.availabilityEnabled ? (
          <div className="schedule-grid">
            {DAY_LABELS.map((day, index) => {
              const slot = form.slots[index];
              return (
                <div className="schedule-row" key={day}>
                  <label className="checkbox-label">
                    <input
                      type="checkbox"
                      checked={slot.enabled}
                      onChange={(event) =>
                        updateSlot(index, { enabled: event.target.checked })
                      }
                    />
                    <span>{day}</span>
                  </label>
                  <input
                    type="time"
                    value={slot.start}
                    disabled={!slot.enabled}
                    onChange={(event) =>
                      updateSlot(index, { start: event.target.value })
                    }
                  />
                  <span className="muted">至</span>
                  <input
                    type="time"
                    value={slot.end}
                    disabled={!slot.enabled}
                    onChange={(event) => updateSlot(index, { end: event.target.value })}
                  />
                </div>
              );
            })}
          </div>
        ) : (
          <small className="muted">未配置时段时全天可用。</small>
        )}
      </div>
    ),
    usage: (
      <div className="field">
        <span className="field-label">用量限额</span>
        <label className="checkbox-label">
          <input
            type="checkbox"
            checked={form.usageLimitEnabled}
            onChange={(event) => update({ usageLimitEnabled: event.target.checked })}
          />
          <span>达到限额后自动暂停派发</span>
        </label>
        {form.usageLimitEnabled ? (
          <div className="form-grid usage-form-grid">
            <label>
              <span>计量维度</span>
              <select
                value={form.usageMetric}
                onChange={(event) =>
                  update({ usageMetric: event.target.value as UsageMetric })
                }
              >
                <option value="tokens">token（输入+输出）</option>
                <option value="input_tokens">输入 token</option>
                <option value="output_tokens">输出 token</option>
                <option value="cost">费用</option>
              </select>
            </label>
            <label>
              <span>统计窗口</span>
              <select
                value={form.usageWindow}
                onChange={(event) =>
                  update({ usageWindow: event.target.value as UsageWindowKind })
                }
              >
                <option value="day">每日</option>
                <option value="month">每月</option>
              </select>
            </label>
            <label>
              <span>限额</span>
              <input
                type="number"
                min={0}
                step="any"
                value={form.usageLimit}
                onChange={(event) => update({ usageLimit: event.target.value })}
              />
            </label>
          </div>
        ) : (
          <small className="muted">未记录用量的作业不会扣减限额。</small>
        )}
      </div>
    ),
    command:
      form.kind === "cli" ? (
        <div className="form-grid">
          <div className="field">
            <span className="field-label">命令模板（JSON 数组）</span>
            <InputBox
              value={form.command}
              onChange={(next) => update({ command: next })}
              rows={3}
              maxAutoGrowChars={420}
              placeholder={'["codex","exec","--json","{model_args}","-"]'}
            />
            <small>
              支持占位符：{"{prompt}"}、{"{job_dir}"}、{"{cwd}"}、{"{model}"}、
              {"{model_args}"}。没有 {"{prompt}"} 时提示词走 stdin。
            </small>
          </div>
          <div className="field">
            <span className="field-label">模型参数模板（JSON 数组）</span>
            <InputBox
              value={form.modelArgs}
              onChange={(next) => update({ modelArgs: next })}
              rows={3}
              maxAutoGrowChars={320}
              placeholder={'["-m","{model}"]'}
            />
          </div>
        </div>
      ) : (
        <p className="muted">
          当前类型使用内置命令模板；把类型切换为“自定义 CLI”后可在这里填写命令模板与模型参数。
        </p>
      ),
  };

  return (
    <div className="drawer-backdrop agent-form-backdrop" onClick={onClose}>
      <aside
        className="drawer agent-form-drawer"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        ref={panelRef}
        onKeyDown={handleKeyDown}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="drawer-header">
          <div>
            <div className="muted">Agent</div>
            <h2 id={titleId}>{agent ? `编辑 ${agent.name}` : "添加 Agent"}</h2>
          </div>
          <button className="ghost" type="button" ref={closeRef} onClick={onClose}>
            关闭
          </button>
        </div>
        <form className="agent-form" onSubmit={(event) => { event.preventDefault(); onSubmit(); }}>
          <div className="drawer-body agent-form-body">
            {error ? <div className="error-box" role="alert">{error}</div> : null}
            {SECTIONS.map((section) => (
              <details className="agent-form-section" key={section.key} open>
                <summary>{section.title}</summary>
                <div className="agent-form-section-body">{sections[section.key]}</div>
              </details>
            ))}
          </div>
          <div className="form-actions agent-form-actions">
            <button className="primary" type="submit" disabled={saving}>
              {saving ? "保存中…" : "保存"}
            </button>
            <button className="ghost" type="button" onClick={onClose}>
              取消
            </button>
          </div>
        </form>
      </aside>
    </div>
  );
}

export function buildAvailability(form: AgentFormState): AgentAvailability | null {
  if (!form.availabilityEnabled) return null;
  const slots = form.slots.flatMap((slot, index) =>
    slot.enabled
      ? [{ weekday: index + 1, start: slot.start, end: slot.end }]
      : [],
  );
  if (slots.length === 0) {
    throw new Error("启用可用时段时至少选择一天");
  }
  return { enabled: true, slots };
}

export function buildUsageLimit(form: AgentFormState): AgentUsageLimit | null {
  if (!form.usageLimitEnabled) return null;
  const limit = Number(form.usageLimit);
  if (!Number.isFinite(limit) || limit <= 0) {
    throw new Error("启用用量限额时限额必须大于 0");
  }
  return {
    enabled: true,
    metric: form.usageMetric,
    window: form.usageWindow,
    limit,
  };
}

export function parseJsonArray(raw: string): string[] {
  const value = raw.trim();
  if (!value) return [];
  const parsed = JSON.parse(value) as unknown;
  if (!Array.isArray(parsed) || parsed.some((item) => typeof item !== "string")) {
    throw new Error("命令模板必须是字符串数组");
  }
  return parsed;
}
