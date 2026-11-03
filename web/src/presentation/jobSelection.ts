import type { JobSummary, Project } from "../types";

/// 取最近一个失败作业：作业按时间顺序排列，只有末尾作业失败时才提供重试入口。
export function latestFailedJobId(jobs: JobSummary[]): string | null {
  const last = jobs[jobs.length - 1];
  return last?.status === "failed" ? last.id : null;
}

/// 项目阻塞且当前没有报告待验收时，最近的项目级失败作业可直接重试。
export function retryableProjectJobId(
  project: Project,
  jobs: JobSummary[],
): string | null {
  if (project.status !== "blocked") return null;
  if (project.stage === "summarizing" && project.report) return null;
  return latestFailedJobId(jobs.filter((job) => !job.task_id));
}
