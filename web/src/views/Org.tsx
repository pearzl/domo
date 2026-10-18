import { useState } from 'react';
import { useDomo } from '../store';
import type { Adapter, Agent, Role, Task } from '../types';
import { Badge, Empty, Modal, ROLE_LABEL, TaskLink, shortId } from '../ui';

const ALL_ROLES: Role[] = ['planner', 'executor', 'acceptor', 'decision_maker'];

export function OrgView() {
  const { agents, tasksByProject, createAgent, updateAgent, deleteAgent } = useDomo();

  const [name, setName] = useState('');
  const [adapter, setAdapter] = useState<Adapter>('mock');
  const [roles, setRoles] = useState<Role[]>(['executor']);
  const [maxParallel, setMaxParallel] = useState(1);
  const [durationMs, setDurationMs] = useState(4000);
  const [model, setModel] = useState('');
  const [timeoutMin, setTimeoutMin] = useState(60);
  const [err, setErr] = useState('');
  const [busy, setBusy] = useState(false);

  // 编辑弹窗（复用同一套字段；adapter 不可改）
  const [editing, setEditing] = useState<Agent | null>(null);
  const [eName, setEName] = useState('');
  const [eRoles, setERoles] = useState<Role[]>([]);
  const [eMaxParallel, setEMaxParallel] = useState(1);
  const [eDurationMs, setEDurationMs] = useState(4000);
  const [eModel, setEModel] = useState('');
  const [eTimeoutMin, setETimeoutMin] = useState(60);
  const [eErr, setEErr] = useState('');
  const [confirmDelete, setConfirmDelete] = useState<Agent | null>(null);

  function openEdit(a: Agent) {
    setEditing(a);
    setEErr('');
    setEName(a.name);
    setERoles([...a.roles]);
    setEMaxParallel(a.capabilities?.max_parallel ?? 1);
    setEDurationMs(a.config?.duration_ms ?? 4000);
    setEModel(a.config?.model ?? '');
    setETimeoutMin(Math.max(1, Math.round((a.config?.timeout_ms ?? 3_600_000) / 60_000)));
  }

  async function submitEdit() {
    if (!editing) return;
    setEErr('');
    if (!eName.trim()) return void setEErr('请填写 agent 名称');
    if (eRoles.length === 0) return void setEErr('请至少选择一个角色');
    setBusy(true);
    const ok = await updateAgent(editing.id, {
      name: eName.trim(),
      adapter: editing.adapter,
      roles: eRoles,
      max_parallel: eMaxParallel,
      duration_ms: eDurationMs,
      model: editing.adapter === 'opencode' && eModel.trim() ? eModel.trim() : undefined,
      timeout_ms: editing.adapter === 'opencode' && eTimeoutMin > 0 ? eTimeoutMin * 60_000 : undefined,
    });
    setBusy(false);
    if (ok) setEditing(null);
  }

  // 全项目任务索引：id -> {title, project_id}（TaskLink 跳转用）
  const taskInfo = new Map<string, { title: string; project_id: string }>();
  const tasksByAgent = new Map<string, Task[]>(); // executor 或 acceptor 指派
  for (const list of Object.values(tasksByProject)) {
    for (const t of list) {
      taskInfo.set(t.id, { title: t.title, project_id: t.project_id });
      for (const aid of [t.executor_agent_id, t.acceptor_agent_id]) {
        if (aid) {
          const arr = tasksByAgent.get(aid) ?? [];
          arr.push(t);
          tasksByAgent.set(aid, arr);
        }
      }
    }
  }

  /** 单个 agent 的负载视图：占用（在途）/ 上限 / 空闲余量 / 排队等待派发 */
  function loadOf(a: Agent) {
    const cap = Math.max(1, a.capabilities?.max_parallel ?? 1);
    const occupied = a.current_task_ids?.length ?? 0;
    const assigned = tasksByAgent.get(a.id) ?? [];
    const queue = assigned.filter(t => t.status === 'ready' || t.status === 'planned').length;
    const free = Math.max(0, cap - occupied);
    const pct = Math.min(100, Math.round((occupied / cap) * 100));
    return { cap, occupied, free, queue, pct };
  }

  const busyCount = agents.filter(a => a.status === 'busy').length;
  const roleCounts = ALL_ROLES.map(r => ({
    role: r,
    count: agents.filter(a => a.roles.includes(r)).length,
  }));

  async function submit() {
    setErr('');
    if (!name.trim()) return void setErr('请填写 agent 名称');
    if (roles.length === 0) return void setErr('请至少选择一个角色');
    if (!Number.isFinite(maxParallel) || maxParallel < 1) return void setErr('并行上限需 ≥ 1');
    if (adapter === 'mock') {
      if (!Number.isFinite(durationMs) || durationMs < 0) return void setErr('执行时长需 ≥ 0');
    }
    setBusy(true);
    const ok = await createAgent({
      name: name.trim(),
      adapter,
      roles,
      max_parallel: maxParallel,
      duration_ms: durationMs,
      // opencode 可选配置
      model: adapter === 'opencode' ? model : undefined,
      timeout_ms: adapter === 'opencode' ? Math.max(1, timeoutMin) * 60_000 : undefined,
    });
    setBusy(false);
    if (ok) setName('');
  }

  return (
    <div className="stack">
      <section className="panel">
        <h2 className="panel-title">登记 Agent</h2>
        {err && <div className="form-error">{err}</div>}
        <form
          className="agent-form"
          onSubmit={e => {
            e.preventDefault();
            void submit();
          }}
        >
          <label>
            名称 *
            <input value={name} onChange={e => setName(e.target.value)} placeholder="如：执行员-A" />
          </label>
          <label>
            适配器
            <select value={adapter} onChange={e => setAdapter(e.target.value as Adapter)}>
              <option value="mock">mock（模拟）</option>
              <option value="opencode">opencode（真实）</option>
            </select>
          </label>
          <label>
            并行上限
            <input
              type="number"
              min={1}
              value={maxParallel}
              onChange={e => setMaxParallel(Number(e.target.value))}
            />
          </label>
          {adapter === 'mock' ? (
            <label>
              执行时长 (ms)
              <input
                type="number"
                min={0}
                value={durationMs}
                onChange={e => setDurationMs(Number(e.target.value))}
              />
            </label>
          ) : (
            <>
              <label>
                模型（可选）
                <input
                  value={model}
                  onChange={e => setModel(e.target.value)}
                  placeholder="provider/model，留空用默认"
                />
              </label>
              <label>
                run 超时（分钟，默认 60）
                <input
                  type="number"
                  min={1}
                  step={1}
                  value={timeoutMin}
                  onChange={e => setTimeoutMin(Number(e.target.value))}
                />
              </label>
            </>
          )}
          <div>
            <div className="label-text">角色 *</div>
            <div className="check-row">
              {ALL_ROLES.map(r => (
                <label key={r} className="check-label">
                  <input
                    type="checkbox"
                    checked={roles.includes(r)}
                    onChange={e =>
                      setRoles(prev => (e.target.checked ? [...prev, r] : prev.filter(x => x !== r)))
                    }
                  />
                  {ROLE_LABEL[r]}
                </label>
              ))}
            </div>
          </div>
          <button className="btn btn-primary" type="submit" disabled={busy}>
            {busy ? '登记中…' : '登记 Agent'}
          </button>
        </form>
      </section>

      {agents.length === 0 ? (
        <Empty text="还没有 agent，先登记一个。" />
      ) : (
        <>
        <section className="panel">
          <h2 className="panel-title">团队负载总览</h2>
          <div className="org-summary">
            <div className="org-stats">
              <div className="org-stat">
                <span className="org-stat-num">{agents.length}</span>
                <span className="muted">Agent</span>
              </div>
              <div className="org-stat">
                <span className="org-stat-num" style={{ color: 'var(--amber)' }}>
                  {busyCount}
                </span>
                <span className="muted">忙碌</span>
              </div>
              <div className="org-stat">
                <span className="org-stat-num" style={{ color: 'var(--green)' }}>
                  {agents.length - busyCount}
                </span>
                <span className="muted">空闲</span>
              </div>
            </div>
            <div className="role-dist">
              <div className="label-text">角色分布</div>
              {roleCounts.map(rc => (
                <div key={rc.role} className="role-dist-row">
                  <span className="role-dist-label">{ROLE_LABEL[rc.role]}</span>
                  <div className="progress role-dist-bar">
                    <div
                      className="progress-fill"
                      style={{ width: agents.length ? `${(rc.count / agents.length) * 100}%` : '0%' }}
                    />
                  </div>
                  <span className="muted role-dist-count">{rc.count}</span>
                </div>
              ))}
            </div>
          </div>
        </section>
        <section className="panel">
          <h2 className="panel-title">成员列表（{agents.length}）</h2>
          <table className="table">
            <thead>
              <tr>
                <th>名称</th>
                <th>适配器</th>
                <th>角色</th>
                <th>能力</th>
                <th>状态</th>
                <th>负载 / 队列</th>
                <th>当前任务</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {agents.map(a => {
                const { cap, occupied, free, queue, pct } = loadOf(a);
                return (
                <tr key={a.id}>
                  <td>
                    <span title={a.id}>{a.name}</span>
                    <span className="mono muted id-hint">#{shortId(a.id)}</span>
                  </td>
                  <td>
                    <Badge tone={a.adapter === 'opencode' ? 'purple' : 'gray'}>
                      {a.adapter === 'opencode' ? 'opencode' : 'mock'}
                    </Badge>
                  </td>
                  <td>
                    <div className="badge-row">
                      {a.roles.map(r => (
                        <Badge key={r} tone={r === 'decision_maker' ? 'purple' : 'blue'}>
                          {ROLE_LABEL[r] ?? r}
                        </Badge>
                      ))}
                    </div>
                  </td>
                  <td className="muted">
                    并行 {cap}
                    {a.capabilities?.supports_resume ? ' · 可恢复' : ''}
                    {a.adapter === 'opencode'
                      ? [
                          a.config?.model ? ` · ${a.config.model}` : '',
                          a.config?.timeout_ms ? ` · 超时 ${(a.config.timeout_ms / 1000).toFixed(0)}s` : '',
                        ].join('') || ' · 默认模型'
                      : a.config?.duration_ms != null
                        ? ` · ${(a.config.duration_ms / 1000).toFixed(1)}s`
                        : ''}
                  </td>
                  <td>
                    <Badge tone={a.status === 'busy' ? 'amber' : 'green'}>
                      {a.status === 'busy' ? '忙碌' : '空闲'}
                    </Badge>
                  </td>
                  <td>
                    <div className="load-cell" title={`占用 ${occupied}/${cap}，空闲余量 ${free}`}>
                      <div className="progress load-bar">
                        <div
                          className={`progress-fill ${pct >= 100 ? 'fill-full' : ''}`}
                          style={{ width: `${pct}%` }}
                        />
                      </div>
                      <span className="muted load-text">
                        占用 {occupied}/{cap}（{pct}%）· 空闲 {free}
                      </span>
                      {queue > 0 ? (
                        <details className="load-queue-detail" title="已指派但等待派发的任务（planned/ready），点击展开">
                          <summary className="load-queue">排队 {queue}</summary>
                          <div className="badge-row">
                            {(tasksByAgent.get(a.id) ?? [])
                              .filter(t => t.status === 'planned' || t.status === 'ready')
                              .map(t => (
                                <TaskLink key={t.id} taskId={t.id} projectId={t.project_id}>
                                  {t.title}
                                </TaskLink>
                              ))}
                          </div>
                        </details>
                      ) : (
                        <span className="muted load-queue-empty">队列空</span>
                      )}
                    </div>
                  </td>
                  <td>
                    {(a.current_task_ids?.length ?? 0) === 0 ? (
                      <span className="muted">—</span>
                    ) : (
                      <div className="badge-row">
                        {a.current_task_ids.map(id => {
                          const info = taskInfo.get(id);
                          return (
                            <TaskLink key={id} taskId={id} projectId={info?.project_id ?? ''}>
                              {info?.title ?? `#${shortId(id)}`}
                            </TaskLink>
                          );
                        })}
                      </div>
                    )}
                  </td>
                  <td>
                    <div className="badge-row">
                      <button className="btn btn-sm" onClick={() => openEdit(a)} title="编辑">✎</button>
                      <button className="btn btn-sm" onClick={() => setConfirmDelete(a)} title="删除">🗑</button>
                    </div>
                  </td>
                </tr>
                );
              })}
            </tbody>
          </table>
        </section>
        </>
      )}

      {editing && (
        <Modal title="编辑 Agent" onClose={() => setEditing(null)}>
          <div className="stack">
            {eErr && <div className="form-error">{eErr}</div>}
            <div className="muted">适配器：{editing.adapter === 'opencode' ? 'opencode（真实）' : 'mock（模拟）'}（不可更改，如需换适配器请删除重建）</div>
            <label>
              名称 *
              <input value={eName} onChange={e => setEName(e.target.value)} />
            </label>
            {editing.adapter === 'mock' ? (
              <label>
                执行时长 (ms)
                <input
                  type="number" min={0}
                  value={eDurationMs}
                  onChange={e => setEDurationMs(Number(e.target.value))}
                />
              </label>
            ) : (
              <>
                <label>
                  模型（provider/model，清空则用默认）
                  <input value={eModel} onChange={e => setEModel(e.target.value)} placeholder="例：opencode/deepseek-v4.1-flash-free" />
                </label>
                <label>
                  run 超时（分钟）
                  <input
                    type="number" min={1} step={1}
                    value={eTimeoutMin}
                    onChange={e => setETimeoutMin(Number(e.target.value))}
                  />
                </label>
              </>
            )}
            <div>
              <div className="label-text">角色 *</div>
              <div className="check-row">
                {ALL_ROLES.map(r => (
                  <label key={r} className="check-label">
                    <input
                      type="checkbox"
                      checked={eRoles.includes(r)}
                      onChange={e =>
                        setERoles(prev => (e.target.checked ? [...prev, r] : prev.filter(x => x !== r)))
                      }
                    />
                    {ROLE_LABEL[r]}
                  </label>
                ))}
              </div>
            </div>
            <label>
              并行上限
              <input
                type="number" min={1}
                value={eMaxParallel}
                onChange={e => setEMaxParallel(Number(e.target.value))}
              />
            </label>
            <div className="row-end">
              <button className="btn" onClick={() => setEditing(null)}>取消</button>
              <button className="btn btn-primary" disabled={busy} onClick={() => void submitEdit()}>
                {busy ? '保存中…' : '保存修改'}
              </button>
            </div>
          </div>
        </Modal>
      )}

      {confirmDelete && (
        <Modal title="删除 Agent" onClose={() => setConfirmDelete(null)}>
          <div className="stack">
            <p>
              确定删除 <strong>{confirmDelete.name}</strong>（#{shortId(confirmDelete.id)}）？
              <br />
              <span className="muted">
                被进行中任务（执行中/待验收/挂起/升级/待派发）引用时后端会拒绝；被已完成任务引用的历史记录仅显示名称回退。
              </span>
            </p>
            <div className="row-end">
              <button className="btn" onClick={() => setConfirmDelete(null)}>取消</button>
              <button
                className="btn"
                style={{ color: 'var(--danger, #c0392b)' }}
                disabled={busy}
                onClick={async () => {
                  setBusy(true);
                  const ok = await deleteAgent(confirmDelete.id);
                  setBusy(false);
                  if (ok) setConfirmDelete(null);
                }}
              >
                删除
              </button>
            </div>
          </div>
        </Modal>
      )}
    </div>
  );
}
