import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import type { Agent, AgentInput, AgentKind, Role } from "../types";
import { api, errorMessage } from "../api";
import { Badge, ErrorBox, formatTime } from "../ui";

const ALL_ROLES: Role[] = ["planner", "executor", "verifier"];
const ROLE_TEXT: Record<Role, string> = {
  planner: "规划者",
  executor: "执行者",
  verifier: "验收者",
};

interface FormState {
  name: string;
  kind: AgentKind;
  model: string;
  roles: Role[];
  priority: string;
  maxConcurrency: string;
  enabled: boolean;
  command: string;
  modelArgs: string;
}

const emptyForm: FormState = {
  name: "",
  kind: "opencode",
  model: "",
  roles: [...ALL_ROLES],
  priority: "0",
  maxConcurrency: "2",
  enabled: true,
  command: "",
  modelArgs: "",
};

export function AgentsPage() {
  const [agents, setAgents] = useState<Agent[]>([]);
  const [error, setError] = useState("");
  const [form, setForm] = useState<FormState>(emptyForm);
  const [editing, setEditing] = useState<Agent | null>(null);
  const [showForm, setShowForm] = useState(false);
  const [saving, setSaving] = useState(false);

  async function refresh() {
    try {
      setAgents(await api.listAgents());
      setError("");
    } catch (err) {
      setError(errorMessage(err));
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  function startCreate() {
    setEditing(null);
    setForm(emptyForm);
    setShowForm(true);
  }

  function startEdit(agent: Agent) {
    setEditing(agent);
    setForm({
      name: agent.name,
      kind: agent.kind,
      model: agent.model,
      roles: agent.roles,
      priority: String(agent.priority),
      maxConcurrency: String(agent.max_concurrency),
      enabled: agent.enabled,
      command: agent.command.length ? JSON.stringify(agent.command) : "",
      modelArgs: agent.model_args.length ? JSON.stringify(agent.model_args) : "",
    });
    setShowForm(true);
  }

  async function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSaving(true);
    setError("");
    try {
      const input: AgentInput = {
        name: form.name.trim(),
        kind: form.kind,
        model: form.model.trim(),
        roles: form.roles,
        priority: Number(form.priority) || 0,
        max_concurrency: Number(form.maxConcurrency) || 1,
        enabled: form.enabled,
        command: parseJsonArray(form.command),
        model_args: parseJsonArray(form.modelArgs),
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

  function toggleRole(role: Role) {
    setForm((current) => ({
      ...current,
      roles: current.roles.includes(role)
        ? current.roles.filter((item) => item !== role)
        : [...current.roles, role],
    }));
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>Agent 团队</h1>
          <p className="subtitle">
            同一 Agent 可以扮演多个角色。按优先级从小到大选择，数字越小越优先。
          </p>
        </div>
        <button className="primary" onClick={startCreate}>
          添加 Agent
        </button>
      </header>

      {error ? <ErrorBox message={error} /> : null}

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
                <option value="mock">mock（测试）</option>
                <option value="cli">自定义 CLI</option>
              </select>
            </label>
            <label>
              <span>模型（可留空使用 CLI 默认）</span>
              <input
                value={form.model}
                onChange={(event) => setForm({ ...form, model: event.target.value })}
                placeholder="例如 opencode-go/deepseek-v4-pro"
              />
            </label>
            <label>
              <span>优先级</span>
              <input
                type="number"
                value={form.priority}
                onChange={(event) => setForm({ ...form, priority: event.target.value })}
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
        {agents.map((agent) => (
          <article key={agent.id} className={`agent-card panel ${agent.enabled ? "" : "disabled"}`}>
            <div className="agent-head">
              <div>
                <h2>{agent.name}</h2>
                <div className="badge-row">
                  <Badge tone="info">{agent.kind}</Badge>
                  <Badge tone={agent.enabled ? "success" : "muted"}>
                    {agent.enabled ? "已启用" : "已禁用"}
                  </Badge>
                  <Badge tone="muted">优先级 {agent.priority}</Badge>
                  <Badge tone="muted">并发 {agent.max_concurrency}</Badge>
                </div>
              </div>
              <div className="button-row">
                <button className="ghost" onClick={() => startEdit(agent)}>
                  编辑
                </button>
                <button className="ghost" onClick={() => void toggle(agent)}>
                  {agent.enabled ? "禁用" : "启用"}
                </button>
                <button
                  className="ghost danger-text"
                  onClick={() => {
                    if (window.confirm(`删除 Agent「${agent.name}」？`)) {
                      void api
                        .deleteAgent(agent.id)
                        .then(refresh)
                        .catch((err) => setError(errorMessage(err)));
                    }
                  }}
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
            {agent.cooldown_until ? (
              <div className="warn-banner">限流冷却至 {formatTime(agent.cooldown_until)}</div>
            ) : null}
            {agent.last_error ? (
              <div className="error-box pre-wrap">{agent.last_error}</div>
            ) : null}
          </article>
        ))}
      </div>
    </div>
  );
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
