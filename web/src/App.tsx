import { Component, useState, type ReactNode } from 'react';
import { AuditView } from './views/Audit';
import { EngineeringView } from './views/Engineering';
import { InboxView } from './views/Inbox';
import { KnowledgeView } from './views/Knowledge';
import { OrgView } from './views/Org';
import { ProjectsView } from './views/Projects';
import { TaskFormModal as TasksFormModal, TasksView, type TaskFormState } from './views/Tasks';
import { DomoProvider, useDomo, type ViewId } from './store';

const VIEW_TITLES: Record<ViewId, string> = {
  engineering: '工程总览',
  projects: '项目总览',
  tasks: '任务视图',
  org: '组织视图',
  inbox: '收件箱',
  audit: '审计日志',
  knowledge: '项目知识库',
};

function Sidebar() {
  const {
    view, setView, inbox, selectedProjectId,
    engineerings, projects, selectedEngineeringId, selectEngineering, selectProject,
  } = useDomo();
  const openCount = inbox.filter(i => i.status === 'open').length;
  const items: { id: ViewId; label: string; badge?: number; dot?: boolean }[] = [
    { id: 'engineering', label: '工程总览' },
    { id: 'projects', label: '项目总览' },
    { id: 'tasks', label: '任务视图', dot: !!selectedProjectId },
    { id: 'org', label: '组织视图' },
    { id: 'inbox', label: '收件箱', badge: openCount },
    { id: 'knowledge', label: '知识库' },
    { id: 'audit', label: '审计' },
  ];
  return (
    <aside className="sidebar">
      <div className="brand">
        <span className="brand-mark">D</span>
        <div>
          <div className="brand-name">DOMO 工作台</div>
          <div className="brand-sub">AI Agent 协作团队</div>
        </div>
      </div>
      <nav className="nav">
        {items.map(it => (
          <button
            key={it.id}
            className={`nav-item ${view === it.id ? 'active' : ''}`}
            onClick={() => setView(it.id)}
          >
            <span>{it.label}</span>
            {it.badge ? <span className="nav-badge">{it.badge}</span> : null}
          </button>
        ))}
      </nav>
      <div className="scope-nav">
        <div className="scope-title">工程 / 项目</div>
        {engineerings.length === 0 && <div className="muted scope-empty">还没有工程</div>}
        {engineerings.map(e => {
          const mine = projects.filter(p => p.engineering_id === e.id);
          const open = selectedEngineeringId === e.id;
          return (
            <div key={e.id} className="scope-group">
              <button
                className={`scope-eng ${open ? 'active' : ''}`}
                title={e.workspace_path ?? e.name}
                onClick={() => selectEngineering(e.id)}
              >
                <span>{e.name}</span>
                <span className="muted">{mine.length}</span>
              </button>
              {open && mine.map(p => (
                <button
                  key={p.id}
                  className={`scope-project ${selectedProjectId === p.id ? 'active' : ''}`}
                  onClick={() => selectProject(p.id)}
                  title={p.brief}
                >
                  {p.name}
                </button>
              ))}
            </div>
          );
        })}
      </div>
      <ConnStatus />
    </aside>
  );
}

function ConnStatus() {
  const { conn } = useDomo();
  const meta = {
    open: { cls: 'ok', text: '已连接' },
    connecting: { cls: 'wait', text: '连接中…' },
    closed: { cls: 'bad', text: '已断线，重连中…' },
  }[conn];
  return (
    <div className={`conn ${meta.cls}`}>
      <span className="dot" />
      {meta.text}
    </div>
  );
}

function Toast() {
  const { error, setError } = useDomo();
  // 错误不自动消失（尤其规划失败详情需要时间阅读），仅手动关闭
  if (!error) return null;
  return (
    <div className="toast" role="alert">
      <span>{error}</span>
      <button className="icon-btn" onClick={() => setError(null)} aria-label="关闭">
        ✕
      </button>
    </div>
  );
}

class ViewErrorBoundary extends Component<{ children: ReactNode }, { err: Error | null }> {
  state = { err: null as Error | null };
  static getDerivedStateFromError(err: Error) { return { err }; }
  render() {
    if (this.state.err) {
      return (
        <div className="panel">
          <h2 className="panel-title">视图渲染出错</h2>
          <p style={{ whiteSpace: 'pre-wrap' }}>{String(this.state.err.message || this.state.err)}</p>
          <button className="btn" onClick={() => this.setState({ err: null })}>重试</button>
        </div>
      );
    }
    return this.props.children;
  }
}

function Breadcrumbs() {
  const {
    engineerings, projects, taskDetail, selectedEngineeringId, selectedProjectId,
    setView, selectEngineering, selectProject,
  } = useDomo();
  const eng = engineerings.find(e => e.id === selectedEngineeringId);
  const proj = projects.find(p => p.id === selectedProjectId);
  return (
    <nav className="breadcrumbs" aria-label="层级导航">
      <button className="crumb-link" onClick={() => setView('engineering')}>工程</button>
      {eng && (
        <>
          <span className="crumb-sep">/</span>
          <button className="crumb-link" onClick={() => selectEngineering(eng.id)}>{eng.name}</button>
        </>
      )}
      {proj && (
        <>
          <span className="crumb-sep">/</span>
          <button className="crumb-link" onClick={() => selectProject(proj.id)}>{proj.name}</button>
        </>
      )}
      {proj && taskDetail && (
        <>
          <span className="crumb-sep">/</span>
          <span className="crumb-current">{taskDetail.title}</span>
        </>
      )}
    </nav>
  );
}

function Shell() {
  const { view } = useDomo();
  const [taskForm, setTaskForm] = useState<TaskFormState | null>(null);

  return (
    <div className="app">
      <Sidebar />
      <main className="main">
        <header className="topbar">
          <Breadcrumbs />
          <span className="topbar-view">{VIEW_TITLES[view]}</span>
        </header>
        <div className="content">
          <ViewErrorBoundary>
            {view === 'engineering' && <EngineeringView />}
            {view === 'projects' && <ProjectsView />}
            {view === 'tasks' && <TasksView onOpenForm={setTaskForm} />}
            {view === 'org' && <OrgView />}
            {view === 'inbox' && <InboxView />}
            {view === 'knowledge' && <KnowledgeView />}
            {view === 'audit' && <AuditView />}
          </ViewErrorBoundary>
        </div>
      </main>
      <Toast />
      {taskForm && (
        <TaskFormHost form={taskForm} onClose={() => setTaskForm(null)} />
      )}
    </div>
  );
}

function TaskFormHost({ form, onClose }: { form: TaskFormState; onClose: () => void }) {
  const { selectedProjectId, selectedTaskId, taskDetail } = useDomo();
  const projectId =
    form.mode === 'create' ? selectedProjectId : form.task?.project_id ?? selectedProjectId;
  if (!projectId) return null;
  // 编辑时以最新详情为准
  const task =
    form.mode === 'edit'
      ? taskDetail && taskDetail.id === form.task.id && selectedTaskId === form.task.id
        ? taskDetail
        : form.task
      : undefined;
  return <TasksFormModal projectId={projectId} mode={form.mode} task={task} onClose={onClose} />;
}

export default function App() {
  return (
    <DomoProvider>
      <Shell />
    </DomoProvider>
  );
}
