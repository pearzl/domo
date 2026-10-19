import { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../api';
import { useDomo } from '../store';
import type { AuditEvent, KnowledgeEntry } from '../types';
import { Badge, Empty, fmtTime, shortId } from '../ui';

/**
 * M5 项目知识库：把人工指导（guidance 事件）沉淀为可复用的项目/全局规范。
 *
 * - 按项目列出「本项目条目 + 全局条目」，随 SQLite 持久化、可增删改查；
 * - 每条条目回链来源人工指导事件（source_event_id → 审计事件）；
 * - 项目条目可一键「提升为全局」，供其他项目引用；
 * - 从任务详情 / 审计视图的「沉淀为规范」入口带来源事件跳转，自动预填。
 */
export function KnowledgeView() {
  const { projects, selectedProjectId, knowledgePrefill, distillGuidance, setError } = useDomo();

  // 目标项目：默认跟随全局选中项目，可在此视图内切换
  const [projectId, setProjectId] = useState<string>(selectedProjectId ?? '');
  useEffect(() => {
    if (selectedProjectId) setProjectId(selectedProjectId);
  }, [selectedProjectId]);
  useEffect(() => {
    if (!projectId && projects.length > 0) setProjectId(projects[0].id);
  }, [projectId, projects]);

  const [entries, setEntries] = useState<KnowledgeEntry[]>([]);
  const [guidanceEvents, setGuidanceEvents] = useState<AuditEvent[]>([]);
  const [loading, setLoading] = useState(false);
  const [showForm, setShowForm] = useState(false);
  const [busy, setBusy] = useState(false);

  // 新建/编辑表单
  const [formBody, setFormBody] = useState('');
  const [formApplies, setFormApplies] = useState('');
  const [formEventId, setFormEventId] = useState('');
  const [editId, setEditId] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!projectId) return;
    setLoading(true);
    try {
      const [list, evs] = await Promise.all([
        api.listKnowledge(projectId),
        api.listEvents({ project_id: projectId, type: 'task.guidance_added', order: 'desc', limit: 100 }),
      ]);
      setEntries(list);
      setGuidanceEvents(Array.isArray(evs) ? evs : (evs.events ?? []));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [projectId, setError]);

  useEffect(() => {
    void load();
  }, [load]);

  // 从任务详情 / 审计视图「沉淀为规范」跳转：切到对应项目 + 预填表单
  useEffect(() => {
    if (!knowledgePrefill) return;
    setProjectId(knowledgePrefill.project_id);
    setEditId(null);
    setFormBody(knowledgePrefill.text);
    setFormApplies('');
    setFormEventId(String(knowledgePrefill.event_id));
    setShowForm(true);
    distillGuidance(null, true); // 已读取，清空预填
  }, [knowledgePrefill, distillGuidance]);

  const project = projects.find(p => p.id === projectId);
  const archived = project?.status === 'archived';

  const eventOptions = useMemo(
    () =>
      guidanceEvents.map(ev => ({
        id: ev.id,
        text: typeof (ev.payload as Record<string, unknown>)?.text === 'string'
          ? ((ev.payload as Record<string, unknown>).text as string)
          : '',
      })),
    [guidanceEvents],
  );

  function resetForm() {
    setEditId(null);
    setFormBody('');
    setFormApplies('');
    setFormEventId('');
  }

  async function submit() {
    if (!formBody.trim()) {
      setError('规范正文不能为空');
      return;
    }
    setBusy(true);
    try {
      if (editId) {
        await api.updateKnowledge(editId, { body: formBody.trim(), applies_to: formApplies.trim() || undefined });
      } else {
        const evId = Number(formEventId);
        if (!Number.isFinite(evId) || evId <= 0) {
          setError('请选择来源人工指导事件');
          setBusy(false);
          return;
        }
        await api.createKnowledge(projectId, {
          body: formBody.trim(),
          applies_to: formApplies.trim() || undefined,
          source_event_id: evId,
        });
      }
      resetForm();
      setShowForm(false);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function onEdit(k: KnowledgeEntry) {
    setEditId(k.id);
    setFormBody(k.body);
    setFormApplies(k.applies_to ?? '');
    setFormEventId(String(k.source_event_id));
    setShowForm(true);
  }

  async function onDelete(k: KnowledgeEntry) {
    setBusy(true);
    try {
      await api.deleteKnowledge(k.id);
      if (editId === k.id) resetForm();
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function onPromote(k: KnowledgeEntry) {
    setBusy(true);
    try {
      await api.promoteKnowledge(k.id);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="stack">
      <div className="toolbar">
        <select className="select" value={projectId} onChange={e => setProjectId(e.target.value)}>
          {projects.map(p => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <span className="spacer" />
        <span className="muted">
          {loading ? '加载中…' : `本项目 + 全局规范 ${entries.length} 条`}
        </span>
        {!showForm && (
          <button
            className="btn btn-primary"
            disabled={archived}
            title={archived ? '项目已归档（只读）' : '把一条人工指导沉淀为项目规范'}
            onClick={() => {
              resetForm();
              setShowForm(true);
            }}
          >
            + 沉淀为规范
          </button>
        )}
      </div>

      {showForm && (
        <section className="panel">
          <h3 className="panel-title">{editId ? '编辑规范' : '新建规范（沉淀人工指导）'}</h3>
          <div className="stack">
            {!editId && (
              <label className="stack" style={{ gap: 4 }}>
                <span className="muted">来源人工指导事件（可回链审计）</span>
                <select className="select" value={formEventId} onChange={e => setFormEventId(e.target.value)}>
                  <option value="">— 选择一条人工指导事件 —</option>
                  {eventOptions.map(o => (
                    <option key={o.id} value={String(o.id)}>
                      #{o.id} · {o.text.slice(0, 60) || '(无正文)'}
                    </option>
                  ))}
                </select>
              </label>
            )}
            <label className="stack" style={{ gap: 4 }}>
              <span className="muted">规范正文（会注入后续 planner/executor/acceptor 提示词）</span>
              <textarea
                rows={4}
                value={formBody}
                onChange={e => setFormBody(e.target.value)}
                placeholder="例：所有改动必须附带可复现的验证命令"
              />
            </label>
            <label className="stack" style={{ gap: 4 }}>
              <span className="muted">适用范围（可选）</span>
              <input
                value={formApplies}
                onChange={e => setFormApplies(e.target.value)}
                placeholder="例：全部任务 / 仅前端任务"
              />
            </label>
            <div className="row-end">
              <button className="btn" onClick={() => { resetForm(); setShowForm(false); }}>取消</button>
              <button className="btn btn-primary" disabled={busy || !formBody.trim()} onClick={() => void submit()}>
                {editId ? '保存修改' : '创建规范'}
              </button>
            </div>
          </div>
        </section>
      )}

      {entries.length === 0 ? (
        <Empty text="暂无规范——在任务详情或审计视图对一条人工指导点「沉淀为规范」即可创建" />
      ) : (
        <section className="panel">
          <div className="event-list">
            {entries.map(k => {
              const editable = !archived && k.project_id === projectId;
              return (
                <div key={k.id} className="event-row" style={{ alignItems: 'flex-start' }}>
                  <span className="event-ctx stack" style={{ gap: 4 }}>
                    <span className="badge-row">
                      {k.scope === 'global' ? (
                        <Badge tone="purple">全局规范</Badge>
                      ) : (
                        <Badge tone="blue">本项目规范</Badge>
                      )}
                      {k.applies_to ? <Badge tone="gray">{k.applies_to}</Badge> : null}
                      <span className="mono muted">#{shortId(k.id)}</span>
                    </span>
                    <span className="pre-wrap">{k.body}</span>
                    <span className="muted" style={{ fontSize: 12 }}>
                      来源事件{' '}
                      <span className="mono">#{k.source_event_id}</span>
                      {' · '}登记 {fmtTime(k.created_at)}
                      {k.updated_at !== k.created_at ? ` · 更新 ${fmtTime(k.updated_at)}` : ''}
                      {k.project_id !== projectId ? ` · 来源项目 ${shortId(k.project_id)}` : ''}
                    </span>
                  </span>
                  <span className="badge-row">
                    {editable && k.scope === 'project' && (
                      <button
                        className="btn btn-sm"
                        disabled={busy}
                        title="提升为全局条目，供其他项目引用"
                        onClick={() => void onPromote(k)}
                      >
                        提升为全局
                      </button>
                    )}
                    {editable && (
                      <>
                        <button className="btn btn-sm" disabled={busy} onClick={() => void onEdit(k)}>编辑</button>
                        <button className="btn btn-sm btn-danger-ghost" disabled={busy} onClick={() => void onDelete(k)}>删除</button>
                      </>
                    )}
                  </span>
                </div>
              );
            })}
          </div>
        </section>
      )}
    </div>
  );
}
