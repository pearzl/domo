import type { ProjectSummary } from "../types";
import { projectNeedsAttention } from "../projectFilter";

export interface AttentionItem {
  project: ProjectSummary;
  waitingMs: number;
}

/// 待我处理列表：只保留待办项目，按等待时长倒序；无法解析更新时间的项目按 0 计。
export function attentionItems(
  projects: ProjectSummary[],
  now: number,
): AttentionItem[] {
  return projects
    .filter(projectNeedsAttention)
    .map((project) => ({
      project,
      waitingMs: waitingMs(project, now),
    }))
    .sort((left, right) => right.waitingMs - left.waitingMs);
}

function waitingMs(project: ProjectSummary, now: number): number {
  const since = Date.parse(project.updated_at);
  if (Number.isNaN(since)) return 0;
  return Math.max(0, now - since);
}
