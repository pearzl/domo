import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import { useDomo } from './store';

export const PROJECT_STATUS_LABEL: Record<string, { label: string; tone: string }> = {
  initializing: { label: '初始化中', tone: 'gray' },
  pending_confirm: { label: '待确认', tone: 'purple' },
  running: { label: '运行中', tone: 'green' },
  pausing: { label: '暂停中', tone: 'amber' },
  paused: { label: '已暂停', tone: 'amber' },
  completed: { label: '已完成', tone: 'blue' },
  archived: { label: '已归档', tone: 'gray' },
};

/** 项目状态悬停说明 */
export const PROJECT_STATUS_DESC: Record<string, string> = {
  initializing: '规划阶段：规划者可生成与调整任务，确认后进入运行',
  pending_confirm: '规划已生成，等待你确认后开始执行',
  running: '调度器自动派发：计划 → 执行 → 验收',
  pausing: '暂停请求已受理，在途会话跑完当前尝试后自动转为已暂停（不派发新工作）',
  paused: '全部会话已停止；可恢复继续，人工裁决不受暂停限制',
  completed: '全部任务达到终态，已产出项目总结报告',
  archived: '已归档（只读）：成果/日志/事件全量保留',
};

export const TASK_STATUS_LABEL: Record<string, { label: string; tone: string }> = {
  planned: { label: '计划中', tone: 'gray' },
  ready: { label: '待执行', tone: 'blue' },
  plan_drafting: { label: '制定计划中', tone: 'blue' },
  plan_review: { label: '计划审查中', tone: 'purple' },
  executing: { label: '执行中', tone: 'amber' },
  recovering: { label: '等待恢复', tone: 'orange' },
  awaiting_acceptance: { label: '待验收', tone: 'purple' },
  escalated: { label: '已升级', tone: 'red' },
  suspended: { label: '已挂起', tone: 'orange' },
  done: { label: '已完成', tone: 'green' },
  cancelled: { label: '已取消', tone: 'gray' },
};

/** 状态悬停说明（中文语义，而非英文名） */
export const TASK_STATUS_DESC: Record<string, string> = {
  planned: '已创建，等待前置依赖满足后进入计划制定',
  ready: '前置依赖已就绪，等待执行者制定任务计划',
  plan_drafting: '执行者正在制定任务计划（步骤/产物/验证方式）',
  plan_review: '验收者正在审查任务计划，通过后执行者开始按计划执行',
  executing: '执行者 agent 正在处理本任务（会话进行中）',
  recovering: '上一轮会话被中断（如服务重启），等待三档恢复续跑；agent 忙时在此排队',
  awaiting_acceptance: '执行已完成，等待验收者（或人工）审核产出',
  escalated: '验收多次退回，已升级到收件箱等待人裁决',
  suspended: '多次执行失败已挂起；到收件箱处理后可恢复重派',
  done: '执行与验收均通过，任务完成',
  cancelled: '已取消（人工放弃或随项目归档），不可再操作',
};

export const ROLE_LABEL: Record<string, string> = {
  planner: '规划者',
  executor: '执行者',
  acceptor: '验收者',
};

export const INBOX_TYPE_LABEL: Record<string, string> = {
  escalation: '执行升级',
  replan: '重规划',
  milestone_gate: '里程碑确认',
};

export const MILESTONE_STATUS_LABEL: Record<string, { label: string; tone: string }> = {
  pending: { label: '进行中', tone: 'gray' },
  awaiting_confirm: { label: '待确认', tone: 'amber' },
  passed: { label: '已通过', tone: 'green' },
  failed: { label: '已退回', tone: 'red' },
};

export const RESOLUTION_LABEL: Record<string, string> = {
  resume: '恢复重派',
  reassign: '更换执行者',
  reassign_acceptor: '更换验收者',
  relax: '放宽标准',
  abandon: '放弃任务',
  pass: '确认通过',
  passed: '里程碑通过',
  reject: '退回',
  failed: '里程碑退回',
  approve: '批准',
  deny: '否决',
  applied: '窗口方案已应用',
  close: '已确认处理',
};

export function fmtTime(ts: number | undefined | null): string {
  if (!ts) return '—';
  const ms = ts < 1e12 ? ts * 1000 : ts;
  // 全站统一东八区（本地时区可能不同，如服务器 UTC）
  const d = new Intl.DateTimeFormat('zh-CN', {
    timeZone: 'Asia/Shanghai',
    year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit', second: '2-digit',
    hour12: false,
  }).format(new Date(ms));
  return `${d.replace(/\//g, '-')} (+08)`;
}

export function shortId(id: string | null | undefined): string {
  if (!id) return '—';
  const i = id.lastIndexOf('_');
  return i >= 0 ? id.slice(i + 1) : id;
}

export function Badge({ tone = 'gray', children }: { tone?: string; children: ReactNode }) {
  return <span className={`badge badge-${tone}`}>{children}</span>;
}

export function StatusBadge({ kind, value }: { kind: 'project' | 'task'; value: string }) {
  const map = kind === 'project' ? PROJECT_STATUS_LABEL : TASK_STATUS_LABEL;
  const meta = map[value] ?? { label: value, tone: 'gray' };
  // 悬停显示中文状态说明（而非英文名）
  const desc = kind === 'task' ? TASK_STATUS_DESC[value] : PROJECT_STATUS_DESC[value];
  return (
    <span title={desc ?? meta.label}>
      <Badge tone={meta.tone}>{meta.label}</Badge>
    </span>
  );
}

/** 全局任务跳转链接：点击 → 切项目 + 打开任务抽屉 + 进入任务视图 */
export function TaskLink({ taskId, projectId, children }: { taskId: string; projectId: string; children?: ReactNode }) {
  const { selectProject, openTask, setView } = useDomo();
  return (
    <button
      className="task-link"
      title={`查看任务 ${taskId}`}
      onClick={() => {
        selectProject(projectId);
        void openTask(taskId);
        setView('tasks');
      }}
    >
      {children ?? `#${shortId(taskId)}`}
    </button>
  );
}

/** agent 跳转链接：点击 → 组织视图并高亮该实例；已删除的实例展示快照信息 */
export function AgentLink({
  agentId,
  snapshotName,
  children,
}: {
  agentId: string;
  /** agent 已不存在（软删除前无记录）时使用的快照展示名 */
  snapshotName?: string;
  children?: ReactNode;
}) {
  const { agents, setView, setFocusAgentId } = useDomo();
  const a = agents.find(x => x.id === agentId);
  const label = children ?? a?.name ?? snapshotName ?? `#${shortId(agentId)}`;
  const tip = a
    ? `${a.name}（${a.adapter} · ${a.roles.join('/')}）— 点击查看`
    : `该 agent 已删除或不存在（快照：${snapshotName ?? agentId}）`;
  return (
    <button
      className="task-link"
      title={tip}
      onClick={e => {
        e.stopPropagation();
        setFocusAgentId(agentId);
        setView('org');
      }}
    >
      {label}
    </button>
  );
}

export function Empty({ text }: { text: string }) {
  return <div className="empty">{text}</div>;
}

export function Modal({
  title,
  onClose,
  children,
  wide,
  className,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  wide?: boolean;
  className?: string;
}) {
  return (
    <div className="overlay" onClick={onClose}>
      <div
        className={`modal ${modalClass(wide, className)}`}
        onClick={e => e.stopPropagation()}
      >
        <div className="modal-head">
          <h3>{title}</h3>
          <button className="icon-btn" onClick={onClose} aria-label="关闭">
            ✕
          </button>
        </div>
        <div className="modal-body">{children}</div>
      </div>
    </div>
  );
}

function modalClass(wide?: boolean, className?: string): string {
  return [wide ? 'modal-wide' : '', className ?? ''].filter(Boolean).join(' ');
}

export function ConfirmDialog({
  title,
  message,
  confirmText,
  onConfirm,
  onCancel,
}: {
  title: string;
  message: string;
  confirmText: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const { setError } = useDomo();
  return (
    <Modal title={title} onClose={onCancel}>
      <p className="confirm-msg">{message}</p>
      <div className="row-end">
        <button className="btn" onClick={onCancel}>
          取消
        </button>
        <button
          className="btn btn-danger"
          onClick={() => {
            setError(null);
            onConfirm();
          }}
        >
          {confirmText}
        </button>
      </div>
    </Modal>
  );
}

/** 通用翻页条（单页长度全站 15；上一页/下一页 + 页码状态） */
export function Pager({
  page,
  totalPages,
  total,
  onPage,
  label = '条',
}: {
  page: number;
  totalPages: number;
  total: number;
  onPage: (p: number) => void;
  label?: string;
}) {
  if (totalPages <= 1) return <span className="muted">共 {total} {label}</span>;
  return (
    <div className="row-end" style={{ paddingTop: 8, gap: 10 }}>
      <span className="muted">
        共 {total} {label} · 第 {page + 1}/{totalPages} 页
      </span>
      <button className="btn btn-sm" disabled={page === 0} onClick={() => onPage(page - 1)}>
        ← 上一页
      </button>
      <button className="btn btn-sm" disabled={page + 1 >= totalPages} onClick={() => onPage(page + 1)}>
        下一页 →
      </button>
    </div>
  );
}

/** 长等待计时（秒）：以 startedAt 为基准每秒重渲染一次；切页/重挂载不丢。 */
export function useElapsed(startedAt: number | undefined | null): number {
  const [, tick] = useState(0);
  useEffect(() => {
    if (!startedAt) return;
    const t = setInterval(() => tick(x => x + 1), 1000);
    return () => clearInterval(t);
  }, [startedAt]);
  return startedAt ? Math.max(0, Math.floor((Date.now() - startedAt) / 1000)) : 0;
}
