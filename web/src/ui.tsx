import type { ReactNode } from "react";

export const STAGE_LABELS: Record<string, string> = {
  analysis: "项目分析",
  executing: "执行中",
  summarizing: "总结中",
  completed: "已完成",
};

export const STATUS_LABELS: Record<string, string> = {
  running: "运行中",
  blocked: "阻塞中",
  queued: "队列中",
  pausing: "暂停中",
  paused: "已暂停",
  archived: "已归档",
};

export const TASK_STAGE_LABELS: Record<string, string> = {
  pending: "待启动",
  executing: "执行",
  verifying: "验收",
  completed: "完成",
};

export const ROLE_LABELS: Record<string, string> = {
  analysis: "项目分析",
  direct: "直接完成",
  planner: "规划",
  executor: "执行",
  verifier: "验收",
  summary: "总结",
};

export const JOB_STATUS_LABELS: Record<string, string> = {
  running: "运行中",
  succeeded: "成功",
  failed: "失败",
  canceled: "已取消",
};

export const FAILURE_KIND_LABELS: Record<string, string> = {
  rate_limit: "限流",
  transient: "供应商波动",
  permanent: "永久错误",
  task: "任务失败",
};

export function formatTime(value?: string | null): string {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString("zh-CN", { hour12: false });
}

export function formatDuration(start?: string | null, end?: string | null): string {
  if (!start) return "—";
  const from = new Date(start).getTime();
  const to = end ? new Date(end).getTime() : Date.now();
  if (Number.isNaN(from) || Number.isNaN(to) || to < from) return "—";
  const seconds = Math.round((to - from) / 1000);
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分 ${seconds % 60} 秒`;
  return `${Math.floor(minutes / 60)} 时 ${minutes % 60} 分`;
}

export function formatTokens(value: number | null | undefined): string {
  if (value === null || value === undefined) return "未知";
  return new Intl.NumberFormat("zh-CN").format(Math.round(value));
}

export function formatCost(value: number | null | undefined): string {
  if (value === null || value === undefined) return "未知";
  if (!value) return "0";
  return value < 1 ? value.toFixed(4) : value.toFixed(2);
}

export function formatMillis(value: number | null | undefined): string {
  if (value === null || value === undefined || value <= 0) return "—";
  const seconds = Math.round(value / 1000);
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分 ${seconds % 60} 秒`;
  return `${Math.floor(minutes / 60)} 时 ${minutes % 60} 分`;
}

export function toneForStatus(status?: string | null): string {
  switch (status) {
    case "running":
    case "executing":
    case "verifying":
      return "info";
    case "blocked":
    case "failed":
      return "danger";
    case "queued":
    case "pausing":
    case "paused":
    case "pending":
      return "warn";
    case "completed":
    case "succeeded":
      return "success";
    case "archived":
    case "canceled":
      return "muted";
    default:
      return "muted";
  }
}

export function Badge({
  children,
  tone = "muted",
  title,
}: {
  children: ReactNode;
  tone?: string;
  title?: string;
}) {
  return (
    <span className={`badge badge-${tone}`} title={title}>
      {children}
    </span>
  );
}

export function StageBadge({ stage }: { stage: string }) {
  const tone =
    stage === "completed"
      ? "success"
      : stage === "executing" || stage === "summarizing"
        ? "info"
        : stage === "analysis"
          ? "warn"
          : "muted";
  return <Badge tone={tone}>{STAGE_LABELS[stage] ?? stage}</Badge>;
}

export function StatusBadge({ status }: { status?: string | null }) {
  if (!status) return null;
  return <Badge tone={toneForStatus(status)}>{STATUS_LABELS[status] ?? status}</Badge>;
}

export function TaskStateBadge({ stage, status }: { stage: string; status?: string | null }) {
  if (status === "canceled") return <Badge tone="muted">已取消</Badge>;
  if (status === "blocked") return <Badge tone="danger">阻塞</Badge>;
  if (status === "running") return <Badge tone="info">运行中</Badge>;
  if (stage === "completed") return <Badge tone="success">完成</Badge>;
  if (status === "paused") return <Badge tone="warn">已暂停</Badge>;
  if (status === "queued") return <Badge tone="warn">队列中</Badge>;
  return <Badge>{TASK_STAGE_LABELS[stage] ?? stage}</Badge>;
}

export function Progress({ total, completed }: { total: number; completed: number }) {
  const percent = total > 0 ? Math.round((completed / total) * 100) : 0;
  return (
    <div className="progress" aria-label={`${completed}/${total}`}>
      <div className="progress-track">
        <div className="progress-fill" style={{ width: `${percent}%` }} />
      </div>
      <span className="progress-text">
        {completed}/{total}
      </span>
    </div>
  );
}

export function Empty({ title, hint }: { title: string; hint?: string }) {
  return (
    <div className="empty">
      <div className="empty-title">{title}</div>
      {hint ? <div className="empty-hint">{hint}</div> : null}
    </div>
  );
}

export function ErrorBox({ message }: { message: string }) {
  return <div className="error-box">{message}</div>;
}
