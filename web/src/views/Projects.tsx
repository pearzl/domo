import { useEffect, useState } from 'react';
import { useDomo } from '../store';
import type { InboxItem, Project, ProjectBaseline, Workspace } from '../types';
import { Badge, ConfirmDialog, Empty, MILESTONE_STATUS_LABEL, Modal, StatusBadge, fmtTime } from '../ui';
import { ReportModal } from './Report';

const OPEN_STATUSES = ['initializing', 'pending_confirm', 'running', 'pausing', 'paused'];

type WsKind = '' | 'local' | 'git';

interface WsFields {
  kind: WsKind;
  path: string;
  url: string;
  branch: string;
}

function fieldsToWorkspace(f: WsFields): Workspace | null {
  // 任一必填输入留空 → 视为不设置工作区
  if (f.kind === 'local' && f.path.trim()) return { kind: 'local', path: f.path.trim() };
  if (f.kind === 'git' && f.url.trim()) {
    const b = f.branch.trim();
    return b ? { kind: 'git', url: f.url.trim(), branch: b } : { kind: 'git', url: f.url.trim() };
  }
  return null;
}

/** 工作区选择行：留空 = 使用默认工作区 ~/.domo/workspace/<项目id>/ */
function WorkspaceFields({
  f,
  set,
}: {
  f: WsFields;
  set: (patch: Partial<WsFields>) => void;
}) {
  return (
    <div className="ws-grid">
      <label className="ws-kind">
        工作区（必填）
        <select value={f.kind === '' ? 'local' : f.kind} onChange={e => set({ kind: e.target.value as WsKind })}>
          <option value="local">本地目录（local）</option>
          <option value="git">Git 仓库（git）</option>
        </select>
      </label>
      {f.kind === 'local' && (
        <label>
          绝对路径
          <input
            value={f.path}
            onChange={e => set({ path: e.target.value })}
            placeholder="/home/... 已存在的目录"
          />
        </label>
      )}
      {f.kind === 'git' && (
        <>
          <label>
            仓库 URL
            <input
              value={f.url}
              onChange={e => set({ url: e.target.value })}
              placeholder="https://... 或 ~/repos/xxx（file:// 支持本地仓库）"
            />
          </label>
          <label>
            分支（可选）
            <input
              value={f.branch}
              onChange={e => set({ branch: e.target.value })}
              placeholder="如 main / dev"
            />
          </label>
        </>
      )}
    </div>
  );
}

/** 徽章短文案：local 用路径末段，git 用主机名 */
function workspaceBrief(ws: Workspace): string {
  if (ws.kind === 'local') {
    const segs = (ws.path ?? '').split('/').filter(Boolean);
    return segs.length > 0 ? `⛁ ${segs[segs.length - 1]}` : '⛁ local';
  }
  try {
    return `⛁ ${new URL(ws.url ?? '').hostname}`;
  } catch {
    return '⛁ git';
  }
}

function WorkspaceBadge({ ws }: { ws: Workspace }) {
  return (
    <span title={ws.kind === 'local' ? ws.path : `${ws.url}${ws.branch ? ` @ ${ws.branch}` : ''}`}>
      <Badge tone={ws.kind === 'local' ? 'blue' : 'purple'}>{workspaceBrief(ws)}</Badge>
    </span>
  );
}

/** 项目基线徽章：多仓展示「仓数 → 分支」，单仓展示分支或提交 */
function BaselineBadge({ baseline }: { baseline?: ProjectBaseline | null }) {
  if (!baseline) return null;
  const repos = Object.entries(baseline.repos ?? {});
  if (repos.length > 0) {
    const branch = repos[0][1];
    return (
      <span title={repos.map(([rel, b]) => `${rel} → ${b}`).join('\n')}>
        <Badge tone="green">
          基线：{repos.length} 仓 · {branch}
        </Badge>
      </span>
    );
  }
  if (baseline.branch) {
    return <Badge tone="green">基线：{baseline.branch}</Badge>;
  }
  if (baseline.commit) {
    return (
      <span title={baseline.commit}>
        <Badge tone="green">基线：{baseline.commit.slice(0, 8)}</Badge>
      </span>
    );
  }
  return null;
}

/** 找到某里程碑当前 open 的 gate 收件箱条目（用于「去确认」一键跳转） */
function gateItemFor(inbox: InboxItem[], milestoneId: string): InboxItem | undefined {
  return inbox.find(
    i =>
      i.type === 'milestone_gate' &&
      i.status === 'open' &&
      (i.payload as { milestone_id?: string } | null)?.milestone_id === milestoneId,
  );
}

export function ProjectsView() {
  const {
    projects,
    engineerings,
    tasksByProject,
    milestonesByProject,
    inbox,
    reports,
    createProject,
    updateProject,
    projectAction,
    selectProject,
    focusInbox,
    loadReport,
    setError,
    planning: planningState,
  } = useDomo();

  const [name, setName] = useState('');
  const [brief, setBrief] = useState('');
  // 所属工程（默认选第一个；留空 = 未归属，用于兼容旧数据）
  const [engineeringId, setEngineeringId] = useState('');
  useEffect(() => {
    if (!engineeringId && engineerings.length > 0) setEngineeringId(engineerings[0].id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [engineerings.length]);
  const [busy, setBusy] = useState(false);
  const [confirmCancel, setConfirmCancel] = useState<Project | null>(null);
  // 状态复选框已含归档开关（旧变量保留避免误引用）
  // M3 项目总结报告查看弹窗
  const [reportFor, setReportFor] = useState<Project | null>(null);

  // 规划长等待（状态在全局 store，跨视图切换保持）；elapsed 由 startedAt 计算
  const planningId = planningState?.projectId ?? null;
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!planningState) return;
    setTick(0);
    const t = setInterval(() => setTick(x => x + 1), 1000);
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [planningState?.projectId]);
  const elapsed = planningState ? Math.floor((Date.now() - planningState.startedAt) / 1000) : 0;

  // 项目编辑弹窗（非归档任意状态可编辑：名称/目标/工作区）
  const [editWs, setEditWs] = useState<{
    project: Project;
    name: string;
    brief: string;
    f: WsFields;
  } | null>(null);

  async function submitCreate() {
    if (!name.trim()) return;
    // 工作区属于工程：项目不再单独指定
    if (!brief.trim()) {
      setError('必须填写项目目标。');
      return;
    }
    setBusy(true);
    const ok = await createProject(name.trim(), brief.trim(), undefined, engineeringId || null);
    setBusy(false);
    if (ok) {
      setName('');
      setBrief('');
    }
  }

  // 状态过滤：复选框自由组合；默认不勾「已完成」「已归档」（避免刷屏），其余全选
  const ALL_STATUS_FILTERS: { key: string; label: string }[] = [
    { key: 'initializing', label: '初始化' },
    { key: 'running', label: '运行中' },
    { key: 'pausing', label: '暂停中' },
    { key: 'paused', label: '已暂停' },
    { key: 'completed', label: '已完成' },
    { key: 'archived', label: '已归档' },
  ];
  const [statusChecked, setStatusChecked] = useState<Set<string>>(
    () => new Set(['initializing', 'running', 'paused'])
  );
  const toggleStatus = (k: string) =>
    setStatusChecked(prev => {
      const next = new Set(prev);
      if (next.has(k)) next.delete(k);
      else next.add(k);
      return next;
    });
  const visibleProjects = projects.filter(p => statusChecked.has(p.status));
  const statusCount = (k: string) => projects.filter(p => p.status === k).length;

  return (
    <div className="stack">
      <section className="panel">
        <h2 className="panel-title">创建项目</h2>
        <form
          className="inline-form"
          onSubmit={e => {
            e.preventDefault();
            void submitCreate();
          }}
        >
          <input
            value={name}
            onChange={e => setName(e.target.value)}
            placeholder="项目名称"
            required
          />
          <input
            value={brief}
            onChange={e => setBrief(e.target.value)}
            placeholder="项目目标（必填：交给团队的完整说明）"
            required
          />
          <label className="ws-kind">
            所属工程
            <select value={engineeringId} onChange={e => setEngineeringId(e.target.value)}>
              <option value="">未归属</option>
              {engineerings.map(e => (
                <option key={e.id} value={e.id}>
                  {e.name}
                </option>
              ))}
            </select>
          </label>
          <button
            className="btn btn-primary"
            type="submit"
            disabled={busy || !name.trim() || !brief.trim()}
          >
            {busy ? '创建中…' : '创建项目'}
          </button>
        </form>
      </section>

      {projects.length === 0 ? (
        <Empty text="还没有项目，先创建一个吧。" />
      ) : (
        <>
      <div className="toolbar" style={{ flexWrap: 'wrap', alignItems: 'center', gap: 14 }}>
        <span className="muted">显示状态（可组合）：</span>
        {ALL_STATUS_FILTERS.map(f => (
          <label key={f.key} className="muted" style={{ cursor: 'pointer' }}>
            <input
              type="checkbox"
              checked={statusChecked.has(f.key)}
              onChange={() => toggleStatus(f.key)}
            />{' '}
            {f.label}（{statusCount(f.key)}）
          </label>
        ))}
        <span className="spacer" />
        <span className="muted">当前显示 {visibleProjects.length} / {projects.length} 个项目</span>
      </div>
          
<div className="card-grid">
          {visibleProjects.map(p => {
            const tasks = tasksByProject[p.id] ?? [];
            const milestones = milestonesByProject[p.id] ?? [];
            const done = tasks.filter(t => t.status === 'done').length;
            const total = tasks.length;
            const pct = total > 0 ? Math.round((done / total) * 100) : 0;
            const projInbox = inbox.filter(i => i.project_id === p.id && i.status === 'open').length;
            const open = OPEN_STATUSES.includes(p.status);
            const planning = planningId === p.id;
            const engKind = p.engineering_id
              ? engineerings.find(e => e.id === p.engineering_id)?.workspace_kind
              : undefined;
            return (
              <div key={p.id} className="card project-card">
                <div className="card-head">
                  <button className="link-title" onClick={() => selectProject(p.id)} title={p.id}>
                    {p.name}
                  </button>
                  <StatusBadge kind="project" value={p.status} />
                </div>
                <p className="card-brief">{p.brief || '（未设置目标）'}</p>
                <div className="badge-row">
                  {p.engineering_id && (
                    <Badge tone="purple">
                      工程：{engineerings.find(e => e.id === p.engineering_id)?.name ?? p.engineering_id}
                    </Badge>
                  )}
                  {p.workspace ? (
                    <WorkspaceBadge ws={p.workspace} />
                  ) : (
                    <span className="ws-hint">将使用默认工作区 ~/.domo/workspace/{p.id}（确认规划时自动创建）</span>
                  )}
                  <BaselineBadge baseline={p.baseline} />
                  {open && (
                    <button
                      className="btn"
                      title="编辑名称 / 项目目标 / 工作区（任何时候，除已归档）"
                      onClick={() =>
                        setEditWs({
                          project: p,
                          name: p.name,
                          brief: p.brief,
                          f: {
                            kind: p.workspace?.kind ?? 'local',
                            path: p.workspace?.path ?? '',
                            url: p.workspace?.url ?? '',
                            branch: p.workspace?.branch ?? '',
                          },
                        })
                      }
                    >
                      ✎ 编辑
                    </button>
                  )}
                </div>
                <div className="progress-row">
                  <div className="progress">
                    <div className="progress-fill" style={{ width: `${pct}%` }} />
                  </div>
                  <span className="progress-text">
                    {total > 0 ? `${done}/${total} 任务` : '暂无任务'}（{pct}%）
                  </span>
                </div>
                {planning && (
                  <div className="risk-line">
                    <span className="spinner" /> 规划中…已等待 {elapsed}s · LLM 规划可能需要几分钟
                  </div>
                )}
                {projInbox > 0 && <div className="risk-line">⚠ {projInbox} 项待决策</div>}
                {milestones.length > 0 && (
                  <div className="milestone-list">
                    {milestones.map(m => {
                      const mtasks = tasks.filter(t => t.milestone_id === m.id);
                      const mdone = mtasks.filter(t => t.status === 'done').length;
                      const mtotal = mtasks.length;
                      const mpct =
                        mtotal > 0
                          ? Math.round((mdone / mtotal) * 100)
                          : m.status === 'passed'
                            ? 100
                            : 0;
                      const meta = MILESTONE_STATUS_LABEL[m.status] ?? { label: m.status, tone: 'gray' };
                      const gate =
                        m.status === 'awaiting_confirm' ? gateItemFor(inbox, m.id) : undefined;
                      return (
                        <div key={m.id} className={`milestone-row ms-${m.status}`}>
                          <div className="milestone-row-head">
                            <span className="milestone-name" title={m.goal ?? m.name}>
                              {m.name}
                            </span>
                            <Badge tone={meta.tone}>{meta.label}</Badge>
                          </div>
                          <div className="progress-row">
                            <div className="progress">
                              <div
                                className={`progress-fill fill-${m.status}`}
                                style={{ width: `${mpct}%` }}
                              />
                            </div>
                            <span className="progress-text">{mdone}/{mtotal}</span>
                          </div>
                          {gate ? (
                            <button
                              className="btn btn-sm btn-primary milestone-gate-btn"
                              title="跳转到决策收件箱的里程碑确认项"
                              onClick={() => focusInbox(gate.id)}
                            >
                              待确认 · 去收件箱 →
                            </button>
                          ) : m.goal ? (
                            <span className="ws-hint milestone-goal">{m.goal}</span>
                          ) : null}
                        </div>
                      );
                    })}
                  </div>
                )}
                <div className="card-foot">
                  <span className="muted">{fmtTime(p.created_at)}</span>
                  <div className="btn-row">
                    {(p.status === 'completed' || p.status === 'archived' || reports[p.id]) && (
                      <button
                        className="btn"
                        title="查看项目总结报告（里程碑汇报 / 任务统计 / 事件摘要 / 产物清单）"
                        onClick={() => {
                          setReportFor(p);
                          void loadReport(p.id);
                        }}
                      >
                        📄 报告
                      </button>
                    )}
                    {(p.status === 'initializing' || p.status === 'pending_confirm') && (
                      <button
                        className="btn"
                        title="生成规划 / 确认执行已移至任务视图的「规划台」"
                        onClick={() => selectProject(p.id)}
                      >
                        去规划台 →
                      </button>
                    )}
                    {p.status === 'running' && (
                      <button className="btn" onClick={() => void projectAction(p.id, 'pause')}>
                        暂停
                      </button>
                    )}
                    {p.status === 'pausing' && (
                      <>
                        <button className="btn" title="暂停请求已受理，在途会话结束后自动转为已暂停；点击立即恢复运行" onClick={() => void projectAction(p.id, 'resume')}>
                          取消暂停（恢复运行）
                        </button>
                        <button
                          className="btn btn-danger-ghost"
                          title="立即终止全部在途会话并转到已暂停（当前尝试的进度会丢失）"
                          onClick={() => {
                            if (window.confirm('强制暂停将立即终止本项目全部在途 agent 会话，当前尝试的进度会丢失；其他项目不受影响。确定？')) {
                              void projectAction(p.id, 'pause', true);
                            }
                          }}
                        >
                          ⚡ 强制暂停
                        </button>
                      </>
                    )}
                    {p.status === 'paused' && (
                      <button
                        className="btn"
                        title={
                          engKind === 'multi'
                            ? '切换到本项目分支并恢复；工作区有未提交改动时会阻止并列出仓库与文件'
                            : '恢复运行'
                        }
                        onClick={() => void projectAction(p.id, 'resume')}
                      >
                        恢复
                      </button>
                    )}
                    {open && (
                      <>
                        <button className="btn btn-danger-ghost" onClick={() => setConfirmCancel(p)}>
                          归档
                        </button>
                      </>
                    )}
                  </div>
                </div>
              </div>
            );
          })}
          </div>
        </>
      )}

      {confirmCancel && (
        <ConfirmDialog
          title="归档项目"
          message={`确定将项目「${confirmCancel.name}」归档吗？在途会话将停止，非终态任务转为已取消，项目转为只读存档（成果与记录全部保留）。`}
          confirmText="确认归档"
          onCancel={() => setConfirmCancel(null)}
          onConfirm={() => {
            void projectAction(confirmCancel.id, 'cancel');
            setConfirmCancel(null);
          }}
        />
      )}

      {reportFor && (
        <ReportModal
          projectId={reportFor.id}
          projectName={reportFor.name}
          onClose={() => setReportFor(null)}
        />
      )}

      {editWs && (
        <Modal title={`编辑项目 · ${editWs.project.name}`} onClose={() => setEditWs(null)}>
          <div className="stack">
            <label>
              项目名称
              <input
                value={editWs.name}
                onChange={e => setEditWs(prev => (prev ? { ...prev, name: e.target.value } : prev))}
              />
            </label>
            <label>
              项目目标
              <input
                value={editWs.brief}
                onChange={e => setEditWs(prev => (prev ? { ...prev, brief: e.target.value } : prev))}
              />
            </label>
            <WorkspaceFields
              f={editWs.f}
              set={patch => setEditWs(prev => (prev ? { ...prev, f: { ...prev.f, ...patch } } : prev))}
            />
          </div>
          <div className="row-end">
            <button className="btn" onClick={() => setEditWs(null)}>
              取消
            </button>
            <button
              className="btn btn-primary"
              onClick={() => {
                void (async () => {
                  if (!editWs.brief.trim() || fieldsToWorkspace(editWs.f) === null) {
                    setError('项目目标与工作区为必填项。');
                    return;
                  }
                  const ok = await updateProject(editWs.project.id, {
                    name: editWs.name.trim(),
                    brief: editWs.brief.trim(),
                    workspace: fieldsToWorkspace(editWs.f),
                  });
                  if (ok) setEditWs(null);
                })();
              }}
            >
              保存
            </button>
          </div>
        </Modal>
      )}
    </div>
  );
}
