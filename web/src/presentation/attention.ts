import type { AttentionAction, ProjectSummary } from "../types";

export interface AttentionItem {
  id: string;
  action: AttentionAction;
  affectedTasks: number;
  blocksMilestone: boolean;
  projectAdvancing: boolean;
  project: ProjectSummary;
  waitingMs: number;
}

/** 服务端事项逐条展示，等待时间使用事项自己的起点。 */
export function attentionItems(projects: ProjectSummary[], now: number): AttentionItem[] {
  return projects.flatMap(project => project.attention_items.map(action => ({
    id: action.id,
    action,
    affectedTasks: action.affected_tasks,
    blocksMilestone: action.blocks_milestone,
    projectAdvancing: action.project_advancing,
    project,
    waitingMs: Number.isNaN(Date.parse(action.since))
      ? 0 : Math.max(0, now - Date.parse(action.since)),
  }))).sort((left, right) => right.waitingMs - left.waitingMs);
}
