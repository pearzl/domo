import type { ProjectAttention, ProjectSummary } from "./types";

export const PROJECT_ATTENTION_LABELS: Record<ProjectAttention, string> = {
  confirm: "待确认",
  review: "待验收",
  failed: "失败",
  merge: "待合入",
  blocked: "待处理",
};

/// 项目列表的阶段与状态标签取值；
/// 排序顺序即页面展示顺序。
export const PROJECT_STAGES = [
  "analysis",
  "executing",
  "summarizing",
  "completed",
] as const;

export const PROJECT_STATUSES = [
  "running",
  "blocked",
  "pausing",
  "paused",
  "archived",
] as const;

export type ProjectStageFilter = (typeof PROJECT_STAGES)[number];
export type ProjectStatusFilter = (typeof PROJECT_STATUSES)[number];

export interface ProjectFilters {
  /// 空数组表示不加限制（全部）；
  /// 同组内多选为"或"，各筛选条件之间为"且"。
  stages: ProjectStageFilter[];
  statuses: ProjectStatusFilter[];
  /// 只保留与项目详情相同口径的待办项目。
  attentionOnly: boolean;
}

export const EMPTY_PROJECT_FILTERS: ProjectFilters = {
  stages: [],
  statuses: [],
  attentionOnly: false,
};

export function isProjectFilterActive(filters: ProjectFilters): boolean {
  return (
    filters.stages.length > 0 ||
    filters.statuses.length > 0 ||
    filters.attentionOnly
  );
}

/// 同组内为多选"或"：再次点击已选项即取消该选项；
/// 点击"全部"清空该组。
export function toggleProjectStage(
  filters: ProjectFilters,
  stage: ProjectStageFilter,
): ProjectFilters {
  return {
    ...filters,
    stages: toggleValue(filters.stages, stage),
  };
}

export function toggleProjectStatus(
  filters: ProjectFilters,
  status: ProjectStatusFilter,
): ProjectFilters {
  return {
    ...filters,
    statuses: toggleValue(filters.statuses, status),
  };
}

export function toggleProjectAttention(
  filters: ProjectFilters,
): ProjectFilters {
  return { ...filters, attentionOnly: !filters.attentionOnly };
}

export function resetProjectFilters(): ProjectFilters {
  return EMPTY_PROJECT_FILTERS;
}

/// 纯过滤：在已加载的项目列表上完成，阶段、状态与待办条件之间取交集。
export function filterProjects(
  projects: ProjectSummary[],
  filters: ProjectFilters,
): ProjectSummary[] {
  if (!isProjectFilterActive(filters)) return projects;
  return projects.filter(
    (project) =>
      matchStage(project, filters.stages) &&
      matchStatus(project, filters.statuses) &&
      (!filters.attentionOnly || projectNeedsAttention(project)),
  );
}

export function projectNeedsAttention(project: ProjectSummary): boolean {
  return project.attention !== null;
}

export function matchProjectAttention(
  project: ProjectSummary,
  attention: ProjectAttention,
): boolean {
  return project.attention === attention;
}

export function matchProjectStage(
  project: ProjectSummary,
  stage: ProjectStageFilter,
): boolean {
  return project.stage === stage;
}

export function matchProjectStatus(
  project: ProjectSummary,
  status: ProjectStatusFilter,
): boolean {
  return project.status === status;
}

function matchStage(
  project: ProjectSummary,
  stages: ProjectStageFilter[],
): boolean {
  return stages.length === 0 || stages.includes(
    project.stage as ProjectStageFilter,
  );
}

function matchStatus(
  project: ProjectSummary,
  statuses: ProjectStatusFilter[],
): boolean {
  return statuses.length === 0 || statuses.includes(
    project.status as ProjectStatusFilter,
  );
}

/// 标签数量基于当前加载的全部项目单独统计（未套用另一组筛选）。
export function countProjectStage(
  projects: ProjectSummary[],
  stage: ProjectStageFilter,
): number {
  return projects.filter((project) => matchProjectStage(project, stage)).length;
}

export function countProjectStatus(
  projects: ProjectSummary[],
  status: ProjectStatusFilter,
): number {
  return projects.filter((project) =>
    matchProjectStatus(project, status),
  ).length;
}

export function countProjectAttention(projects: ProjectSummary[]): number {
  return projects.filter(projectNeedsAttention).length;
}

function toggleValue<T>(values: T[], value: T): T[] {
  return values.includes(value)
    ? values.filter((item) => item !== value)
    : [...values, value];
}
