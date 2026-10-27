import type { ProjectSummary } from "./types";

/// 项目列表的阶段与状态标签取值；
/// 排序顺序即页面展示顺序。
export const PROJECT_STAGES = [
  "goal_confirm",
  "planning",
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
  /// 同组内多选为"或"，两组之间为"且"。
  stages: ProjectStageFilter[];
  statuses: ProjectStatusFilter[];
}

export const EMPTY_PROJECT_FILTERS: ProjectFilters = { stages: [], statuses: [] };

export function isProjectFilterActive(filters: ProjectFilters): boolean {
  return filters.stages.length > 0 || filters.statuses.length > 0;
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

export function resetProjectFilters(): ProjectFilters {
  return EMPTY_PROJECT_FILTERS;
}

/// 纯过滤：在已加载的项目列表上完成，阶段组和状态组之间取交集。
export function filterProjects(
  projects: ProjectSummary[],
  filters: ProjectFilters,
): ProjectSummary[] {
  if (!isProjectFilterActive(filters)) return projects;
  return projects.filter(
    (project) =>
      matchStage(project, filters.stages) && matchStatus(project, filters.statuses),
  );
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

function toggleValue<T>(values: T[], value: T): T[] {
  return values.includes(value)
    ? values.filter((item) => item !== value)
    : [...values, value];
}
