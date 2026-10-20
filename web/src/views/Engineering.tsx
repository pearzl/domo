import { useState } from 'react';
import { useDomo } from '../store';
import type { WorkspaceKind } from '../types';
import { Badge, Empty, StatusBadge } from '../ui';

const KIND_LABEL: Record<WorkspaceKind, string> = {
  single: '单仓',
  multi: '多仓',
  plain: '普通目录',
};

const KIND_HINT: Record<WorkspaceKind, string> = {
  single: '一个 git 仓库；任务用 worktree 隔离，可并行',
  multi: '多个仓库的父目录；项目与任务串行，按项目分支切换',
  plain: '非 git 目录；任务可并行，但没有版本控制保护',
};

export function EngineeringView() {
  const { engineerings, projects, createEngineering, selectProject, setView, setError } = useDomo();

  const [name, setName] = useState('');
  const [brief, setBrief] = useState('');
  const [kind, setKind] = useState<WorkspaceKind>('single');
  const [path, setPath] = useState('');
  const [constraints, setConstraints] = useState('');
  const [busy, setBusy] = useState(false);

  async function submit() {
    if (!name.trim()) return;
    if (kind !== 'plain' && !path.trim()) {
      setError('单仓 / 多仓工程必须填写已存在的工作区目录。');
      return;
    }
    setBusy(true);
    const e = await createEngineering({
      name: name.trim(),
      brief: brief.trim(),
      workspace_kind: kind,
      workspace_path: path.trim() || null,
      constraints: constraints.trim() || null,
    });
    setBusy(false);
    if (e) {
      setName('');
      setBrief('');
      setPath('');
      setConstraints('');
    }
  }

  return (
    <div className="stack">
      <section className="panel">
        <h2 className="panel-title">创建工程</h2>
        <p className="muted" style={{ marginTop: 0 }}>
          工程是完整产品：只承载“在哪儿做、用谁做、不能怎么做”，不含目标与进度。迭代放到项目里。
        </p>
        <form
          className="inline-form"
          onSubmit={e => {
            e.preventDefault();
            void submit();
          }}
        >
          <input value={name} onChange={e => setName(e.target.value)} placeholder="工程名称" required />
          <input
            value={brief}
            onChange={e => setBrief(e.target.value)}
            placeholder="简介（这是什么产品）"
          />
          <label className="ws-kind">
            工作区类型
            <select value={kind} onChange={e => setKind(e.target.value as WorkspaceKind)}>
              <option value="single">单仓（git 仓库）</option>
              <option value="multi">多仓（父目录）</option>
              <option value="plain">普通目录</option>
            </select>
          </label>
          <input
            value={path}
            onChange={e => setPath(e.target.value)}
            placeholder={
              kind === 'multi'
                ? '多仓父目录绝对路径（已存在）'
                : kind === 'single'
                  ? '仓库绝对路径（已存在）'
                  : '目录绝对路径（可选）'
            }
            required={kind !== 'plain'}
          />
          <input
            value={constraints}
            onChange={e => setConstraints(e.target.value)}
            placeholder="防跑偏约束（技术栈 / 目录约定 / 禁改范围，可选）"
          />
          <button className="btn btn-primary" type="submit" disabled={busy || !name.trim()}>
            {busy ? '创建中…' : '创建工程'}
          </button>
        </form>
        <p className="muted" style={{ marginBottom: 0 }}>
          {KIND_HINT[kind]}
        </p>
      </section>

      {engineerings.length === 0 ? (
        <Empty text="还没有工程。先创建一个，再在它下面建项目。" />
      ) : (
        <div className="card-grid">
          {engineerings.map(e => {
            const mine = projects.filter(p => p.engineering_id === e.id);
            const running = mine.filter(p => p.status === 'running').length;
            const open = mine.filter(p =>
              ['initializing', 'pending_confirm', 'running', 'pausing', 'paused'].includes(p.status),
            ).length;
            return (
              <section key={e.id} className="panel">
                <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'baseline', gap: 8 }}>
                  <h3 className="panel-title" style={{ margin: 0 }}>{e.name}</h3>
                  <Badge tone={e.workspace_kind === 'multi' ? 'purple' : e.workspace_kind === 'plain' ? 'blue' : 'green'}>
                    {KIND_LABEL[e.workspace_kind]}
                  </Badge>
                </div>
                {e.brief && <p className="muted">{e.brief}</p>}
                <p className="muted" style={{ margin: '4px 0' }}>
                  工作区：<code>{e.workspace_path ?? '（未设置）'}</code>
                </p>
                {e.constraints && (
                  <p className="muted" style={{ margin: '4px 0' }}>
                    防跑偏约束：{e.constraints}
                  </p>
                )}
                <p className="muted">
                  项目 {mine.length} 个（进行中 {open}，运行中 {running}）
                </p>
                {mine.length === 0 ? (
                  <p className="muted">还没有项目。到「项目总览」创建，并选择本工程。</p>
                ) : (
                  <ul style={{ margin: '6px 0 0', paddingLeft: 18 }}>
                    {mine.map(p => (
                      <li key={p.id}>
                        <button
                          className="btn btn-sm"
                          onClick={() => {
                            selectProject(p.id);
                            setView('projects');
                          }}
                        >
                          {p.name}
                        </button>{' '}
                        <StatusBadge kind="project" value={p.status} />
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            );
          })}
        </div>
      )}
    </div>
  );
}
