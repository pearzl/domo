import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import type {
  Agent,
  AgentAvailability,
  AgentInput,
  AgentKind,
  AgentUsageLimit,
  DefaultTeamPool,
  Role,
  UsageMetric,
  UsageWindowKind,
} from "../types";
import { api, errorMessage } from "../api";
import { AgentPoolPanel } from "../components/AgentPoolPanel";
import { RichContent } from "../components/RichContent";
import {
  Badge,
  Empty,
  ErrorBox,
  FAILURE_KIND_LABELS,
  failureSummaryLabel,
  formatTime,
  formatUsageAmount,
  shortText,
} from "../ui";

const ALL_ROLES: Role[] = ["planner", "executor", "verifier"];
const ROLE_TEXT: Record<Role, string> = {
  planner: "规划者",
  executor: "执行者",
  verifier: "验收者",
};
const DAY_LABELS = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
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

interface SlotState {
  enabled: boolean;
  start: string;
  end: string;
}

interface FormState {
  name: string;
  kind: AgentKind;
  model: string;
  roles: Role[];
  maxConcurrency: string;
  enabled: boolean;
  command: string;
  modelArgs: string;
  availabilityEnabled: boolean;
  slots: SlotState[];
  usageLimitEnabled: boolean;
  usageMetric: UsageMetric;
  usageWindow: UsageWindowKind;
  usageLimit: string;
}

function emptySlots(): SlotState[] {
  return DAY_LABELS.map((_, index) => ({
    enabled: false,
    start: index < 5 ? "09:00" : "10:00",
    end: index < 5 ? "18:00" : "16:00",
  }));
}

function emptyForm(): FormState {
  return {
    name: "",
    kind: "opencode",
    model: "",
    roles: [...ALL_ROLES],
    maxConcurrency: "2",
    enabled: true,
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

export function AgentsPage() {
  const [agents, setAgents] = useState<Agent[]>([]);
  const [team, setTeam] = useState<DefaultTeamPool | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [form, setForm] = useState<FormState>(emptyForm);
  const [editing, setEditing] = useState<Agent | null>(null);
  const [showForm, setShowForm] = useState(false);
  const [saving, setSaving] = useState(false);
  const [busyId, setBusyId] = useState("");

  async function refresh() {
    try {
      const [agentList, defaultTeam] = await Promise.all([
        api.listAgents(),
        api.getDefaultTeam(),
      ]);
      setAgents(agentList);
      setTeam(defaultTeam);
      setError("");
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  function startCreate() {
    setEditing(null);
    setForm(emptyForm());
    setShowForm(true);
  }

  function startEdit(agent: Agent) {
    setEditing(agent);
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
    setForm({
      name: agent.name,
      kind: agent.kind,
      model: agent.model,
      roles: agent.roles,
      maxConcurrency: String(agent.max_concurrency),
      enabled: agent.enabled,
      command: agent.command.length ? JSON.stringify(agent.command) : "",
      modelArgs: agent.model_args.length ? JSON.stringify(agent.model_args) : "",
      availabilityEnabled: Boolean(agent.availability?.enabled),
      slots,
      usageLimitEnabled: Boolean(agent.usage_limit?.enabled),
      usageMetric: agent.usage_limit?.metric ?? "tokens",
      usageWindow: agent.usage_limit?.window ?? "day",
      usageLimit: agent.usage_limit ? String(agent.usage_limit.limit) : "100000",
    });
    setShowForm(true);
  }

  async function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSaving(true);
    setError("");
    try {
      const availability = buildAvailability(form);
      const usageLimit = buildUsageLimit(form);
      const input: AgentInput = {
        name: form.name.trim(),
        kind: form.kind,
        model: form.model.trim(),
        roles: form.roles,
        max_concurrency: Number(form.maxConcurrency) || 1,
        enabled: form.enabled,
        command: parseJsonArray(form.command),
        model_args: parseJsonArray(form.modelArgs),
        availability,
        usage_limit: usageLimit,
      };
      if (editing) {
        await api.updateAgent(editing.id, input);
      } else {
        await api.createAgent(input);
      }
      setShowForm(false);
      await refresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  }

  async function toggle(agent: Agent) {
    try {
      await api.updateAgent(agent.id, { enabled: !agent.enabled });
      await refresh();
    } catch (err) {
      setError(errorMessage(err));
    }
  }

  async function resetUsage(agent: Agent) {
    setBusyId(agent.id);
    setError("");
    try {
      await api.resetAgentUsage(agent.id);
      await refresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusyId("");
    }
  }

  async function removeAgent(agent: Agent) {
    if (!window.confirm(`删除 Agent「${agent.name}」？`)) return;
    setBusyId(agent.id);
    setError("");
    try {
      await api.deleteAgent(agent.id);
      await refresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusyId("");
    }
  }

  function toggleRole(role: Role) {
    setForm((current) => ({
      ...current,
      roles: current.roles.includes(role)
        ? current.roles.filter((item) => item !== role)
        : [...current.roles, role],
    }));
  }

  function updateSlot(index: number, patch: Partial<SlotState>) {
    setForm((current) => ({
      ...current,
      slots: current.slots.map((slot, position) =>
        position === index ? { ...slot, ...patch } : slot,
      ),
    }));
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>Agent 团队</h1>
          <p className="subtitle">
            同一 Agent 可以扮演多个角色。卡片上的启停是全局开关，影响所有项目与任务；
            默认团队决定新项目的初始 Agent 池。可用时段按本地时区生效，用量限额达到后自动跳过，新窗口开始后自动恢复。
          </p>
        </div>
        <button className="primary" onClick={startCreate}>
          添加 Agent
        </button>
      </header>

      {error ? <ErrorBox message={error} /> : null}

      {loading ? (
        <section className="panel">
          <div className="muted">正在加载 Agent…</div>
        </section>
      ) : null}

      {team ? (
        <AgentPoolPanel
          title="默认团队"
          subtitle="系统级团队模板：全部已注册 Agent 按角色出现在默认团队中，顺序越靠前优先级越高。新建项目的项目 Agent 池按这里的角色、顺序与启停状态快照初始化，之后项目独立维护。"
          roles={team.roles}
          scope="default"
          defaultOpen
          onReorder={(role, agentId, position) =>
            api.updateDefaultTeamAgent(role, agentId, { position })
          }
          onMove={(role, agentId, direction) =>
            api.updateDefaultTeamAgent(role, agentId, { direction })
          }
          onToggle={(role, agentId, enabled) =>
            api.updateDefaultTeamAgent(role, agentId, { enabled })
          }
          onRefresh={() => refresh()}
        />
      ) : null}

      {showForm ? (
        <form className="panel form" onSubmit={handleSubmit}>
          <div className="form-grid">
            <label>
              <span>名称</span>
              <input
                value={form.name}
                onChange={(event) => setForm({ ...form, name: event.target.value })}
                required
              />
            </label>
            <label>
              <span>类型</span>
              <select
                value={form.kind}
                onChange={(event) =>
                  setForm({ ...form, kind: event.target.value as AgentKind })
                }
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
                onChange={(event) => setForm({ ...form, model: event.target.value })}
                placeholder="例如 gpt-5-codex、deepseek-v4-pro；留空使用 CLI 默认"
              />
            </label>
            <label>
              <span>最大并发</span>
              <input
                type="number"
                min={1}
                value={form.maxConcurrency}
                onChange={(event) =>
                  setForm({ ...form, maxConcurrency: event.target.value })
                }
              />
            </label>
            <label className="checkbox-label">
              <span>启用</span>
              <input
                type="checkbox"
                checked={form.enabled}
                onChange={(event) => setForm({ ...form, enabled: event.target.checked })}
              />
            </label>
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

          <div className="field">
            <span className="field-label">可用时段（本地时区）</span>
            <label className="checkbox-label">
              <input
                type="checkbox"
                checked={form.availabilityEnabled}
                onChange={(event) =>
                  setForm({ ...form, availabilityEnabled: event.target.checked })
                }
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
                        onChange={(event) =>
                          updateSlot(index, { end: event.target.value })
                        }
                      />
                    </div>
                  );
                })}
              </div>
            ) : (
              <small className="muted">未配置时段时全天可用。</small>
            )}
          </div>

          <div className="field">
            <span className="field-label">用量限额</span>
            <label className="checkbox-label">
              <input
                type="checkbox"
                checked={form.usageLimitEnabled}
                onChange={(event) =>
                  setForm({ ...form, usageLimitEnabled: event.target.checked })
                }
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
                      setForm({
                        ...form,
                        usageMetric: event.target.value as UsageMetric,
                      })
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
                      setForm({
                        ...form,
                        usageWindow: event.target.value as UsageWindowKind,
                      })
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
                    onChange={(event) =>
                      setForm({ ...form, usageLimit: event.target.value })
                    }
                  />
                </label>
              </div>
            ) : (
              <small className="muted">
                未记录用量的作业不会扣减限额。
              </small>
            )}
          </div>

          {form.kind === "cli" ? (
            <div className="form-grid">
              <label>
                <span>命令模板（JSON 数组）</span>
                <textarea
                  value={form.command}
                  onChange={(event) => setForm({ ...form, command: event.target.value })}
                  rows={3}
                  placeholder={'["codex","exec","--json","{model_args}","-"]'}
                />
                <small>
                  支持占位符：{"{prompt}"}、{"{job_dir}"}、{"{cwd}"}、{"{model}"}、
                  {"{model_args}"}。没有 {"{prompt}"} 时提示词走 stdin。
                </small>
              </label>
              <label>
                <span>模型参数模板（JSON 数组）</span>
                <textarea
                  value={form.modelArgs}
                  onChange={(event) => setForm({ ...form, modelArgs: event.target.value })}
                  rows={3}
                  placeholder={'["-m","{model}"]'}
                />
              </label>
            </div>
          ) : null}

          <div className="form-actions">
            <button className="primary" type="submit" disabled={saving}>
              {saving ? "保存中…" : "保存"}
            </button>
            <button className="ghost" type="button" onClick={() => setShowForm(false)}>
              取消
            </button>
          </div>
        </form>
      ) : null}

      <div className="agent-grid">
        {!loading && agents.length === 0 ? (
          <Empty
            title="还没有 Agent"
            hint="点击右上角「添加 Agent」创建第一个 Agent，之后可在默认团队中调整顺序与启停，新建项目会自动继承。"
          />
        ) : (
          agents.map((agent) => (
          <article key={agent.id} className={`agent-card panel ${agent.enabled ? "" : "disabled"}`}>
            <div className="agent-head">
              <div>
                <h2>{agent.name}</h2>
                <div className="badge-row">
                  <Badge tone="info">{agent.kind}</Badge>
                  <Badge tone={agent.enabled ? "success" : "muted"}>
                    {agent.enabled ? "全局已启用" : "全局已停用"}
                  </Badge>
                  <Badge tone="muted">并发 {agent.max_concurrency}</Badge>
                  {agent.usage_window?.exceeded ? (
                    <Badge tone="danger">用量限额已触发</Badge>
                  ) : null}
                </div>
              </div>
              <div className="button-row">
                <button className="ghost" onClick={() => startEdit(agent)}>
                  编辑
                </button>
                <button
                  className="ghost"
                  title="全局开关：影响所有项目和任务"
                  onClick={() => void toggle(agent)}
                >
                  {agent.enabled ? "全局停用" : "全局启用"}
                </button>
                <button
                  className="ghost danger-text"
                  disabled={busyId === agent.id}
                  onClick={() => void removeAgent(agent)}
                >
                  删除
                </button>
              </div>
            </div>
            <div className="agent-meta">
              <div>
                <span className="meta-label">模型</span>
                <span className="mono">{agent.model || "CLI 默认"}</span>
              </div>
              <div>
                <span className="meta-label">角色</span>
                <span>{agent.roles.map((role) => ROLE_TEXT[role] ?? role).join(" / ")}</span>
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
                  /{" "}
                  {formatUsageAmount(agent.usage_window.limit, agent.usage_window.metric)}
                  {agent.usage_window.exceeded
                    ? ` · 已跳过，${formatTime(agent.usage_window.window_end)} 后自动恢复`
                    : ""}
                </span>
                <button
                  className="ghost"
                  disabled={busyId === agent.id}
                  onClick={() => void resetUsage(agent)}
                >
                  重置窗口用量
                </button>
              </div>
            ) : null}
            {agent.failure_kind ? (
              <div className="warn-banner">
                最近失败分类：
                {FAILURE_KIND_LABELS[agent.failure_kind] ?? agent.failure_kind}
                {agent.transient_failures
                  ? ` · 供应商波动 ${agent.transient_failures} 次`
                  : ""}
              </div>
            ) : null}
            {agent.cooldown_until ? (
              <div className="warn-banner">
                冷却至 {formatTime(agent.cooldown_until)}
              </div>
            ) : null}
            {agent.failure_summary ? (
              <div className="warn-banner">
                <strong>{failureSummaryLabel(agent.failure_summary.kind)}</strong>
                <span> · {shortText(agent.failure_summary.message, 160)}</span>
                {agent.failure_summary.status !== null
                  ? ` · HTTP ${agent.failure_summary.status}`
                  : ""}
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
          ))
        )}
      </div>
    </div>
  );
}

function buildAvailability(form: FormState): AgentAvailability | null {
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

function buildUsageLimit(form: FormState): AgentUsageLimit | null {
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

function scheduleSummary(availability: AgentAvailability): string {
  if (availability.slots.length === 0) return "未设置任何时段（始终不可用）";
  return availability.slots
    .map(
      (slot) =>
        `${DAY_LABELS[slot.weekday - 1] ?? `周${slot.weekday}`} ${slot.start}-${slot.end}`,
    )
    .join("、");
}

function parseJsonArray(raw: string): string[] {
  const value = raw.trim();
  if (!value) return [];
  const parsed = JSON.parse(value) as unknown;
  if (!Array.isArray(parsed) || parsed.some((item) => typeof item !== "string")) {
    throw new Error("命令模板必须是字符串数组");
  }
  return parsed;
}
