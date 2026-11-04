/** 事件分类供时间线与执行摘要共用；各视图独立决定排序和分组。 */
export const JOB_KINDS = new Set([
  "job_queued", "job_started", "job_continued", "job_finished", "job_failed",
  "job_stalled", "job_stall_recovered", "job_recovered",
]);

export const CONFIG_KINDS = new Set([
  "project_agents_updated", "task_agents_updated", "default_team_updated",
  "agent_usage_reset", "analysis_policy_updated", "agent_unavailable",
]);

export const KEY_JOB_KINDS = new Set([
  "job_failed", "job_stalled", "job_recovered", "job_stall_recovered",
]);

export function isKeyEventKind(kind: string): boolean {
  return !CONFIG_KINDS.has(kind) && (!JOB_KINDS.has(kind) || KEY_JOB_KINDS.has(kind));
}
