/** 事件分类供时间线与执行摘要共用；各视图独立决定排序和分组。 */
export const JOB_KINDS = new Set([
  "job_queued", "job_started", "job_continued", "job_finished", "job_failed",
  "job_stalled", "job_stall_recovered", "job_recovered", "job_state_changed",
]);

export const CONFIG_KINDS = new Set([
  "project_agents_updated", "task_agents_updated", "default_team_updated",
  "agent_usage_reset", "analysis_policy_updated", "agent_unavailable",
]);

export const KEY_JOB_KINDS = new Set([
  "job_failed", "job_stalled", "job_recovered", "job_stall_recovered",
]);

const ROUTINE_KINDS = new Set(["task_state_changed", "project_state_changed", "task_executed"]);

export function isKeyEventKind(kind: string): boolean {
  return !CONFIG_KINDS.has(kind) && !ROUTINE_KINDS.has(kind) && (!JOB_KINDS.has(kind) || KEY_JOB_KINDS.has(kind));
}

/** 原始日志按需读取，摘要不把 Agent JSON/进程输出当作用户可读结论。 */
export function eventSummary(message: string, max = 160): string {
  const rawStart = message.search(/Agent 进程退出码|\{"(?:type|sessionID|timestamp)"\s*:|\n\s*\{/);
  const readable = (rawStart >= 0 ? message.slice(0, rawStart) : message).replace(/\s+/g, " ").trim().replace(/[：:]$/, "");
  const chars = Array.from(readable || "作业输出已记录，请展开查看原始详情。");
  return chars.length > max ? `${chars.slice(0, max - 1).join("")}…` : chars.join("");
}
