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
  const { view, setView, inbox, selectedProjectId } = useDomo();
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

function Shell() {
  const { view, taskDetail } = useDomo();
  const [taskForm, setTaskForm] = useState<TaskFormState | null>(null);

  return (
    <div className="app">
      <Sidebar />
      <main className="main">
        <header className="topbar">
          <h1>{VIEW_TITLES[view]}</h1>
          {view === 'tasks' && taskDetail && <span className="muted crumb">/ 任务详情</span>}
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
