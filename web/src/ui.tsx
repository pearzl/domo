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

/// 项目阶段、项目状态、任务阶段、任务状态与作业状态的唯一文案来源；
/// 表现层与组件都从这里取中文标签，保证同一状态在概览、列表、抽屉和时间线里用同一个词。
/// 任务阶段描述任务在项目内的位置，作业状态描述一次 Agent 调用的结果，两者不混用。
export const TASK_STAGE_LABELS: Record<string, string> = {
  pending: "待启动",
  executing: "执行",
  verifying: "验收",
  completed: "完成",
};

export const TASK_STAGE_ORDER: readonly string[] = [
  "pending",
  "executing",
  "verifying",
  "completed",
];

/// 任务状态叠加在阶段之上：取消、阻塞、运行、暂停与排队优先于阶段展示。
export const TASK_STATUS_LABELS: Record<string, string> = {
  running: "运行中",
  blocked: "阻塞",
  queued: "队列中",
  paused: "已暂停",
  pausing: "暂停中",
  canceled: "已取消",
};

export const PROJECT_STAGE_ORDER: readonly string[] = [
  "analysis",
  "executing",
  "summarizing",
  "completed",
];

export const PROJECT_STATUS_ORDER: readonly string[] = [
  "running",
  "blocked",
  "queued",
  "pausing",
  "paused",
  "archived",
];

export const JOB_STATUS_ORDER: readonly string[] = [
  "running",
  "succeeded",
  "failed",
  "canceled",
];

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

export function stageLabel(stage: string | null | undefined): string {
  if (!stage) return "—";
  return STAGE_LABELS[stage] ?? stage;
}

export function projectStatusLabel(status: string | null | undefined): string | null {
  if (!status) return null;
  return STATUS_LABELS[status] ?? status;
}

export function taskStageLabel(stage: string | null | undefined): string {
  if (!stage) return "—";
  return TASK_STAGE_LABELS[stage] ?? stage;
}

export function taskStatusLabel(status: string | null | undefined): string | null {
  if (!status) return null;
  return TASK_STATUS_LABELS[status] ?? status;
}

export function jobStatusLabel(status: string | null | undefined): string {
  if (!status) return "—";
  return JOB_STATUS_LABELS[status] ?? status;
}

/// 任务状态文案：取消、阻塞、运行与暂停优先于阶段，其余按阶段展示。
export function taskStateLabel(stage: string, status?: string | null): string {
  if (status === "canceled") return TASK_STATUS_LABELS.canceled;
  if (status === "blocked") return TASK_STATUS_LABELS.blocked;
  if (status === "running") return TASK_STATUS_LABELS.running;
  if (stage === "completed") return TASK_STAGE_LABELS.completed;
  if (status === "paused") return TASK_STATUS_LABELS.paused;
  if (status === "queued") return TASK_STATUS_LABELS.queued;
  return taskStageLabel(stage);
}

export const FAILURE_KIND_LABELS: Record<string, string> = {
  rate_limit: "限流",
  transient: "供应商波动",
  permanent: "永久错误",
  protocol: "协议失败",
  task: "任务失败",
};

export const FAILURE_SUMMARY_LABELS: Record<string, string> = {
  rate_limit: "限流",
  timeout: "超时",
  stalled: "卡死",
  process_exit: "进程退出",
  protocol: "结果异常",
  permanent: "配置错误",
  provider_error: "供应商错误",
  unknown: "失败",
};

export function failureSummaryLabel(kind?: string | null): string {
  if (!kind) return "失败";
  return FAILURE_SUMMARY_LABELS[kind] ?? kind;
}

/** 行内只放摘要，避免长文本撑开列表。 */
export function shortText(value: string | null | undefined, max = 80): string {
  const collapsed = (value ?? "").split(/\s+/).filter(Boolean).join(" ");
  const chars = Array.from(collapsed);
  if (chars.length <= max) return collapsed;
  return `${chars.slice(0, max - 1).join("")}…`;
}

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
  if (value < 0.0001) return "<0.0001";
  return value < 1 ? value.toFixed(4) : value.toFixed(2);
}

/** 按计量维度格式化用量：费用走 formatCost，其余 token 指标走 formatTokens。 */
export function formatUsageAmount(value: number | null | undefined, metric: string): string {
  if (metric === "cost") return formatCost(value);
  return formatTokens(value);
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
  return <Badge tone={tone}>{stageLabel(stage)}</Badge>;
}

export function StatusBadge({ status }: { status?: string | null }) {
  if (!status) return null;
  return <Badge tone={toneForStatus(status)}>{projectStatusLabel(status)}</Badge>;
}

export function TaskStateBadge({ stage, status }: { stage: string; status?: string | null }) {
  let tone = "muted";
  if (status === "canceled") tone = "muted";
  else if (status === "blocked") tone = "danger";
  else if (status === "running") tone = "info";
  else if (stage === "completed") tone = "success";
  else if (status === "paused" || status === "pausing" || status === "queued") tone = "warn";
  return <Badge tone={tone}>{taskStateLabel(stage, status)}</Badge>;
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

/// 空状态引导：给出可执行的下一步步骤与首选操作，而不是只说明“暂无”。
export function EmptySteps({
  title,
  hint,
  steps,
  action,
}: {
  title: string;
  hint?: string;
  steps: string[];
  action?: { label: string; onClick: () => void };
}) {
  return (
    <div className="empty">
      <div className="empty-title">{title}</div>
      {hint ? <div className="empty-hint">{hint}</div> : null}
      <ol className="empty-steps">
        {steps.map((step) => (
          <li key={step}>{step}</li>
        ))}
      </ol>
      {action ? (
        <div className="empty-actions">
          <button className="primary" onClick={action.onClick}>
            {action.label}
          </button>
        </div>
      ) : null}
    </div>
  );
}

export function ErrorBox({ message }: { message: string }) {
  return <div className="error-box">{message}</div>;
}
