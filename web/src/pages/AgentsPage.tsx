/**
 * Agent 团队页。列表承载全部 Agent 卡片，新建与编辑共用一个侧滑编辑面板，
 * 面板浮在视口上，列表再长也不会把表单挤出可见范围。
 *
 * 人工核对清单（编辑面板始终可得）：
 * 1. 进入 Agent 团队页，创建 6 个以上 Agent，使列表超出一屏。
 * 2. 滚动到列表底部，点击最后一个 Agent 卡片上的「编辑」。
 * 3. 编辑面板从右侧浮出并完整可见，顶部为「基本/调度/限额/命令」四个分段。
 * 4. 直接修改字段并保存，卡片数据立即更新；关闭面板后焦点回到刚才的「编辑」按钮。
 * 5. 点击右上角「添加 Agent」，打开的是同一个面板，分段与字段一致，保存后新增卡片。
 * 6. 面板内按 Tab 只在面板内环绕，Esc 或点击遮罩关闭面板。
 */
import { useEffect, useState } from "react";
import type { Agent, AgentInput, DefaultTeamPool } from "../types";
import { api, errorMessage } from "../api";
import { SkeletonLines } from "../components/Skeleton";
import { AgentCard } from "../components/AgentCard";
import { AgentPoolPanel } from "../components/AgentPoolPanel";
import { GlobalQueue } from "../components/TeamOperations";
import {
  AgentFormPanel,
  buildAvailability,
  buildUsageLimit,
  emptyForm,
  formForAgent,
  isAgentFormDraft,
  parseJsonArray,
} from "../components/AgentFormPanel";
import type { AgentFormState } from "../components/AgentFormPanel";
import {
  browserDraftStorage,
  clearDraft,
  draftStorageKey,
  readDraft,
  writeDraftJson,
} from "../presentation/draft";
import { Empty, ErrorBox, Icon, PageGuide } from "../ui";

// 供其它模块与测试直接从页面取到卡片与编辑面板组件。
export { AgentCard } from "../components/AgentCard";
export { AgentFormPanel } from "../components/AgentFormPanel";

function newAgentDraftKey(): string {
  return draftStorageKey({ field: "agent-form:new" });
}

/// 编辑已有 Agent 时草稿按 Agent id 分开存放，避免在不同 Agent 之间串用未保存内容。
function agentDraftKey(agentId: string): string {
  return draftStorageKey({ field: `agent-form:${agentId}` });
}

function readAgentFormDraft(key: string): AgentFormState | null {
  const raw = readDraft(browserDraftStorage(), key);
  if (raw === null) return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    return isAgentFormDraft(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

export function AgentsPage() {
  const [agents, setAgents] = useState<Agent[]>([]);
  const [team, setTeam] = useState<DefaultTeamPool | null>(null);
  const [loadError, setLoadError] = useState("");
  const [actionError, setActionError] = useState("");
  const [formError, setFormError] = useState("");
  const [loading, setLoading] = useState(true);
  const [form, setForm] = useState<AgentFormState>(emptyForm);
  const [editing, setEditing] = useState<Agent | null>(null);
  const [showForm, setShowForm] = useState(false);
  const [saving, setSaving] = useState(false);
  const [busyId, setBusyId] = useState("");

  async function refresh(showLoading = false) {
    if (showLoading) setLoading(true);
    try {
      const [agentList, defaultTeam] = await Promise.all([
        api.listAgents(),
        api.getDefaultTeam(),
      ]);
      setAgents(agentList);
      setTeam(defaultTeam);
      setLoadError("");
    } catch (err) {
      setLoadError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void refresh(true);
  }, []);

  function startCreate() {
    setEditing(null);
    setFormError("");
    setForm(readAgentFormDraft(newAgentDraftKey()) ?? emptyForm());
    setShowForm(true);
  }

  function startEdit(agent: Agent) {
    setEditing(agent);
    setFormError("");
    setForm(readAgentFormDraft(agentDraftKey(agent.id)) ?? formForAgent(agent));
    setShowForm(true);
  }

  // 面板打开期间持续保存草稿；提交成功或取消后不再写入。
  useEffect(() => {
    if (!showForm) return;
    writeDraftJson(
      browserDraftStorage(),
      editing ? agentDraftKey(editing.id) : newAgentDraftKey(),
      form,
    );
  }, [showForm, editing, form]);

  function closeForm() {
    // 取消时保留草稿，下次打开同一表单继续编辑。
    setShowForm(false);
  }

  async function handleSubmit() {
    setSaving(true);
    setFormError("");
    try {
      const availability = buildAvailability(form);
      const usageLimit = buildUsageLimit(form);
      const input: AgentInput = {
        name: form.name.trim(),
        kind: form.kind,
        model: form.model.trim(),
        roles: form.roles,
        max_concurrency: Number(form.maxConcurrency) || 1,
        allow_priority_fallback: form.allowPriorityFallback,
        enabled: form.enabled,
        command: parseJsonArray(form.command),
        model_args: parseJsonArray(form.modelArgs),
        availability,
        usage_limit: usageLimit,
      };
      if (!editing) {
        input.add_to_existing_pools = form.addToExistingPools;
      }
      if (editing) {
        await api.updateAgent(editing.id, input);
      } else {
        await api.createAgent(input);
      }
      clearDraft(
        browserDraftStorage(),
        editing ? agentDraftKey(editing.id) : newAgentDraftKey(),
      );
      setShowForm(false);
      await refresh();
    } catch (err) {
      setFormError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  }

  async function toggle(agent: Agent) {
    setActionError("");
    try {
      await api.updateAgent(agent.id, { enabled: !agent.enabled });
      await refresh();
    } catch (err) {
      setActionError(errorMessage(err));
    }
  }

  async function resetUsage(agent: Agent) {
    setBusyId(agent.id);
    setActionError("");
    try {
      await api.resetAgentUsage(agent.id);
      await refresh();
    } catch (err) {
      setActionError(errorMessage(err));
    } finally {
      setBusyId("");
    }
  }

  async function removeAgent(agent: Agent) {
    if (!window.confirm(`删除 Agent「${agent.name}」？`)) return;
    setBusyId(agent.id);
    setActionError("");
    try {
      await api.deleteAgent(agent.id);
      await refresh();
    } catch (err) {
      setActionError(errorMessage(err));
    } finally {
      setBusyId("");
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>Agent 团队</h1>
          <p className="subtitle">
            配置团队分工、调度顺序与用量边界，让合适的 Agent 完成合适的工作。
          </p>
          <PageGuide>同一 Agent 可扮演多个角色。卡片启停是影响所有项目与任务的全局开关；默认团队决定新项目的初始 Agent 池。可用时段按本地时区生效，用量达到限额后自动跳过，新窗口开始后恢复。</PageGuide>
        </div>
        <button className="primary" onClick={startCreate}>
          <Icon name="plus" />
          添加 Agent
        </button>
      </header>

      {loadError ? (
        <ErrorBox
          message={loadError}
          onRetry={() => void refresh(true)}
          retrying={loading}
        />
      ) : null}
      {actionError ? <ErrorBox message={actionError} /> : null}

      {loading ? (
        <section className="panel">
          <SkeletonLines label="正在加载 Agent…" />
        </section>
      ) : null}

      <GlobalQueue agents={agents} />
      {team ? (
        <AgentPoolPanel
          title="默认团队"
          subtitle="系统级团队模板：全部已注册 Agent 按角色出现在默认团队中，顺序越靠前优先级越高。新建项目的项目 Agent 池按这里的角色、顺序与启停状态快照初始化，之后项目独立维护。"
          roles={team.roles}
          scope="default"
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

      {!loading && agents.length > 0 ? <div className="section-heading"><h2>已注册 Agent <span className="section-count">{agents.length}</span></h2><span className="muted">{agents.filter((agent) => agent.enabled).length} 个已启用 · 启停影响所有项目</span></div> : null}
      <div className="agent-grid">
        {!loading && agents.length === 0 && !loadError ? (
          <Empty
            title="还没有 Agent"
            hint="点击右上角「添加 Agent」创建第一个 Agent，之后可在默认团队中调整顺序与启停，新建项目会自动继承。"
          />
        ) : (
          agents.map((agent) => (
            <AgentCard
              key={agent.id}
              agent={agent}
              busy={busyId === agent.id}
              onEdit={() => startEdit(agent)}
              onToggle={() => void toggle(agent)}
              onRemove={() => void removeAgent(agent)}
              onResetUsage={() => void resetUsage(agent)}
            />
          ))
        )}
      </div>

      {showForm ? (
        <AgentFormPanel
          agent={editing}
          form={form}
          saving={saving}
          error={formError}
          onChange={setForm}
          onSubmit={() => void handleSubmit()}
          onClose={closeForm}
        />
      ) : null}
    </div>
  );
}
