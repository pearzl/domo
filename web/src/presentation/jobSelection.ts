import type { JobSummary, Project } from "../types";

/// 最新失败或人工停止作业提供新会话操作；“继续”读取服务端资格。
export function latestFailedJobId(jobs: JobSummary[]): string | null {
  const last = jobs[jobs.length - 1];
  return last?.status === "failed" ||
    (last?.status === "canceled" && last.failure_kind === "user_stopped") ||
    last?.can_continue
    ? last.id
    : null;
}

/// 项目阻塞且没有报告待验收时，最近的项目级失败作业提供人工执行入口。
export function retryableProjectJobId(
  project: Project,
  jobs: JobSummary[],
): string | null {
  if (project.status !== "blocked") return null;
  if (project.stage === "summarizing" && project.report) return null;
  return latestFailedJobId(jobs.filter((job) => !job.task_id));
}
