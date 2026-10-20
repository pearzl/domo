import { useEffect, useState } from 'react';
import { useDomo } from '../store';
import type { ApplyResult, InboxAgentBrief, InboxContext, InboxItem, ReplanChange, Task } from '../types';
import {
  Badge, Empty, fmtTime, INBOX_TYPE_LABEL, RESOLUTION_LABEL, shortId, ROLE_LABEL, Pager,
  DECISION_KIND_LABEL, DECISION_SCOPE_LABEL, MILESTONE_STATUS_LABEL, StatusBadge,
} from '../ui';

const TERMINAL_STATUS = ['done', 'cancelled'];

function resolutionLabel(resolution: InboxItem['resolution']): string {
  // 后端 resolution 是 JSON 对象 {resolution, note?, agent_id?, decision?}
  const raw = typeof resolution === 'object' && resolution !== null
    ? (resolution as Record<string, unknown>).resolution
    : resolution;
  let label = RESOLUTION_LABEL[typeof raw === 'string' ? raw : ''];
  // 决策类条目：补充 approve/deny/retry/auto_closed 的裁决结果
  if (typeof raw === 'string' && ['approve', 'deny', 'retry'].includes(raw)) {
    label = RESOLUTION_LABEL[raw];
  }
  if (typeof resolution === 'object' && resolution !== null) {
    const dec = (resolution as Record<string, unknown>).decision;
    if (dec === 'auto_closed') label = '已随裁定自动关闭';
    // 更换动作附带的 agent 摘要
    if (typeof raw === 'string' && (raw === 'reassign' || raw === 'reassign_acceptor')) {
      const ag = (resolution as Record<string, unknown>).agent_id;
      if (typeof ag === 'string') label = `${label}→ #${shortId(ag)}`;
    }
  }
  return label ?? '已处理';
}

export function InboxView() {
  const { inbox, projects, focusedInboxId } = useDomo();

  const open = inbox.filter(i => i.status === 'open');
  const resolved = inbox.filter(i => i.status === 'resolved');
  const projectName = (id: string) => projects.find(p => p.id === id)?.name ?? '未知项目';
  // 已处理记录分页（每页 15，最新在前）
  const [rPage, setRPage] = useState(0);
  const rSorted = [...resolved].sort((a, b) => b.created_at - a.created_at);
  const rPages = Math.max(1, Math.ceil(rSorted.length / 15));
  const rShown = rSorted.slice(rPage * 15, rPage * 15 + 15);

  // 从里程碑 gate 一键跳转过来时，滚动并高亮目标条目
  useEffect(() => {
    if (!focusedInboxId) return;
    const el = document.getElementById(`inbox-${focusedInboxId}`);
    el?.scrollIntoView({ behavior: 'smooth', block: 'center' });
  }, [focusedInboxId]);

  return (
    <div className="stack">
      {open.length === 0 ? (
        <Empty text="决策收件箱是空的，当前没有待人事项。" />
      ) : (
        open.map(item => (
          <InboxRow
            key={item.id}
            item={item}
            projectName={projectName(item.project_id)}
            focused={item.id === focusedInboxId}
          />
        ))
      )}

      {resolved.length > 0 && (
        <section className="panel">
          <h2 className="panel-title">已处理（{resolved.length}）</h2>
          <Pager page={rPage} totalPages={rPages} total={resolved.length} onPage={setRPage} label="条记录" />
          <div className="stack">
            {rShown.map(item => (
              <div key={item.id} className="inbox-item resolved">
                <div className="inbox-head">
                  <Badge tone="gray">{INBOX_TYPE_LABEL[item.type] ?? item.type}</Badge>
                  <span className="muted">{projectName(item.project_id)}</span>
                  <span className="muted">{fmtTime(item.created_at)}</span>
                  <span className="spacer" />
                  <Badge tone="blue">已处理 · {resolutionLabel(item.resolution)}</Badge>
                </div>
                <PayloadBlock payload={item.payload} />
                <ResolutionDetail resolution={item.resolution} />
              </div>
            ))}
          </div>
          <Pager page={rPage} totalPages={rPages} total={resolved.length} onPage={setRPage} label="条记录" />
        </section>
      )}
    </div>
  );
}

/** 徽章配色：执行端问题（红）vs 验收端问题（amber） */
function reasonBadge(reason: unknown): { label: string; tone: 'red' | 'amber' } {
  if (reason === 'rejection_limit') return { label: '验收退回升级', tone: 'amber' };
  if (reason === 'acceptance_failure_limit') return { label: '验收运行失败挂起', tone: 'amber' };
  return { label: '执行失败挂起', tone: 'red' };
}

function AgentBrief({ legend, a, highlight }: { legend: string; a?: InboxAgentBrief | null; highlight?: boolean }) {
  return (
    <span className="mono" style={highlight ? { fontWeight: 600 } : undefined}>
      {legend}：{a ? `${a.name}【${a.adapter}，model=${a.model ?? '默认'}】` : '未指派'}
    </span>
  );
}

function ContextPanel({ ctx }: { ctx: InboxContext }) {
  const { task, executor, acceptor, last_error } = ctx;
  return (
    <div className="stack" style={{ borderLeft: '3px solid var(--line, #ccc)', paddingLeft: 10 }}>
      <div>
        <strong>任务：</strong>
        {task.title}
        <span className="muted"> #{shortId(task.id)} · {task.status} · 第 {task.attempt} 次尝试 · 失败 {task.failure_count} / 退回 {task.rejection_count}</span>
      </div>
      {task.description && <p className="muted" style={{ whiteSpace: 'pre-wrap', margin: 0 }}>描述：{task.description}</p>}
      {task.acceptance_criteria && <p className="muted" style={{ whiteSpace: 'pre-wrap', margin: 0 }}>验收标准：{task.acceptance_criteria}</p>}
      {last_error && (
        <div style={{ color: 'var(--danger, #c0392b)', whiteSpace: 'pre-wrap' }}>
          最近一次失败原因：<code>{last_error}</code>
        </div>
      )}
      <div style={{ display: 'flex', gap: 14, flexWrap: 'wrap' }}>
        <AgentBrief legend="执行者" a={executor} highlight={true} />
        <AgentBrief legend="验收者" a={acceptor} />
      </div>
      {ctx.phase_hint && <p className="muted" style={{ margin: 0 }}>💡 {ctx.phase_hint}</p>}
    </div>
  );
}

/** 收件箱行分派：里程碑 gate 有专属的「通过 / 退回」裁决；
 *  退回后重开的里程碑窗口（replan + payload.kind="milestone_window"）走窗口重规划；
 *  决策类条目（带 payload.decision_id）走「批准 / 否决 / 重试」；
 *  其余 replan 走普通重规划提示，升级类条目走升级裁决。 */
function InboxRow({
  item,
  projectName,
  focused,
}: {
  item: InboxItem;
  projectName: string;
  focused?: boolean;
}) {
  const payload = (item.payload ?? {}) as Record<string, unknown>;
  if (item.type === 'milestone_gate') {
    return <GateRow item={item} projectName={projectName} focused={focused} />;
  }
  // gate 退回后重开的「里程碑窗口」：人在窗口内追加任务 / 调整后续规划
  if (item.type === 'replan' && payload.kind === 'milestone_window') {
    return <WindowRow item={item} projectName={projectName} focused={focused} />;
  }
  if (typeof payload.decision_id === 'string') {
    return <DecisionRow item={item} projectName={projectName} focused={focused} />;
  }
  // 普通重规划提示（无决策请求，如放弃任务后提示下游需重规划）：确认知悉即可关闭
  if (item.type === 'replan') {
    return <PlainReplanRow item={item} projectName={projectName} focused={focused} />;
  }
  return <EscalationRow item={item} projectName={projectName} focused={focused} />;
}

/** 普通重规划提示（无决策请求）：例如放弃任务后提示下游需要重规划。
 *  这类条目不含自动落地动作，只需人工知悉并在任务看板/介入面板调整规划后确认关闭。 */
function PlainReplanRow({
  item,
  projectName,
  focused,
}: {
  item: InboxItem;
  projectName: string;
  focused?: boolean;
}) {
  const { resolveInboxItem, tasksByProject, selectProject, setView } = useDomo();
  const payload = (item.payload ?? {}) as Record<string, unknown>;
  const detail = typeof payload.detail === 'string' ? payload.detail : '';
  const abandoned = typeof payload.abandoned_task_id === 'string' ? payload.abandoned_task_id : '';
  const downstream = Array.isArray(payload.downstream)
    ? (payload.downstream as unknown[]).filter((x): x is string => typeof x === 'string')
    : [];
  const tasks = tasksByProject[item.project_id] ?? [];
  const titleOf = (id: string) => tasks.find(t => t.id === id)?.title ?? `#${shortId(id)}`;

  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);

  async function submit() {
    setBusy(true);
    const ok = await resolveInboxItem(item.id, 'close', note.trim());
    setBusy(false);
    if (ok) setNote('');
  }

  return (
    <div id={`inbox-${item.id}`} className={`panel inbox-item${focused ? ' inbox-focus' : ''}`}>
      <div className="inbox-head">
        <Badge tone="purple">{INBOX_TYPE_LABEL[item.type] ?? item.type}</Badge>
        <Badge tone="amber">待重规划</Badge>
        <span>{projectName}</span>
        <span className="muted">{fmtTime(item.created_at)}</span>
        <span className="spacer" />
      </div>

      <div className="stack" style={{ borderLeft: '3px solid var(--line, #ccc)', paddingLeft: 10 }}>
        {detail && <p style={{ margin: 0, whiteSpace: 'pre-wrap' }}>💡 {detail}</p>}
        {abandoned && <div className="muted">已放弃任务：{titleOf(abandoned)}</div>}
        {downstream.length > 0 && (
          <div className="muted">受影响的下游任务（{downstream.length}）：{downstream.map(titleOf).join('、')}</div>
        )}
        <p className="muted" style={{ margin: 0 }}>
          请到任务看板 / 人工介入面板调整这些任务的后续规划；确认已知悉后可在此关闭该提示。
        </p>
      </div>

      <div className="row-end">
        <button className="btn" onClick={() => { selectProject(item.project_id); setView('tasks'); }}>
          查看项目任务 →
        </button>
      </div>

      <div className="resolve-row">
        <input value={note} onChange={e => setNote(e.target.value)} placeholder="处理备注（可选）" />
        <button className="btn btn-primary" disabled={busy} onClick={() => void submit()}>
          {busy ? '处理中…' : '确认处理'}
        </button>
      </div>
    </div>
  );
}

/** 决策收件箱条目（战略升级 / 决策者故障降级）：人工 approve/deny 直接裁定，
 *  retry 则把故障/降级的决策放回待裁定队列（恢复决策者实例后）。 */
function DecisionRow({
  item,
  projectName,
  focused,
}: {
  item: InboxItem;
  projectName: string;
  focused?: boolean;
}) {
  const { resolveInboxItem, selectProject, openTask, setView } = useDomo();
  const payload = (item.payload ?? {}) as Record<string, unknown>;
  const kind = typeof payload.kind === 'string' ? payload.kind : '';
  const scope = typeof payload.scope === 'string' ? payload.scope : '';
  const taskId = typeof payload.task_id === 'string' ? payload.task_id : '';
  const detail = typeof payload.detail === 'string' ? payload.detail : '';
  const reasons = Array.isArray(payload.reasons)
    ? (payload.reasons as unknown[]).filter((r): r is string => typeof r === 'string')
    : [];
  const request = payload.request;

  const [resolution, setResolution] = useState<'approve' | 'deny' | 'retry'>('approve');
  const [note, setNote] = useState('');
  const [criteria, setCriteria] = useState('');
  const [busy, setBusy] = useState(false);

  // 协商失败裁决：批准时可附上建议的验收标准
  const needsCriteria = kind === 'negotiation_failed';

  async function submit() {
    setBusy(true);
    const ok = await resolveInboxItem(
      item.id,
      resolution,
      note.trim(),
      undefined,
      needsCriteria && criteria.trim() ? criteria.trim() : undefined,
    );
    setBusy(false);
    if (ok) {
      setNote('');
      setCriteria('');
    }
  }

  function jumpToTask() {
    if (!taskId) return;
    selectProject(item.project_id);
    void openTask(taskId);
    setView('tasks');
  }

  return (
    <div id={`inbox-${item.id}`} className={`panel inbox-item${focused ? ' inbox-focus' : ''}`}>
      <div className="inbox-head">
        <Badge tone="purple">{INBOX_TYPE_LABEL[item.type] ?? item.type}</Badge>
        {kind && <Badge tone="blue">{DECISION_KIND_LABEL[kind] ?? kind}</Badge>}
        {scope && (
          <Badge tone={scope === 'strategic' ? 'amber' : 'gray'}>
            {DECISION_SCOPE_LABEL[scope] ?? scope}
          </Badge>
        )}
        <span>{projectName}</span>
        <span className="muted">{fmtTime(item.created_at)}</span>
        <span className="spacer" />
        <Badge tone="amber">待裁决</Badge>
      </div>

      <div className="stack" style={{ borderLeft: '3px solid var(--line, #ccc)', paddingLeft: 10 }}>
        <div>
          <strong>决策请求：</strong>
          {kind ? DECISION_KIND_LABEL[kind] ?? kind : '—'}
          <span className="mono muted"> #{shortId(payload.decision_id as string | undefined)}</span>
        </div>
        {taskId && (
          <div className="muted">
            关联任务 <span className="mono">#{shortId(taskId)}</span>
          </div>
        )}
        {reasons.length > 0 && <div className="muted">触发原因：{reasons.join('、')}</div>}
        {detail && <p style={{ margin: 0, whiteSpace: 'pre-wrap' }}>💡 {detail}</p>}
        {request != null && (
          <details className="json">
            <summary>request</summary>
            <pre>{JSON.stringify(request, null, 2)}</pre>
          </details>
        )}
        <p className="muted" style={{ margin: 0 }}>
          批准 = 按该请求裁定通过（重规划随后进入规划落地）；否决 = 关闭请求；重试 = 恢复决策者后重新自动裁决。
        </p>
      </div>

      {taskId && (
        <div className="row-end">
          <button className="btn" onClick={jumpToTask}>
            查看任务详情 / 日志 →
          </button>
        </div>
      )}

      <div className="resolve-row">
        <select
          value={resolution}
          onChange={e => setResolution(e.target.value as 'approve' | 'deny' | 'retry')}
          className="select"
        >
          <option value="approve">{RESOLUTION_LABEL.approve}</option>
          <option value="deny">{RESOLUTION_LABEL.deny}</option>
          <option value="retry">{RESOLUTION_LABEL.retry}</option>
        </select>
        {needsCriteria && resolution === 'approve' && (
          <input
            value={criteria}
            onChange={e => setCriteria(e.target.value)}
            placeholder="建议验收标准（可选）"
          />
        )}
        <input value={note} onChange={e => setNote(e.target.value)} placeholder="裁决备注（可选）" />
        <button className="btn btn-primary" disabled={busy} onClick={() => void submit()}>
          {busy ? '处理中…' : '处理'}
        </button>
      </div>
    </div>
  );
}

/** M3 里程碑 gate：确认通过放行下游分支，退回保持冻结等待重规划。 */
function GateRow({
  item,
  projectName,
  focused,
}: {
  item: InboxItem;
  projectName: string;
  focused?: boolean;
}) {
  const { resolveInboxItem, selectProject, setView } = useDomo();
  const payload = (item.payload ?? {}) as Record<string, unknown>;
  const mid = typeof payload.milestone_id === 'string' ? payload.milestone_id : '';
  const name = typeof payload.name === 'string' ? payload.name : '里程碑';
  const goal = typeof payload.goal === 'string' ? payload.goal : '';
  const effect = typeof payload.effect === 'string' ? payload.effect : '';
  const detail = typeof payload.detail === 'string' ? payload.detail : '';

  const [resolution, setResolution] = useState<'pass' | 'reject'>('pass');
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);

  async function submit() {
    setBusy(true);
    const ok = await resolveInboxItem(item.id, resolution, note.trim());
    setBusy(false);
    if (ok) setView('projects');
  }

  return (
    <div id={`inbox-${item.id}`} className={`panel inbox-item${focused ? ' inbox-focus' : ''}`}>
      <div className="inbox-head">
        <Badge tone="purple">{INBOX_TYPE_LABEL.milestone_gate}</Badge>
        <span>{projectName}</span>
        <span className="muted">{fmtTime(item.created_at)}</span>
        <span className="spacer" />
        <Badge tone="amber">待确认</Badge>
      </div>

      <div className="stack" style={{ borderLeft: '3px solid var(--line, #ccc)', paddingLeft: 10 }}>
        <div>
          <strong>里程碑：</strong>
          {name}
          <span className="muted"> #{shortId(mid)}</span>
        </div>
        {goal && <p className="muted" style={{ whiteSpace: 'pre-wrap', margin: 0 }}>目标：{goal}</p>}
        {effect && <p className="muted" style={{ whiteSpace: 'pre-wrap', margin: 0 }}>效果：{effect}</p>}
        {detail && <p style={{ margin: 0 }}>💡 {detail}</p>}
        <p className="muted" style={{ margin: 0 }}>
          确认通过后放行该里程碑的下游依赖分支；退回则下游保持冻结，等待重规划。
        </p>
      </div>

      <div className="row-end">
        <button className="btn" onClick={() => { selectProject(item.project_id); setView('tasks'); }}>
          查看项目任务 →
        </button>
      </div>

      <div className="resolve-row">
        <select value={resolution} onChange={e => setResolution(e.target.value as 'pass' | 'reject')} className="select">
          <option value="pass">{RESOLUTION_LABEL.pass}</option>
          <option value="reject">{RESOLUTION_LABEL.reject}</option>
        </select>
        <input value={note} onChange={e => setNote(e.target.value)} placeholder="确认备注（可选）" />
        <button className="btn btn-primary" disabled={busy} onClick={() => void submit()}>
          {busy ? '处理中…' : '处理'}
        </button>
      </div>
    </div>
  );
}

/** M3 里程碑窗口：gate 退回后重开的重规划窗口。
 *  人在窗口内【追加任务】或【调整后续规划】，逐项应用：
 *  - 新增任务默认 acceptance_required=true（标准留空则后端补兜底标准）；
 *  - 依赖按任务标题解析（既有任务 or 同批新增）；
 *  - 修改/新增依赖做 DAG 环校验，成环项被拒绝并汇报原因，其余项继续应用；
 *  - 应用写入事件审计，窗口收件箱项随即 resolved。 */
function WindowRow({
  item,
  projectName,
  focused,
}: {
  item: InboxItem;
  projectName: string;
  focused?: boolean;
}) {
  const { applyMilestoneWindow, tasksByProject, milestonesByProject, selectProject, setView } = useDomo();
  const payload = (item.payload ?? {}) as Record<string, unknown>;
  const mid = typeof payload.milestone_id === 'string' ? payload.milestone_id : '';
  const name = typeof payload.name === 'string' ? payload.name : '里程碑';
  const goal = typeof payload.goal === 'string' ? payload.goal : '';
  const effect = typeof payload.effect === 'string' ? payload.effect : '';
  const detail = typeof payload.detail === 'string' ? payload.detail : '';
  const rejectNote = typeof payload.note === 'string' ? payload.note : '';

  const tasks = tasksByProject[item.project_id] ?? [];
  const milestone = (milestonesByProject[item.project_id] ?? []).find(m => m.id === mid);
  const members = tasks.filter(t => t.milestone_id === mid);
  const nonTerminal = members.filter(t => !TERMINAL_STATUS.includes(t.status));

  // 待应用方案（有序，逐项应用）
  const [drafts, setDrafts] = useState<ReplanChange[]>([]);
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ApplyResult | null>(null);

  // 追加任务表单
  const [title, setTitle] = useState('');
  const [desc, setDesc] = useState('');
  const [criteria, setCriteria] = useState('');
  const [depTitles, setDepTitles] = useState<string[]>([]);

  // 调整既有任务（依赖按标题编辑）
  const [editId, setEditId] = useState<string | null>(null);
  const [editDeps, setEditDeps] = useState<string[]>([]);
  const [editDesc, setEditDesc] = useState('');
  const [editCriteria, setEditCriteria] = useState('');

  // 依赖候选：项目内非终态任务 + 待应用方案里新增的任务标题
  const draftAddTitles = drafts
    .filter(c => c.op === 'add')
    .map(c => {
      const t = (c.task ?? {}) as Record<string, unknown>;
      return typeof t.title === 'string' ? t.title : '';
    })
    .filter(Boolean);
  const existingTitles = tasks.filter(t => !TERMINAL_STATUS.includes(t.status)).map(t => t.title);
  const allTitles = Array.from(new Set([...existingTitles, ...draftAddTitles]));

  function addDraft() {
    const t = title.trim();
    if (!t) return;
    setDrafts(d => [
      ...d,
      {
        op: 'add',
        task: {
          title: t,
          description: desc.trim(),
          depends_on: depTitles,
          ...(criteria.trim() ? { acceptance_criteria: criteria.trim() } : {}),
        },
      },
    ]);
    setTitle('');
    setDesc('');
    setCriteria('');
    setDepTitles([]);
  }

  function openEdit(t: Task) {
    setEditId(t.id);
    setEditDeps(
      t.depends_on.map(id => tasks.find(x => x.id === id)?.title ?? id).filter(Boolean),
    );
    setEditDesc(t.description ?? '');
    setEditCriteria(t.acceptance_criteria ?? '');
  }

  function addUpdateDraft(t: Task) {
    setDrafts(d => [
      ...d,
      { op: 'update', task_id: t.id, fields: { depends_on: editDeps, description: editDesc, acceptance_criteria: editCriteria } },
    ]);
    setEditId(null);
  }

  function addCancelDraft(t: Task) {
    if (drafts.some(c => c.op === 'cancel' && c.task_id === t.id)) return;
    setDrafts(d => [...d, { op: 'cancel', task_id: t.id }]);
  }

  function removeDraft(i: number) {
    setDrafts(d => d.filter((_, idx) => idx !== i));
  }

  function changeText(c: ReplanChange): string {
    const op = String(c.op ?? '?');
    if (op === 'add') {
      const t = (c.task ?? {}) as Record<string, unknown>;
      const deps = Array.isArray(t.depends_on) ? (t.depends_on as string[]).join('、') : '';
      return `＋ 新增任务：「${t.title ?? '?'}」${deps ? `（依赖：${deps}）` : ''}`;
    }
    if (op === 'update') {
      const f = (c.fields ?? {}) as Record<string, unknown>;
      const keys = Object.keys(f).filter(k => f[k] !== undefined).join('、');
      const t = tasks.find(x => x.id === c.task_id);
      return `✎ 调整 ${t ? `「${t.title}」` : shortId(String(c.task_id ?? ''))}（${keys}）`;
    }
    if (op === 'cancel') {
      const t = tasks.find(x => x.id === c.task_id);
      return `✕ 取消 ${t ? `「${t.title}」` : shortId(String(c.task_id ?? ''))}`;
    }
    return `? ${JSON.stringify(c).slice(0, 120)}`;
  }

  async function submit() {
    if (drafts.length === 0 || busy) return;
    // 依赖按标题解析：先把 add 排前，保证同批新增可被后续 add/update 解析。
    const rank = (c: ReplanChange) => (c.op === 'add' ? 0 : c.op === 'update' ? 1 : 2);
    const ordered = drafts
      .map((c, i) => ({ c, i }))
      .sort((a, b) => rank(a.c) - rank(b.c) || a.i - b.i)
      .map(x => x.c);
    setBusy(true);
    const r = await applyMilestoneWindow(item.project_id, mid, ordered, note.trim());
    setBusy(false);
    if (r) {
      setResult(r);
      setDrafts([]);
    }
  }

  return (
    <div id={`inbox-${item.id}`} className={`panel inbox-item${focused ? ' inbox-focus' : ''}`}>
      <div className="inbox-head">
        <Badge tone="purple">{INBOX_TYPE_LABEL.replan}</Badge>
        <Badge tone="amber">里程碑窗口 · 待重规划</Badge>
        <span>{projectName}</span>
        <span className="muted">{fmtTime(item.created_at)}</span>
        <span className="spacer" />
        <button className="btn" onClick={() => { selectProject(item.project_id); setView('tasks'); }}>
          查看项目任务 →
        </button>
      </div>

      <div className="stack" style={{ borderLeft: '3px solid var(--line, #ccc)', paddingLeft: 10 }}>
        <div>
          <strong>里程碑：</strong>
          {name}
          <span className="muted"> #{shortId(mid)}</span>
          {milestone && (
            <Badge tone={MILESTONE_STATUS_LABEL[milestone.status]?.tone ?? 'gray'}>
              {MILESTONE_STATUS_LABEL[milestone.status]?.label ?? milestone.status}
            </Badge>
          )}
        </div>
        {goal && <p className="muted" style={{ whiteSpace: 'pre-wrap', margin: 0 }}>目标：{goal}</p>}
        {effect && <p className="muted" style={{ whiteSpace: 'pre-wrap', margin: 0 }}>效果：{effect}</p>}
        {rejectNote && <p style={{ margin: 0 }}>↩ 退回意见：{rejectNote}</p>}
        {detail && <p style={{ margin: 0 }}>💡 {detail}</p>}
        <p className="muted" style={{ margin: 0 }}>
          窗口成员任务（{members.length}）：{members.map(t => t.title).join('、') || '—'}
          {nonTerminal.length > 0 && ` · 待完成 ${nonTerminal.length}`}
        </p>
        <p className="muted" style={{ margin: 0 }}>
          新增任务默认带验收；依赖按标题填写（可引用同批新增）；成环的依赖调整会被拒绝并给出原因，其余项照常应用。
          窗口内成员全部完成后，里程碑会重新进入待确认。
        </p>
      </div>

      {/* 追加任务 */}
      <section className="drawer-section">
        <h4>① 追加任务</h4>
        <div className="form-grid">
          <label>
            标题 *
            <input value={title} onChange={e => setTitle(e.target.value)} placeholder="例：补齐环境准备步骤" />
          </label>
          <label>
            描述
            <textarea rows={2} value={desc} onChange={e => setDesc(e.target.value)} placeholder="任务说明" />
          </label>
          <label>
            验收标准（留空则自动补一条兜底标准）
            <textarea rows={2} value={criteria} onChange={e => setCriteria(e.target.value)} placeholder="例：准备产物可检验" />
          </label>
          <div>
            <div className="label-text">前置依赖（按标题，可多选 · {depTitles.length}）</div>
            <div className="dep-select">
              {allTitles.filter(t => t !== title.trim()).length === 0 ? (
                <span className="muted">暂无可作为依赖的任务</span>
              ) : (
                allTitles
                  .filter(t => t !== title.trim())
                  .map(t => (
                    <label key={t} className="check-label">
                      <input
                        type="checkbox"
                        checked={depTitles.includes(t)}
                        onChange={e =>
                          setDepTitles(prev => (e.target.checked ? [...prev, t] : prev.filter(x => x !== t)))
                        }
                      />
                      <span>{t}</span>
                    </label>
                  ))
              )}
            </div>
          </div>
        </div>
        <div className="row-end">
          <button className="btn" disabled={!title.trim()} onClick={addDraft}>
            加入待应用方案
          </button>
        </div>
      </section>

      {/* 调整后续规划 */}
      <section className="drawer-section">
        <h4>② 调整后续规划</h4>
        {tasks.length === 0 ? (
          <p className="muted">任务尚未加载</p>
        ) : (
          <ul className="dep-list">
            {tasks
              .filter(t => !TERMINAL_STATUS.includes(t.status))
              .map(t => (
                <li key={t.id} style={{ display: 'block' }}>
                  <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
                    <span>{t.title}</span>
                    <StatusBadge kind="task" value={t.status} />
                    {t.milestone_id === mid && <Badge tone="amber">窗口成员</Badge>}
                    <span className="spacer" />
                    <button className="btn" onClick={() => (editId === t.id ? setEditId(null) : openEdit(t))}>
                      {editId === t.id ? '收起' : '改依赖 / 字段'}
                    </button>
                    <button className="btn btn-danger-ghost" onClick={() => addCancelDraft(t)}>
                      取消
                    </button>
                  </div>
                  {editId === t.id && (
                    <div className="stack" style={{ marginTop: 6, paddingLeft: 8, borderLeft: '2px solid var(--line, #ccc)' }}>
                      <div>
                        <div className="label-text">依赖（按标题 · {editDeps.length}）</div>
                        <div className="dep-select">
                          {allTitles
                            .filter(o => o !== t.title)
                            .map(o => (
                              <label key={o} className="check-label">
                                <input
                                  type="checkbox"
                                  checked={editDeps.includes(o)}
                                  onChange={e =>
                                    setEditDeps(prev => (e.target.checked ? [...prev, o] : prev.filter(x => x !== o)))
                                  }
                                />
                                <span>{o}</span>
                              </label>
                            ))}
                        </div>
                      </div>
                      <label>
                        描述
                        <textarea rows={2} value={editDesc} onChange={e => setEditDesc(e.target.value)} />
                      </label>
                      <label>
                        验收标准
                        <textarea rows={2} value={editCriteria} onChange={e => setEditCriteria(e.target.value)} />
                      </label>
                      <div className="row-end">
                        <button className="btn" onClick={() => addUpdateDraft(t)}>
                          加入待应用方案
                        </button>
                      </div>
                    </div>
                  )}
                </li>
              ))}
          </ul>
        )}
      </section>

      {/* 待应用方案 */}
      <section className="drawer-section">
        <h4>③ 待应用方案（{drafts.length}）</h4>
        {drafts.length === 0 ? (
          <p className="muted">还没有待应用的调整。可在上面①追加任务或②调整规划后加入。</p>
        ) : (
          <ul>
            {drafts.map((c, i) => (
              <li key={i} style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                <span>{changeText(c)}</span>
                <span className="spacer" />
                <button className="icon-btn" onClick={() => removeDraft(i)} aria-label="移除">
                  ✕
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <div className="resolve-row">
        <input value={note} onChange={e => setNote(e.target.value)} placeholder="窗口重规划说明（可选）" />
        <button className="btn btn-primary" disabled={busy || drafts.length === 0} onClick={() => void submit()}>
          {busy ? '应用中…' : '应用窗口方案'}
        </button>
      </div>

      {result && (
        <section className="drawer-section highlight">
          <h4>应用结果</h4>
          <p>✓ 已应用 {result.applied.length} 项</p>
          {result.skipped.length > 0 && (
            <>
              <p style={{ margin: 0 }}>✕ 拒绝 {result.skipped.length} 项（其余项照常应用）：</p>
              <ul>
                {result.skipped.map((s, i) => (
                  <li key={i} className="muted">
                    {s.title ?? s.task_id ?? s.op ?? `#${s.index ?? i}`} — {s.reason}
                  </li>
                ))}
              </ul>
            </>
          )}
        </section>
      )}
    </div>
  );
}

function EscalationRow({
  item,
  projectName,
  focused,
}: {
  item: InboxItem;
  projectName: string;
  focused?: boolean;
}) {
  const { agents, resolveInboxItem, selectProject, openTask, setView } = useDomo();
  const payload = (item.payload ?? {}) as Record<string, unknown>;
  const reason = payload.reason;
  const badge = reasonBadge(reason);
  const ctx = item.context;
  const taskId = ctx?.task.id ?? (typeof payload.task_id === 'string' ? payload.task_id : '');

  function jumpToTask() {
    if (!taskId) return;
    selectProject(item.project_id);
    // openTask 拉取详情并打开抽屉
    void openTask(taskId);
    setView('tasks');
  }

  // 默认动作按真实失败环节推荐：验收基建失败 → 更换验收者；其余 → 恢复重派
  const hintKey = ctx?.last_failure_type
    ?? (reason === 'rejection_limit' ? 'acceptance.rejected'
      : reason === 'acceptance_failure_limit' ? 'acceptance.run_failed'
      : 'task.execution_failed');
  const [resolution, setResolution] = useState<string>(
    hintKey === 'acceptance.run_failed' ? 'reassign_acceptor' : 'resume'
  );
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);

  // 动作由后端按待决类型下发；缺省时回退到旧动作集（兼容旧快照）
  const actions: { id: string; label: string; needs_agent?: 'executor' | 'acceptor' }[] =
    item.actions && item.actions.length > 0
      ? item.actions
      : [
          { id: 'resume', label: '恢复重派' },
          { id: 'reassign', label: '更换执行者', needs_agent: 'executor' },
          { id: 'reassign_acceptor', label: '更换验收者', needs_agent: 'acceptor' },
          { id: 'relax', label: '放宽标准' },
          { id: 'abandon', label: '放弃任务' },
        ];
  const wantRole = actions.find(a => a.id === resolution)?.needs_agent;
  const [agentId, setAgentId] = useState<string>('');

  const candidates = wantRole === 'executor'
    ? agents.filter(a => a.roles.includes('executor'))
    : agents.filter(a => a.roles.includes('acceptor'));
  // 允许执行与验收同一实例（设计如此），仅排除决策者下场
  const dmIds = agents.filter(a => a.roles.includes('decision_maker')).map(a => a.id);
  // 切换裁决动作时，预选当前任务对应的 agent（方便只改最小项）
  function pickDefaultAgent(res: string) {
    const role = actions.find(a => a.id === res)?.needs_agent;
    if (!role || !ctx) return '';
    return role === 'executor' ? (ctx.executor?.id ?? '') : (ctx.acceptor?.id ?? '');
  }

  async function submit() {
    if (wantRole && !agentId) return;
    setBusy(true);
    await resolveInboxItem(item.id, resolution, note.trim(), wantRole ? agentId : undefined);
    setBusy(false);
  }

  return (
    <div id={`inbox-${item.id}`} className={`panel inbox-item${focused ? ' inbox-focus' : ''}`}>
      <div className="inbox-head">
        <Badge tone={item.type === 'escalation' ? badge.tone : 'purple'}>
          {INBOX_TYPE_LABEL[item.type] ?? item.type}
        </Badge>
        {item.type === 'escalation' && <Badge tone={badge.tone === 'red' ? 'gray' : 'amber'}>{badge.label}</Badge>}
        <span>{projectName}</span>
        <span className="muted">{fmtTime(item.created_at)}</span>
        <span className="spacer" />
        <Badge tone="amber">待处理</Badge>
      </div>

      {ctx ? <ContextPanel ctx={ctx} /> : <PayloadBlock payload={item.payload} />}

      {taskId && (
        <div className="row-end">
          <button className="btn" onClick={jumpToTask}>查看任务详情 / 日志 →</button>
        </div>
      )}

      <div className="resolve-row">
        <select value={resolution} onChange={e => { setResolution(e.target.value); setAgentId(pickDefaultAgent(e.target.value)); }} className="select">
          {actions.map(a => (
            <option key={a.id} value={a.id}>
              {a.label}
            </option>
          ))}
        </select>
        {wantRole && (
          <select value={agentId} onChange={e => setAgentId(e.target.value)} className="select">
            <option value="">选择{wantRole === 'executor' ? '执行者' : '验收者'}…</option>
            {candidates.filter(a => !dmIds.includes(a.id)).map(a => (
              <option key={a.id} value={a.id}>
                {a.name}（{a.roles.map(r => ROLE_LABEL[r] ?? r).join('/')} · {a.status === 'busy' ? '忙碌' : '空闲'}
                {a.adapter === 'opencode' && a.config?.model ? ` · ${a.config.model}` : ''})
              </option>
            ))}
          </select>
        )}
        <input
          value={note}
          onChange={e => setNote(e.target.value)}
          placeholder="裁决备注（可选）"
        />
        <button className="btn btn-primary" disabled={busy || (wantRole && !agentId)} onClick={() => void submit()}>
          {busy ? '处理中…' : '处理'}
        </button>
      </div>
    </div>
  );
}

function ResolutionDetail({ resolution }: { resolution: InboxItem['resolution'] }) {
  if (typeof resolution !== 'object' || resolution === null) return null;
  const r = resolution as Record<string, unknown>;
  const applied = Array.isArray(r.applied) ? r.applied : [];
  const skipped = Array.isArray(r.skipped) ? r.skipped : [];
  if (applied.length === 0 && skipped.length === 0) return null;
  return (
    <div className="stack" style={{ marginTop: 6 }}>
      {applied.length > 0 && <div className="muted">✓ 已应用 {applied.length} 项</div>}
      {skipped.length > 0 && (
        <div>
          <div style={{ color: 'var(--danger, #c0392b)' }}>✕ 拒绝 {skipped.length} 项（其余项照常应用）</div>
          <ul>
            {skipped.map((s, i) => {
              const it = (s ?? {}) as Record<string, unknown>;
              return (
                <li key={i} className="muted">
                  {String(it.title ?? it.task_id ?? it.op ?? `#${it.index ?? i}`)} — {String(it.reason ?? '')}
                </li>
              );
            })}
          </ul>
        </div>
      )}
    </div>
  );
}

function PayloadBlock({ payload }: { payload: unknown }) {
  return (
    <details className="json">
      <summary>payload</summary>
      <pre>{JSON.stringify(payload, null, 2)}</pre>
    </details>
  );
}
