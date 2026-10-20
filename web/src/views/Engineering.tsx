import { useState } from 'react';
import { useDomo } from '../store';
import type { WorkspaceKind } from '../types';
import { Badge, Empty, StatusBadge } from '../ui';

const KIND_LABEL: Record<WorkspaceKind, string> = {
  single: 'Git 仓库',
  multi: '共享目录',
  plain: '共享目录',
};

export function EngineeringView() {
  const { engineerings, projects, createEngineering, selectProject, setView } = useDomo();

  const [name, setName] = useState('');
  const [brief, setBrief] = useState('');
  const [path, setPath] = useState('');
  const [constraints, setConstraints] = useState('');
  const [busy, setBusy] = useState(false);

  async function submit() {
    if (!name.trim()) return;
    setBusy(true);
    const e = await createEngineering({
      name: name.trim(),
      brief: brief.trim(),
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
          <input
            value={path}
            onChange={e => setPath(e.target.value)}
            placeholder="工作区绝对路径（git 仓库根 / 共享目录；留空用默认工作区）"
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
          类型自动识别：锚点是 git 仓库根 → 任务用 worktree 隔离、可并行；其余目录 → 共享目录，项目与任务串行，不创建/切换项目分支。
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
                  <Badge tone={e.workspace_kind === 'single' ? 'green' : 'blue'}>
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
