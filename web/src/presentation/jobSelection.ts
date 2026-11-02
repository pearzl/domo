import type { JobSummary } from "../types";

/// 取最近一个失败作业：作业按时间顺序排列，只有末尾作业失败时才提供重试入口。
export function latestFailedJobId(jobs: JobSummary[]): string | null {
  const last = jobs[jobs.length - 1];
  return last?.status === "failed" ? last.id : null;
}
