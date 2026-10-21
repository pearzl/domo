import { useMemo, useState } from 'react';
import { useDomo } from '../store';
import type { Agent, ApplyResult, AuditEvent, InterveneResult, ReplanReportResult, Task, TaskDetail, TaskFormValue, TaskStatus } from '../types';
import { Empty, Modal, Pager, StatusBadge, TASK_STATUS_LABEL, TaskLink, fmtTime, shortId, useElapsed } from '../ui';
import { formatChecks } from '../checks';
import { TaskDag } from './Dag';
import { PoolTable } from './PoolTable';
const COLUMNS: TaskStatus[] = [
  'planned',
  'ready',
  'plan_drafting',
  'plan_review',
  'executing',
  'recovering',
  'awaiting_acceptance',
  'escalated',
  'suspended',
  'done',
];

export type TaskFormState = { mode: 'create' } | { mode: 'edit'; task: Task };

export function TasksView({ onOpenForm }: { onOpenForm: (s: TaskFormState) => void }) {
  const {
    projects,
    selectedProjectId,
    selectProject,
    tasksByProject,
    milestonesByProject,
    tasksViewMode,
    setTasksViewMode,
    dagDir,
    setDagDir,
    agents,
    selectedTaskId,
    taskDetail,
    openTask,
    closeTask,
    updateProject,
    projectAction,
    planProject,
    planning,
    setError,
  } = useDomo();
  const [intervene, setIntervene] = useState(false);
  // DAG 布局方向 tb=上下（默认）/ lr=左右：状态在全局 store，切页再切回不丢失
  // 初始化规划台：目标草稿 + 指定规划者/执行者/验收者 + 项目 agent 池
  const [briefDraft, setBriefDraft] = useState<string | null>(null);
  const [planPlanner, setPlanPlanner] = useState('');
  const [planFeedback, setPlanFeedback] = useState('');

  const planElapsed = useElapsed(planning?.startedAt);
  const tasks = selectedProjectId ? (tasksByProject[selectedProjectId] ?? []) : [];
  const milestones = selectedProjectId ? (milestonesByProject[selectedProjectId] ?? []) : [];
  const project = projects.find(p => p.id === selectedProjectId);

  if (!selectedProjectId || !project) {
    return (
      <div className="stack">
        <Empty text="请先在项目总览中选择一个项目。" />
        {projects.length > 0 && (
          <div className="chip-row">
            {projects.map(p => (
              <button key={p.id} className="chip" onClick={() => selectProject(p.id)}>
                {p.name}
              </button>
            ))}
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="stack">
      <div className="toolbar">
        <select
          value={selectedProjectId}
          onChange={e => selectProject(e.target.value)}
          className="select project-select"
        >
          {projects.map(p => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <StatusBadge kind="project" value={project.status} />
        <span className="muted">{tasks.length} 个任务</span>
        <div className="seg" role="tablist" aria-label="任务视图呈现">
          <button
            role="tab"
            aria-selected={tasksViewMode === 'kanban'}
            className={`seg-btn ${tasksViewMode === 'kanban' ? 'active' : ''}`}
            onClick={() => setTasksViewMode('kanban')}
          >
            看板
          </button>
          <button
            role="tab"
            aria-selected={tasksViewMode === 'dag'}
            className={`seg-btn ${tasksViewMode === 'dag' ? 'active' : ''}`}
            onClick={() => setTasksViewMode('dag')}
          >
            DAG 图
          </button>
        </div>
        <span className="spacer" />
        {project.status !== 'archived' && (
          <button
            className="btn"
            title="向规划者说明问题/要求，得到分析与调整方案后决定是否采纳"
            onClick={() => setIntervene(true)}
          >
            🧭 人工介入
          </button>
        )}
        {project.status !== 'archived' && (
          <button className="btn btn-primary" onClick={() => onOpenForm({ mode: 'create' })}>
            + 新建任务
          </button>
        )}
      </div>

      {/* 初始化规划台：目标文本框（与规划者/执行 agent 讨论用）+ agent 池 + 指定规划者/执行/验收 + 生成/确认规划 */}
      {project.status === 'initializing' && !project.plan?.confirmed_at && (
        <section className="panel">
          <h2 className="panel-title">规划台（初始化项目）</h2>
          <div className="stack">
            <label>
              项目目标（必填 · 作为初始化提示词，规划者据此拆解任务；可随时修改后重新生成规划）
              <textarea
                rows={3}
                value={briefDraft ?? project.brief ?? ''}
                onChange={e => setBriefDraft(e.target.value)}
                placeholder="交给团队的目标说明：做什么、做到什么程度、有什么约束……"
              />
            </label>
            <PoolTable projectId={project.id} />
            <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap' }}>
              <label className="muted">
                规划者（生成规划的 LLM 实例；任务 agent 不在此绑定，执行时按池优先级动态选择）
                <select value={planPlanner} onChange={e => setPlanPlanner(e.target.value)} className="select">
                  <option value="">自动选择</option>
                  {agents.filter(a => a.adapter === 'opencode' && a.roles.includes('planner')).map(a => (
                    <option key={a.id} value={a.id}>{a.name}</option>
                  ))}
                </select>
              </label>
              <span className="muted" style={{ alignSelf: 'end' }}>
                提示：可用右上角「🧭 人工介入」与规划者对话讨论任务拆解
              </span>
            </div>
            <label>
              对规划有异议？（可选）填写修改意见 → 规划者按意见重新规划全部任务与里程碑
              <textarea
                rows={2}
                value={planFeedback}
                onChange={e => setPlanFeedback(e.target.value)}
                placeholder="例：把审计相关任务合并为一个；先做会话稳定性再做审计；阶段划分按交付批次重排……"
              />
            </label>
            <div className="row-end">
              <button
                className="btn btn-primary"
                disabled={!!planning}
                title={tasks.length > 0
                  ? '按意见（可留空）重新规划：规划成功后原子替换现有任务与里程碑；失败则原计划保留'
                  : '由规划者拆解任务与里程碑'}
                onClick={() => {
                  void (async () => {
                    const brief = (briefDraft ?? project.brief ?? '').trim();
                    if (!brief) return void setError('必须填写项目目标，规划者据此拆解任务。');
                    if (brief !== (project.brief ?? '')) await updateProject(project.id, { brief });
                    if (tasks.length > 0) {
                      const okGo = window.confirm(
                        `将重新规划并替换当前 ${tasks.length} 个任务与 ${milestones.length} 个里程碑。\n` +
                        `规划失败时原计划会完整保留。继续？`
                      );
                      if (!okGo) return;
                    }
                    const added = await planProject(project.id, {
                      planner_agent_id: planPlanner || undefined,
                      feedback: planFeedback.trim() || undefined,
                    });
                    if (added >= 0) {
                      setBriefDraft(null);
                      setPlanFeedback('');
                    }
                  })();
                }}
              >
                {planning?.projectId === project.id
                  ? `规划者生成中… ${planElapsed}s`
                  : tasks.length > 0
                    ? '✨ 重新规划（替换现有计划）'
                    : '✨ 生成规划'}
              </button>
              <button
                className="btn btn-primary"
                disabled={tasks.length === 0 || !!planning}
                title={tasks.length === 0 ? '先生成或手动创建任务' : '确认任务拆分、里程碑与目标后，项目进入运行'}
                onClick={() => void projectAction(project.id, 'confirm')}
              >
                ✓ 确认规划，开始执行 →
              </button>
            </div>
          </div>
        </section>
      )}

      {tasks.length === 0 ? (
        <Empty text="该项目还没有任务，点右上角「新建任务」创建。" />
      ) : tasksViewMode === 'dag' ? (
        <TaskDag
          tasks={tasks}
          milestones={milestones}
          agents={agents}
          selectedTaskId={selectedTaskId}
          onOpenTask={id => void openTask(id)}
          dir={dagDir}
          onToggleDir={() => setDagDir(dagDir === 'tb' ? 'lr' : 'tb')}
        />
      ) : (
        <div className="kanban">
          {COLUMNS.map(col => {
            const colTasks = tasks.filter(t => t.status === col);
            const colLabel = TASK_STATUS_LABEL[col]?.label ?? col;
            return (
              <div key={col} className={`kanban-col col-${col}`}>
                <div className="kanban-col-head">
                  <span>{colLabel}</span>
                  <span className="kanban-count">{colTasks.length}</span>
                </div>
                <div className="kanban-cards">
                  {colTasks.map(t => (
                    <TaskCard
                      key={t.id}
                      task={t}
                      agents={agents}
                      active={t.id === selectedTaskId}
                      onClick={() => void openTask(t.id)}
                    />
                  ))}
                  {colTasks.length === 0 && <div className="kanban-empty">—</div>}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {taskDetail && <TaskDetailDrawer task={taskDetail} onClose={closeTask} onEdit={() => onOpenForm({ mode: 'edit', task: taskDetail })} />}
      {intervene && (
        <InterveneModal
          projectId={selectedProjectId}
          projectName={project.name}
          onClose={() => setIntervene(false)}
        />
      )}
    </div>
  );
}

/** 人工介入：用户说明 → 规划者分析探讨 → 展示方案 → 可采纳应用，可继续讨论 */
function InterveneModal({
  projectId,
  projectName,
  onClose,
}: {
  projectId: string;
  projectName: string;
  onClose: () => void;
}) {
  const { intervene, applyIntervention } = useDomo();
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [result, setResult] = useState<InterveneResult | null>(null);
  const [applied, setApplied] = useState<ApplyResult | null>(null);
  const [history, setHistory] = useState<{ note: string; reply: string; time: number }[]>([]);

  async function send(current: string) {
    if (!current.trim() || busy) return;
    setBusy(true);
    setElapsed(0);
    const started = Date.now();
    const timer = setInterval(() => setElapsed(Math.floor((Date.now() - started) / 1000)), 1000);
    const r = await intervene(projectId, current.trim());
    clearInterval(timer);
    setElapsed(0);
    setBusy(false);
    if (r) {
      setApplied(null); // 新一轮讨论作废上一轮的采纳结果状态
      setResult(r);
      setHistory(h => [{ note: current.trim(), reply: r.reply || r.analysis || '（规划者未给出回复）', time: Date.now() }, ...h]);
      setNote('');
    }
  }

  async function accept() {
    if (!result || busy) return;
    setBusy(true);
    const r = await applyIntervention(projectId, result.changes);
    setBusy(false);
    if (r) {
      setApplied(r);
    }
  }

  function changeText(c: Record<string, unknown>): string {
    const op = String(c.op ?? '?');
    if (op === 'add') {
      const t = (c.task ?? {}) as Record<string, unknown>;
      const deps = Array.isArray(t.depends_on) ? (t.depends_on as string[]).join('、') : '';
      return `＋ 新增任务：「${t.title ?? '?'}」${deps ? `（依赖：${deps}）` : ''}`;
    }
    if (op === 'update') {
      const f = (c.fields ?? {}) as Record<string, unknown>;
      const keys = Object.keys(f).filter(k => f[k] !== undefined).join('、');
      return `✎ 修改任务 ${shortId(String(c.task_id ?? ''))}${keys ? `（字段：${keys}）` : ''}`;
    }
    if (op === 'cancel') return `✕ 取消任务 ${shortId(String(c.task_id ?? ''))}`;
    return `? ${JSON.stringify(c).slice(0, 120)}`;
  }

  return (
    <Modal title={`人工介入 · ${projectName}`} onClose={onClose}>
      <div className="stack">
        <div className="ws-hint">方案由规划者提出，你确认后才应用到任务看板；应用后可随时再继续讨论。</div>
        <label>
          说明你想调整的内容（问题、要求、方向……规划者将结合当前任务全貌分析）
          <textarea
            rows={4}
            value={note}
            onChange={e => setNote(e.target.value)}
            placeholder="例：按当前进度，登录模块的方案风险太高，改成渐进式交付；另外把测试环境部署从本次范围里拿掉"
          />
        </label>
        <div className="row-end">
          <button
            className="btn btn-primary"
            disabled={busy || !note.trim()}
            onClick={() => void send(note)}
          >
            {busy || elapsed > 0 ? `规划者分析中… ${elapsed}s` : '发送给规划者'}
          </button>
        </div>

        {history.length > 0 && (
          <div className="stack">
            {history.map((h, i) => (
              <section key={h.time} className={`drawer-section${i === 0 ? ' highlight' : ''}`}>
                <h4>第 {history.length - i} 轮 · {fmtTime(h.time)}</h4>
                <p className="muted">你：{h.note}</p>
                <p style={{ whiteSpace: 'pre-wrap' }}>{h.reply}</p>
              </section>
            ))}
          </div>
        )}

        {result && (
          <section className="drawer-section">
            <h4>分析</h4>
            <p className="muted" style={{ whiteSpace: 'pre-wrap' }}>{result.analysis || '（无）'}</p>
            {result.changes.length === 0 ? (
              <p>规划者认为当前无需改动，可继续输入进行探讨。</p>
            ) : (
              <>
                <h4>调整方案（{result.changes.length} 项）</h4>
                <ul>
                  {result.changes.map((c, i) => (
                    <li key={i}>{changeText(c as Record<string, unknown>)}</li>
                  ))}
                </ul>
                <div className="row-end">
                  <button className="btn btn-primary" disabled={busy} onClick={() => void accept()}>
                    采纳方案并应用
                  </button>
                </div>
              </>
            )}
          </section>
        )}

        {applied && (
          <section className="drawer-section highlight">
            <h4>应用结果</h4>
            <p>✓ 已应用 {applied.applied.length} 项</p>
            {applied.skipped.length > 0 && (
              <ul>
                {applied.skipped.map((s, i) => (
                  <li key={i} className="muted">✕ 跳过：{s.title ?? s.task_id ?? s.op ?? `#${s.index}`} — {s.reason}</li>
                ))}
              </ul>
            )}
          </section>
        )}

        <div className="row-end">
          <button className="btn" onClick={onClose}>关闭</button>
        </div>
      </div>
    </Modal>
  );
}

function TaskCard({
  task,
  agents,
  active,
  onClick,
}: {
  task: Task;
  agents: Agent[];
  active: boolean;
  onClick: () => void;
}) {
  const executor = agents.find(a => a.id === task.executor_agent_id);
  return (
    <button className={`task-card ${active ? 'active' : ''}`} onClick={onClick}>
      <div className="task-card-title">{task.title}</div>
      <div className="task-card-meta">
        <span className="muted">依赖 {task.depends_on?.length ?? 0}</span>
        <span className="muted">
          尝试 {task.attempt} · 退回 {task.rejection_count}
          {task.failure_count > 0 ? ` · 失败 ${task.failure_count}` : ''}
        </span>
      </div>
      <div className="task-card-foot">
        <span className="agent-tag">{executor ? executor.name : '未指派'}</span>
        {task.artifacts.length > 0 && <span className="muted">产物 {task.artifacts.length}</span>}
      </div>
    </button>
  );
}

/** 任务计划摘要：只展示总结性质的前若干要点，完整计划与原始输出指向归档。 */
function PlanSummary({
  plan,
  outputs,
}: {
  plan: string;
  outputs: { id: number; ts: number; attempt: number; outcome: string }[];
}) {
  const [expanded, setExpanded] = useState(false);
  // 取前若干行/字符作为摘要（跳过空行，最多 12 行、600 字）
  const lines = plan.split('\n').filter(l => l.trim().length > 0);
  const summaryLines = expanded ? lines : lines.slice(0, 12);
  let summary = summaryLines.join('\n');
  if (!expanded && summary.length > 600) summary = summary.slice(0, 600) + '……';
  const latest = outputs.length > 0 ? outputs[outputs.length - 1] : null;
  return (
    <div className="stack">
      <pre className="pre-wrap" style={{ fontSize: 12, margin: 0 }}>{summary}</pre>
      <div className="row-end" style={{ gap: 10 }}>
        {(lines.length > summaryLines.length || plan.length > summary.length) && (
          <button className="btn btn-sm" onClick={() => setExpanded(e => !e)}>
            {expanded ? '收起摘要' : '展开摘要'}
          </button>
        )}
        {latest ? (
          <a className="btn btn-sm" href={`/api/agent-outputs/${latest.id}`} target="_blank" rel="noreferrer">
            查看完整计划与 agent 原始输出 →
          </a>
        ) : (
          <span className="muted" style={{ fontSize: 12 }}>（完整计划见下方「agent 原始输出归档」）</span>
        )}
      </div>
    </div>
  );
}

function TaskDetailDrawer({
  task,
  onClose,
  onEdit,
}: {
  task: TaskDetail;
  onClose: () => void;
  onEdit: () => void;
}) {
  const { tasksByProject, agents, logsByTask, sendGuidance, reviewTask, reportReplan, distillGuidance } = useDomo();
  const siblings = tasksByProject[task.project_id] ?? [];

  const [guidance, setGuidance] = useState('');
  const [comment, setComment] = useState('');
  const [busy, setBusy] = useState(false);
  const [localErr, setLocalErr] = useState('');
  const [replanReason, setReplanReason] = useState('');
  const [replanDetail, setReplanDetail] = useState('');
  const [replanResult, setReplanResult] = useState<ReplanReportResult | null>(null);
  const [replanErr, setReplanErr] = useState('');

  const logs = logsByTask[task.id] ?? task.logs;
  const executor = agents.find(a => a.id === task.executor_agent_id);
  const acceptor = agents.find(a => a.id === task.acceptor_agent_id);
  // M5：本任务最新一条人工指导事件（一键「沉淀为规范」的来源）
  const latestGuidanceEv = [...(task.events ?? [])]
    .reverse()
    .find(e => e.type === 'task.guidance_added');

  async function doReview(verdict: 'accept' | 'reject') {
    if (verdict === 'reject' && !comment.trim()) {
      setLocalErr('退回必须填写意见');
      return;
    }
    setBusy(true);
    const ok = await reviewTask(task.id, verdict, comment.trim());
    setBusy(false);
    if (ok) setComment('');
  }

  async function doGuidance() {
    if (!guidance.trim()) return;
    setBusy(true);
    const ok = await sendGuidance(task.id, guidance.trim());
    setBusy(false);
    if (ok) setGuidance('');
  }

  /** 上报「发现规划问题」：写入人工收件箱，由人批准或否决。 */
  async function doReportReplan() {
    if (!replanReason.trim()) return;
    setBusy(true);
    setReplanErr('');
    setReplanResult(null);
    const res = await reportReplan(task.id, {
      reason: replanReason.trim(),
      detail: replanDetail.trim() || undefined,
    });
    setBusy(false);
    if (res) {
      setReplanResult(res);
      setReplanReason('');
      setReplanDetail('');
    } else {
      setReplanErr('上报失败，请稍后重试');
    }
  }

  const terminal = task.status === 'done' || task.status === 'cancelled';

  return (
    <div className="drawer-wrap">
      <div className="drawer-overlay" onClick={onClose} />
      <aside className="drawer">
        <div className="drawer-head">
          <div>
            <div className="drawer-title">{task.title}</div>
            <div className="drawer-sub">
              <StatusBadge kind="task" value={task.status} />
              <span className="mono muted">#{shortId(task.id)}</span>
            </div>
          </div>
          <button className="icon-btn" onClick={onClose} aria-label="关闭">
            ✕
          </button>
        </div>

        {localErr && <div className="form-error">{localErr}</div>}

        <section className="drawer-section">
          <h4>基本信息</h4>
          <div className="meta-grid">
            <span className="muted">执行 agent</span><span>{executor ? executor.name : '—'}</span>
            <span className="muted">验收 agent</span><span>{acceptor ? acceptor.name : '—'}</span>
            <span className="muted">尝试次数</span><span>{task.attempt}</span>
            <span className="muted">退回 / 失败</span>
            <span>{task.rejection_count} / {task.failure_count}</span>
            <span className="muted">验收要求</span>
            <span>{task.acceptance_required ? '需要验收' : '豁免'}</span>
            {task.executor_agent_id && task.executor_agent_id === task.acceptor_agent_id && (
              <>
                <span className="muted">独立性</span>
                <span>⚠ 执行与验收同一实例（独立性有限）</span>
              </>
            )}
            <span className="muted">创建时间</span><span>{fmtTime(task.created_at)}</span>
          </div>
        </section>

        <section className="drawer-section">
          <h4>描述</h4>
          <p className="pre-wrap">{task.description || '（无）'}</p>
        </section>

        <section className="drawer-section">
          <h4>验收标准</h4>
          <p className="pre-wrap">{task.acceptance_criteria || '（未填写）'}</p>
        </section>

        <section className="drawer-section">
          <h4>机械检查（系统执行 · 证据）</h4>
          {(() => {
            const checks = task.acceptance_checks ?? [];
            const evidence = task.acceptance_evidence ?? [];
            if (checks.length === 0 && evidence.length === 0) {
              return <p className="muted">未配置机械检查。验收者会用自己的工具核查其余要求。</p>;
            }
            const rows = checks.map((c, i) => {
              const ev = evidence[i];
              const label =
                c.type === 'command'
                  ? `命令：${c.cmd}（期望退出码 ${c.expect_exit ?? 0}）`
                  : c.type === 'file_exists'
                    ? `文件存在：${c.path}`
                    : `文件不存在：${c.path}`;
              return { label, ev };
            });
            return (
              <div className="stack">
                {rows.map((r, i) => (
                  <div key={i} className="risk-line" style={{ margin: 0 }}>
                    <span>{r.ev ? (r.ev.passed ? '✓' : '✗') : '·'}</span>
                    <span>{r.label}</span>
                    {r.ev?.detail && (
                      <span className="muted" style={{ marginLeft: 6 }}>
                        {r.ev.detail.length > 200 ? `${r.ev.detail.slice(0, 200)}…` : r.ev.detail}
                      </span>
                    )}
                  </div>
                ))}
                {evidence.length === 0 && <p className="muted">尚未执行（任务进入验收时自动运行）。</p>}
              </div>
            );
          })()}
        </section>

        <section className="drawer-section">
          <h4>agent 原始输出归档（{(task as unknown as { agent_outputs?: unknown[] }).agent_outputs?.length ?? 0}）</h4>
          {(() => {
            const outs = (task as unknown as { agent_outputs?: { id: number; phase: string; attempt: number; ts: number; outcome: string; text: string; raw_len: number }[] }).agent_outputs ?? [];
            if (outs.length === 0) return <p className="muted">暂无归档（每次 agent 运行都会自动存档，供审计回放）</p>;
            return (
              <div className="stack">
                {outs.slice().reverse().map(o => (
                  <details key={o.id} className="json">
                    <summary>
                      {o.phase || 'run'} · 第 {o.attempt} 轮 · {o.outcome} · {fmtTime(o.ts)}
                      {o.raw_len > 0 ? ` · 原始流 ${Math.round(o.raw_len / 1024)}KB` : ''}
                    </summary>
                    <pre style={{ whiteSpace: 'pre-wrap', fontSize: 11.5 }}>{o.text || '（无文本输出）'}</pre>
                    <p className="muted">
                      <a href={`/api/agent-outputs/${o.id}`} target="_blank" rel="noreferrer">查看完整记录（含原始 stdout 流）</a>
                    </p>
                  </details>
                ))}
              </div>
            );
          })()}
        </section>

        <section className="drawer-section">
          <h4>任务计划摘要（执行者制定 · 验收者审查）</h4>
          {(task.plan_rejection_count ?? 0) > 0 && (
            <p className="muted">计划已被打回 {task.plan_rejection_count} 轮</p>
          )}
          {task.execution_plan ? (
            <PlanSummary plan={task.execution_plan} outputs={(task.agent_outputs ?? []).filter(o => o.phase === 'plan_task')} />
          ) : (
            <p className="muted">（尚未制定；任务进入「制定计划中」后产出）</p>
          )}
        </section>

        <section className="drawer-section">
          <h4>依赖（{task.depends_on?.length ?? 0}）</h4>
          {(task.depends_on?.length ?? 0) === 0 ? (
            <p className="muted">无前置依赖</p>
          ) : (
            <ul className="dep-list">
              {task.depends_on.map(id => {
                const dep = siblings.find(t => t.id === id);
                return (
                  <li key={id}>
                    {dep ? (
                      <>
                        <TaskLink taskId={id} projectId={task.project_id}>{dep.title}</TaskLink>
                        <StatusBadge kind="task" value={dep.status} />
                      </>
                    ) : (
                      <span className="mono muted">#{shortId(id)}</span>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </section>

        <section className="drawer-section">
          <h4>产物（{task.artifacts.length}）</h4>
          {task.artifacts.length === 0 ? (
            <p className="muted">暂无产物</p>
          ) : (
            <ul className="dep-list mono">
              {task.artifacts.map((a, i) => (
                <li key={i}>{a}</li>
              ))}
            </ul>
          )}
        </section>

        {task.status === 'awaiting_acceptance' && (
          <section className="drawer-section highlight">
            <h4>人工验收</h4>
            <textarea
              rows={2}
              value={comment}
              onChange={e => setComment(e.target.value)}
              placeholder="验收意见（退回时必填）"
            />
            <div className="btn-row">
              <button className="btn btn-primary" disabled={busy} onClick={() => void doReview('accept')}>
                验收通过
              </button>
              <button className="btn btn-danger-ghost" disabled={busy} onClick={() => void doReview('reject')}>
                退回重做
              </button>
            </div>
          </section>
        )}

        <section className="drawer-section">
          <h4>人工指导</h4>
          <textarea
            rows={2}
            value={guidance}
            onChange={e => setGuidance(e.target.value)}
            placeholder="写入指导，作用于当前/下次尝试"
          />
          <div className="row-end">
            {latestGuidanceEv && (
              <button
                className="btn"
                title="把最新一条人工指导沉淀为项目规范（随 SQLite 持久化，注入后续 planner/executor/acceptor 提示词）"
                onClick={() =>
                  distillGuidance({
                    event_id: latestGuidanceEv.id,
                    project_id: latestGuidanceEv.project_id,
                    text:
                      typeof (latestGuidanceEv.payload as Record<string, unknown>)?.text === 'string'
                        ? ((latestGuidanceEv.payload as Record<string, unknown>).text as string)
                        : '',
                  })
                }
              >
                沉淀为规范
              </button>
            )}
            <button className="btn" disabled={busy || !guidance.trim()} onClick={() => void doGuidance()}>
              发送指导
            </button>
          </div>
        </section>

        {!terminal && (
          <section className="drawer-section">
            <h4>报告规划问题</h4>
            <p className="muted" style={{ margin: 0 }}>
              执行/验收中若发现规划本身有问题，可上报到收件箱；由你批准或否决。
              批准后由规划者复核并落地（新增任务带验收、成环依赖跳过）。
            </p>
            {replanResult ? (
              <div className="stack">
                <p style={{ margin: 0 }}>
                  ✓ 已写入收件箱，等待你批准或否决
                  {replanResult.reasons.length > 0 ? `（触发原因：${replanResult.reasons.join('、')}）` : ''}。
                </p>
                <div className="row-end">
                  <button className="btn" onClick={() => setReplanResult(null)}>
                    再报一条
                  </button>
                </div>
              </div>
            ) : (
              <>
                <textarea
                  rows={2}
                  value={replanReason}
                  onChange={e => setReplanReason(e.target.value)}
                  placeholder="问题描述（必填）：例：当前依赖顺序会导致返工，缺少环境准备步骤"
                />
                <textarea
                  rows={2}
                  value={replanDetail}
                  onChange={e => setReplanDetail(e.target.value)}
                  placeholder="补充说明（可选）"
                />
                {replanErr && <div className="form-error">{replanErr}</div>}
                <div className="row-end">
                  <button
                    className="btn"
                    disabled={busy || !replanReason.trim()}
                    onClick={() => void doReportReplan()}
                  >
                    上报到收件箱
                  </button>
                </div>
              </>
            )}
          </section>
        )}

        <AttemptLogSection task={task} logs={logs} />

        <section className="drawer-section">
          <h4>事件时间线（{task.events.length}）</h4>
          <TimelineView events={task.events} />
        </section>

        <div className="drawer-foot">
          <button className="btn" onClick={onEdit}>
            编辑任务
          </button>
          <span className="muted">仅未开始的任务可编辑</span>
        </div>
      </aside>
    </div>
  );
}

/**
 * 执行日志按「尝试轮次」拆分：以 agent.session_started（execute/accept 两阶段）
 * 事件为窗口边界切分日志；轮次间最新在前，轮次内保持时序正序。
 * 不依赖后端结构变化，历史日志天然适配。
 */
function groupLogsByAttempt(task: TaskDetail, logs: { ts: number; line: string }[]) {
  type Win = { key: string; label: string; start: number; end: number; phase: string };
  const starts = [...task.events]
    .filter(e => e.type === 'agent.session_started')
    .map(e => {
      const p = (e.payload ?? {}) as Record<string, unknown>;
      const role = typeof p.role === 'string' ? p.role : '';
      const phase = String(p.phase ?? 'execute');
      return {
        ts: e.ts,
        phase,
        role,
        attempt: Number(p.attempt ?? 0),
      };
    })
    .sort((a, b) => a.ts - b.ts);
  const wins: Win[] = starts.map((s, i) => ({
    key: `${s.phase}-${s.role}-${s.attempt}-${s.ts}`,
    label:
      s.phase === 'accept' ? `验收 · 第 ${s.attempt} 轮`
      : s.phase === 'plan' ? '规划会话'
      : `执行 · 第 ${s.attempt} 轮`,
    start: s.ts,
    end: starts[i + 1]?.ts ?? Number.MAX_SAFE_INTEGER,
    phase: s.phase,
  }));
  const groups = new Map<string, { label: string; start: number; lines: { ts: number; line: string }[] }>();
  for (const l of logs) {
    const w = wins.find(w => l.ts >= w.start && l.ts < w.end);
    const key = w?.key ?? 'unassigned';
    const label = w?.label ?? '其他 / 未归类';
    if (!groups.has(key)) groups.set(key, { label, start: w?.start ?? 0, lines: [] });
    groups.get(key)!.lines.push(l);
  }
  // 全部轮次按开始时间倒序（最新在前）
  return [...groups.entries()]
    .sort((a, b) => b[1].start - a[1].start)
    .map(([key, g]) => ({ key, ...g }));
}

function AttemptLogSection({ task, logs }: { task: TaskDetail; logs: { ts: number; line: string }[] }) {
  const groups = useMemo(() => groupLogsByAttempt(task, logs), [task, logs]);
  const [sel, setSel] = useState<string | null>(null);
  if (groups.length === 0) {
    return (
      <section className="drawer-section">
        <h4>执行日志（0）</h4>
        <LogView lines={[]} />
      </section>
    );
  }
  const active = groups.find(g => g.key === sel) ?? groups[0]; // 默认最新一轮
  return (
    <section className="drawer-section">
      <h4>
        执行日志（共 {logs.length} 行 · {groups.length} 轮）
      </h4>
      <select
        className="select"
        value={active.key}
        onChange={e => setSel(e.target.value)}
        style={{ marginBottom: 8 }}
      >
        {groups.map(g => (
          <option key={g.key} value={g.key}>
            {g.label} · {g.lines.length} 行 · {fmtTime(g.lines[0]?.ts)}
          </option>
        ))}
      </select>
      <LogView lines={active.lines} />
    </section>
  );
}

/** 任务事件时间线：类型筛选（复选框组合）+ 倒序 + 分页（每页 15） */
const PAGE_SIZE = 15;

const EV_GROUPS: { key: string; label: string; prefix: string }[] = [
  { key: 'task', label: '任务', prefix: 'task.' },
  { key: 'acceptance', label: '验收', prefix: 'acceptance.' },
  { key: 'session', label: 'agent 会话', prefix: 'agent.' },
  { key: 'escalation', label: '升级', prefix: 'escalation.' },
  { key: 'guidance', label: '指导', prefix: 'task.guidance' },
];

function TimelineView({ events }: { events: AuditEvent[] }) {
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [page, setPage] = useState(0);

  const filtered = useMemo(
    () =>
      [...events]
        .filter(e => {
          if (checked.size === 0) return true;
          return [...checked].some(k => {
            const g = EV_GROUPS.find(x => x.key === k);
            return g ? e.type.startsWith(g.prefix) : e.type === k;
          });
        })
        .sort((a, b) => b.id - a.id), // 最新在前
    [events, checked],
  );
  const totalPages = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const shown = filtered.slice(page * PAGE_SIZE, page * PAGE_SIZE + PAGE_SIZE);

  return (
    <>
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 10, marginBottom: 8 }}>
        {EV_GROUPS.map(g => (
          <label key={g.key} className="muted" style={{ cursor: 'pointer', fontSize: 12 }}>
            <input
              type="checkbox"
              checked={checked.has(g.key)}
              onChange={() => {
                setChecked(prev => {
                  const next = new Set(prev);
                  if (next.has(g.key)) next.delete(g.key);
                  else next.add(g.key);
                  return next;
                });
                setPage(0);
              }}
            />{' '}
            {g.label}
          </label>
        ))}
      </div>
      <Pager page={page} totalPages={totalPages} total={filtered.length} onPage={setPage} label="条事件" />
      {filtered.length === 0 ? (
        <p className="muted">暂无符合条件的事件</p>
      ) : (
        <ul className="timeline">
          {shown.map(ev => (
            <li key={ev.id}>
              <div className="timeline-line">
                <span className="mono muted">{fmtTime(ev.ts)}</span>
                <span className="mono ev-type">{ev.type}</span>
              </div>
              <details className="json">
                <summary>payload</summary>
                <pre>{JSON.stringify(ev.payload, null, 2)}</pre>
              </details>
            </li>
          ))}
        </ul>
      )}
      <Pager page={page} totalPages={totalPages} total={filtered.length} onPage={setPage} label="条事件" />
    </>
  );
}

function LogView({ lines }: { lines: { ts: number; line: string }[] }) {
  return (
    <div className="logs">
      {lines.length === 0 ? (
        <div className="muted">暂无日志输出</div>
      ) : (
        lines.map((l, i) => (
          <div key={i} className="log-line">
            <span className="log-ts">{fmtTime(l.ts)}</span>
            {l.line}
          </div>
        ))
      )}
    </div>
  );
}

export function TaskFormModal({
  projectId,
  mode,
  task,
  onClose,
}: {
  projectId: string;
  mode: 'create' | 'edit';
  task?: Task;
  onClose: () => void;
}) {
  const { agents, tasksByProject, submitTaskForm } = useDomo();
  const all = tasksByProject[projectId] ?? [];

  const [value, setValue] = useState<TaskFormValue>(() =>
    mode === 'edit' && task
      ? {
          title: task.title,
          description: task.description ?? '',
          depends_on: task.depends_on ?? [],
          executor_agent_id: task.executor_agent_id ?? '',
          acceptor_agent_id: task.acceptor_agent_id ?? '',
          acceptance_required: task.acceptance_required ?? true,
          acceptance_criteria: task.acceptance_criteria ?? '',
          acceptance_checks_text: formatChecks(task.acceptance_checks),
        }
      : {
          title: '',
          description: '',
          depends_on: [],
          executor_agent_id: '',
          acceptor_agent_id: '',
          acceptance_required: true,
          acceptance_criteria: '',
          acceptance_checks_text: '',
        },
  );
  const [err, setErr] = useState('');
  const [busy, setBusy] = useState(false);

  const set = <K extends keyof TaskFormValue>(k: K, v: TaskFormValue[K]) =>
    setValue(prev => ({ ...prev, [k]: v }));

  const executorAgents = agents.filter(a => a.roles.includes('executor'));
  const acceptorAgents = agents.filter(a => a.roles.includes('acceptor'));
  const depOptions = all.filter(t => t.id !== task?.id);

  async function submit() {
    setErr('');
    if (!value.title.trim()) return void setErr('请填写任务标题');
    if (!value.executor_agent_id) return void setErr('请选择执行 agent');
    if (!value.acceptor_agent_id) return void setErr('请选择验收 agent');
    setBusy(true);
    const ok = await submitTaskForm(projectId, value, mode === 'edit' ? task?.id : undefined);
    setBusy(false);
    if (ok) onClose();
  }

  return (
    <Modal title={mode === 'edit' ? '编辑任务' : '新建任务'} onClose={onClose} wide>
      {err && <div className="form-error">{err}</div>}
      <div className="form-grid">
        <label>
          标题 *
          <input value={value.title} onChange={e => set('title', e.target.value)} placeholder="任务标题" />
        </label>
        <label>
          描述
          <textarea
            rows={3}
            value={value.description}
            onChange={e => set('description', e.target.value)}
            placeholder="任务说明。mock 适配器：含 [fail] 则执行失败，含 [reject] 则验收退回"
          />
        </label>
        <div className="form-pair">
          <label>
            执行 agent *
            <select value={value.executor_agent_id} onChange={e => set('executor_agent_id', e.target.value)}>
              <option value="">— 选择 —</option>
              {executorAgents.map(a => (
                <option key={a.id} value={a.id}>
                  {a.name}（{a.status === 'busy' ? '忙碌' : '空闲'}）
                </option>
              ))}
              {value.executor_agent_id && !executorAgents.some(a => a.id === value.executor_agent_id) && (
                <option value={value.executor_agent_id}>{shortId(value.executor_agent_id)}</option>
              )}
            </select>
          </label>
          <label>
            验收 agent *
            <select value={value.acceptor_agent_id} onChange={e => set('acceptor_agent_id', e.target.value)}>
              <option value="">— 选择 —</option>
              {acceptorAgents.map(a => (
                <option key={a.id} value={a.id}>
                  {a.name}（{a.status === 'busy' ? '忙碌' : '空闲'}）
                </option>
              ))}
              {value.acceptor_agent_id && !acceptorAgents.some(a => a.id === value.acceptor_agent_id) && (
                <option value={value.acceptor_agent_id}>{shortId(value.acceptor_agent_id)}</option>
              )}
            </select>
          </label>
        </div>
        {value.executor_agent_id && value.executor_agent_id === value.acceptor_agent_id && (
          <p className="muted" style={{ margin: 0 }}>
            ⚠ 执行与验收为同一实例：独立性有限，建议为验收另配实例（允许继续）。
          </p>
        )}
        <div className="form-pair">
          <label className="check-label">
            <input
              type="checkbox"
              checked={value.acceptance_required}
              onChange={e => set('acceptance_required', e.target.checked)}
            />
            需要验收
          </label>
          <span />
        </div>
        <label>
          验收标准
          <textarea
            rows={2}
            value={value.acceptance_criteria}
            onChange={e => set('acceptance_criteria', e.target.value)}
            placeholder="可检验的条目（如：输出文件存在、命令返回 0）"
          />
        </label>
        <label>
          机械检查（每行一条，系统在任务工作区自动执行）
          <textarea
            rows={3}
            value={value.acceptance_checks_text}
            onChange={e => set('acceptance_checks_text', e.target.value)}
            placeholder={'file: hello.txt\nnot-file: tmp/out.log\ncmd: cargo test => 0'}
          />
        </label>
        <div>
          <div className="label-text">前置依赖（{value.depends_on.length}）</div>
          <div className="dep-select">
            {depOptions.length === 0 ? (
              <span className="muted">当前没有可作为依赖的任务</span>
            ) : (
              depOptions.map(t => (
                <label key={t.id} className="check-label">
                  <input
                    type="checkbox"
                    checked={value.depends_on.includes(t.id)}
                    onChange={e =>
                      set(
                        'depends_on',
                        e.target.checked
                          ? [...value.depends_on, t.id]
                          : value.depends_on.filter(x => x !== t.id),
                      )
                    }
                  />
                  <span>{t.title}</span>
                  <StatusBadge kind="task" value={t.status} />
                </label>
              ))
            )}
          </div>
        </div>
      </div>
      <div className="row-end">
        <button className="btn" onClick={onClose}>
          取消
        </button>
        <button className="btn btn-primary" disabled={busy} onClick={() => void submit()}>
          {busy ? '提交中…' : mode === 'edit' ? '保存修改' : '创建任务'}
        </button>
      </div>
    </Modal>
  );
}
