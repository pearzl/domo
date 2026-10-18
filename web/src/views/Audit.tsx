import { useEffect, useMemo, useState } from 'react';
import { api } from '../api';
import { useDomo } from '../store';
import type { AuditEvent, Agent, TaskDetail } from '../types';
import { AgentLink, Pager, fmtTime, shortId } from '../ui';

/** 单页长度（全站统一 15） */
const PAGE_SIZE = 15;

/** 类型快捷过滤组（后端按前缀匹配），复选框可自由组合 */
const TYPE_GROUPS: { key: string; label: string; prefix: string }[] = [
  { key: 'task', label: '任务', prefix: 'task.' },
  { key: 'acceptance', label: '验收', prefix: 'acceptance.' },
  { key: 'session', label: 'agent 会话', prefix: 'agent.' },
  { key: 'escalation', label: '升级', prefix: 'escalation.' },
  { key: 'replan', label: '重规划', prefix: 'replan.' },
  { key: 'milestone', label: '里程碑', prefix: 'milestone.' },
  { key: 'decision', label: '决策', prefix: 'decision.' },
  { key: 'report', label: '报告', prefix: 'report.' },
  { key: 'project', label: '项目', prefix: 'project.' },
];

/** datetime-local 值 → 毫秒时间戳 */
function dtToTs(v: string): number | undefined {
  if (!v) return undefined;
  const d = new Date(v);
  return isNaN(d.getTime()) ? undefined : d.getTime();
}

/** 从 payload 提取关联 agent：{id, 快照名} 列表 */
function agentRefs(ev: AuditEvent): { id: string; snap?: string }[] {
  const p = (ev.payload ?? {}) as Record<string, unknown>;
  const out: { id: string; snap?: string }[] = [];
  const push = (id: unknown, snap?: unknown) => {
    if (typeof id === 'string' && id.startsWith('agt_') && !out.some(x => x.id === id)) {
      out.push({ id, snap: typeof snap === 'string' ? snap : undefined });
    }
  };
  push(p.agent_id, p.name); // agent.created/updated/deleted 带 name 快照
  // 关联详情形态：{executor: {id, name, ...}} / {acceptor: {...}}
  for (const key of ['executor', 'acceptor']) {
    const v = p[key];
    if (v && typeof v === 'object') {
      const o = v as Record<string, unknown>;
      push(o.id, o.name);
    }
  }
  return out;
}

export function AuditView() {
  const { projects, selectProject, setView } = useDomo();
  // 全量 agent（含软删除）→ id→name 映射，审计条目直接显示名字
  const [allAgents, setAllAgents] = useState<Agent[]>([]);

  const [projectId, setProjectId] = useState('');
  const [prefixes, setPrefixes] = useState<Set<string>>(new Set());
  const [from, setFrom] = useState('');
  const [to, setTo] = useState('');
  const [page, setPage] = useState(0);
  const [rows, setRows] = useState<AuditEvent[]>([]);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  // 关联详情右侧抽屉
  const [drawerEv, setDrawerEv] = useState<AuditEvent | null>(null);

  useEffect(() => {
    void api.listAgents({ all: true }).then(setAllAgents).catch(() => undefined);
  }, []);

  const prefixesParam = useMemo(
    () => [...prefixes].map(k => TYPE_GROUPS.find(g => g.key === k)?.prefix ?? k).join(','),
    [prefixes],
  );

  useEffect(() => {
    setLoading(true);
    api
      .listEvents({
        project_id: projectId || undefined,
        order: 'desc', // 最新在前
        type_prefix: prefixesParam || undefined,
        from: dtToTs(from),
        to: dtToTs(to),
        limit: PAGE_SIZE,
        offset: page * PAGE_SIZE,
      })
      .then(res => {
        if (Array.isArray(res)) {
          setRows(res);
          setTotal(res.length);
        } else {
          setRows(res.events ?? []);
          setTotal(res.total ?? res.events?.length ?? 0);
        }
      })
      .finally(() => setLoading(false));
  }, [projectId, prefixesParam, from, to, page]);

  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE));
  // 项目名解析：'-'=agent 级事件（无项目归属）；不在列表中的 project_id = 已删除项目
  const projectRef = (pid: string): { name: string; link: boolean } => {
    if (pid === '-') return { name: 'agent 级', link: false };
    const p = projects.find(x => x.id === pid);
    return p ? { name: p.name, link: true } : { name: '已删除项目', link: false };
  };

  function togglePrefix(k: string) {
    setPrefixes(prev => {
      const next = new Set(prev);
      if (next.has(k)) next.delete(k);
      else next.add(k);
      return next;
    });
    setPage(0);
  }

  function goProject(pid: string) {
    selectProject(pid);
    setView('tasks');
  }

  const pagerProps = { page, totalPages, total, onPage: setPage, label: '条事件' };

  return (
    <div className="stack">
      <div className="toolbar" style={{ flexWrap: 'wrap' }}>
        <label className="muted">
          项目：
          <select value={projectId} onChange={e => { setProjectId(e.target.value); setPage(0); }} className="select">
            <option value="">全部项目</option>
            {projects.map(p => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </label>
        <label className="muted">
          从：
          <input type="datetime-local" value={from} onChange={e => { setFrom(e.target.value); setPage(0); }} />
        </label>
        <label className="muted">
          至：
          <input type="datetime-local" value={to} onChange={e => { setTo(e.target.value); setPage(0); }} />
        </label>
      </div>
      <div className="toolbar" style={{ flexWrap: 'wrap', alignItems: 'center', gap: 12 }}>
        <span className="muted">类型（可组合）：</span>
        {TYPE_GROUPS.map(g => (
          <label key={g.key} className="muted" style={{ cursor: 'pointer' }}>
            <input
              type="checkbox"
              checked={prefixes.has(g.key)}
              onChange={() => togglePrefix(g.key)}
            />{' '}
            {g.label}
          </label>
        ))}
        <span className="spacer" />
        <span className="muted">{loading ? '加载中…' : '倒序 · 最新在前'}</span>
      </div>

      <Pager {...pagerProps} />

      {rows.length === 0 ? (
        <div className="empty">暂无事件（试试放宽筛选条件）</div>
      ) : (
        <section className="panel">
          <div className="event-list">
            {rows.map(ev => (
              <EventRow
                key={ev.id}
                ev={ev}
                agentMap={new Map(allAgents.map(a => [a.id, a]))}
                projectRef={projectRef}
                onGoProject={goProject}
                onOpenDetail={() => setDrawerEv(ev)}
              />
            ))}
          </div>
          <Pager {...pagerProps} />
        </section>
      )}

      {drawerEv && (
        <EventDrawer
          ev={drawerEv}
          agentMap={new Map(allAgents.map(a => [a.id, a]))}
          projectRef={projectRef}
          onClose={() => setDrawerEv(null)}
        />
      )}
    </div>
  );
}

function EventRow({
  ev,
  agentMap,
  projectRef,
  onGoProject,
  onOpenDetail,
}: {
  ev: AuditEvent;
  agentMap: Map<string, Agent>;
  projectRef: (pid: string) => { name: string; link: boolean };
  onGoProject: (pid: string) => void;
  onOpenDetail: () => void;
}) {
  const p = (ev.payload ?? {}) as Record<string, unknown>;
  const refs = agentRefs(ev);
  const pr = projectRef(ev.project_id);

  return (
    <div className="event-row">
      <span className="mono muted event-ts">{fmtTime(ev.ts)}</span>
      <span className="mono ev-type">{ev.type}</span>
      <span className="event-ctx">
        {/* 项目可点 → 任务视图；'-' 为 agent 级事件；已删除项目显示快照名（不可点） */}
        {pr.link ? (
          <button className="task-link" title={`打开项目 ${ev.project_id}`} onClick={() => onGoProject(ev.project_id)}>
            {pr.name}
          </button>
        ) : (
          <span className="muted">{pr.name}</span>
        )}
        {ev.task_id ? <span className="mono muted"> · #{shortId(ev.task_id)}</span> : null}
        {refs.length > 0 && (
          <span className="muted">
            {' · '}
            {refs.map(r => (
              <AgentLink key={r.id} agentId={r.id} snapshotName={r.snap ?? agentMap.get(r.id)?.name ?? `#${shortId(r.id)}`}>
                {agentMap.get(r.id)?.name ?? r.snap ?? `#${shortId(r.id)}`}
              </AgentLink>
            ))}
          </span>
        )}
        {typeof p.error === 'string' && <span className="event-err" title={p.error}>⚠ {p.error.slice(0, 60)}</span>}
      </span>
      <button className="btn btn-sm" onClick={onOpenDetail}>
        详情 →
      </button>
    </div>
  );
}

/** 关联详情右侧抽屉（与任务详情抽屉同款交互） */
function EventDrawer({
  ev,
  agentMap,
  projectRef,
  onClose,
}: {
  ev: AuditEvent;
  agentMap: Map<string, Agent>;
  projectRef: (pid: string) => { name: string; link: boolean };
  onClose: () => void;
}) {
  const p = (ev.payload ?? {}) as Record<string, unknown>;
  const taskId = ev.task_id ?? (typeof p.task_id === 'string' ? p.task_id : '');
  const [detail, setDetail] = useState<TaskDetail | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    setDetail(null);
    setErr(null);
    if (taskId) {
      api.getTaskDetail(taskId).then(setDetail).catch(e => setErr(String(e)));
    }
  }, [taskId]);

  const refs = agentRefs(ev);
  const pr = projectRef(ev.project_id);

  return (
    <div className="drawer-wrap">
      <div className="drawer-overlay" onClick={onClose} />
      <aside className="drawer">
        <div className="drawer-head">
          <div>
            <div className="drawer-title">{ev.type}</div>
            <div className="drawer-sub">
              <span className="mono muted">{fmtTime(ev.ts)}</span>
              <span className="muted">{pr.name}</span>
            </div>
          </div>
          <button className="icon-btn" onClick={onClose} aria-label="关闭">✕</button>
        </div>

        <section className="drawer-section highlight">
          <h4>事件 payload</h4>
          <pre style={{ whiteSpace: 'pre-wrap', fontSize: 11.5 }}>{JSON.stringify(ev.payload, null, 2)}</pre>
          {refs.length > 0 && (
            <p className="muted">
              关联 agent：
              {refs.map(r => (
                <AgentLink key={r.id} agentId={r.id} snapshotName={r.snap ?? agentMap.get(r.id)?.name}>
                  {agentMap.get(r.id)?.name ?? r.snap ?? `#${shortId(r.id)}`}
                </AgentLink>
              ))}
            </p>
          )}
        </section>

        {taskId && (
          <section className="drawer-section">
            <h4>关联任务</h4>
            {err && <p className="muted">加载失败：{err}</p>}
            {detail ? (
              <div className="stack">
                <p>
                  <strong>{detail.title}</strong>（{detail.status} · 第 {detail.attempt} 次尝试 · 失败 {detail.failure_count}/退回 {detail.rejection_count}）
                </p>
                {detail.description && <p className="muted" style={{ whiteSpace: 'pre-wrap' }}>描述：{detail.description}</p>}
                {detail.acceptance_criteria && <p className="muted" style={{ whiteSpace: 'pre-wrap' }}>验收标准：{detail.acceptance_criteria}</p>}
              </div>
            ) : (
              !err && <p className="muted">加载中…</p>
            )}
          </section>
        )}

        {detail && (
          <section className="drawer-section">
            <h4>会话轮次（执行 / 验收）</h4>
            <ul className="timeline">
              {detail.events
                .filter(e => e.type === 'agent.session_started' || e.type === 'agent.session_ended')
                .map(e => {
                  const sp = (e.payload ?? {}) as Record<string, unknown>;
                  const phase = sp.phase === 'accept' ? '验收' : '执行';
                  const attempt = String(sp.attempt ?? '?');
                  const agentId = typeof sp.agent_id === 'string' ? sp.agent_id : '';
                  const name = agentMap.get(agentId)?.name ?? (typeof sp.agent_id === 'string' ? `#${shortId(agentId)}` : '');
                  return (
                    <li key={e.id}>
                      <div className="timeline-line">
                        <span className="mono muted">{fmtTime(e.ts)}</span>
                        <span className="mono ev-type">
                          {phase}#{attempt} · {name} · {e.type === 'agent.session_started' ? '开始' : `结束（${String(sp.result ?? '?')}）`}
                        </span>
                      </div>
                      {typeof sp.purpose === 'string' && e.type === 'agent.session_started' && (
                        <p className="muted" style={{ fontSize: 12 }}>🎯 {sp.purpose}{typeof sp.trigger === 'string' ? `（触发：${sp.trigger === 'human' ? '人工' : sp.trigger === 'recovery' ? '恢复重派' : '自动调度'}）` : ''}</p>
                      )}
                      {typeof sp.detail === 'string' && e.type === 'agent.session_ended' && (
                        <p className="muted" style={{ fontSize: 12 }}>结果：{sp.detail}</p>
                      )}
                    </li>
                  );
                })}
            </ul>
          </section>
        )}

        {detail && (
          <section className="drawer-section">
            <h4>日志尾部（20 行）</h4>
            <pre className="ev-log-tail">
              {detail.logs.length > 0 ? detail.logs.slice(-20).map(l => l.line).join('\n') : '（无日志）'}
            </pre>
          </section>
        )}

        {!taskId && <p className="drawer-section muted">（本事件无关联任务，仅 payload 信息）</p>}
      </aside>
    </div>
  );
}
