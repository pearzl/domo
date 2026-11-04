import { describe, expect, it } from "vitest";
import type { ProjectAttention, ProjectSummary } from "./types";
import {
  EMPTY_PROJECT_FILTERS,
  PROJECT_ATTENTION_LABELS,
  countProjectAttention,
  filterProjects,
  isProjectFilterActive,
  matchProjectAttention,
  projectNeedsAttention,
  resetProjectFilters,
  toggleProjectAttention,
  toggleProjectStage,
} from "./projectFilter";

function project(
  id: string,
  attention: ProjectAttention | null,
  overrides: Partial<ProjectSummary> = {},
): ProjectSummary {
  return {
    id,
    name: id,
    goal: "交付功能",
    repo_path: "/repo",
    base_branch: "main",
    branch: `domo/${id}`,
    stage: "executing",
    status: "running",
    blocked_reason: null,
    attention_items: attention ? [{ id, kind: attention, task: null, reason: null,
      since: "2026-01-10T11:00:00Z", affected_tasks: 0, blocks_milestone: false,
      blocks_summary: true, project_advancing: false }] : [],
    task_total: 2,
    task_completed: 1,
    replan_reason: null,
    replan_source: null,
    confirm_mode: "manual",
    completion_mode: "tasks",
    merged_at: null,
    merged_commit: null,
    created_at: "2026-01-10T10:00:00Z",
    updated_at: "2026-01-10T11:00:00Z",
    ...overrides,
  };
}

const PROJECTS = [
  project("confirm", "confirm", { stage: "analysis", status: "blocked" }),
  project("review", "review", { stage: "summarizing", status: "blocked" }),
  project("failed", "failed", { status: "blocked" }),
  project("merge", "merge", { stage: "completed" }),
  project("blocked", "blocked", { status: "blocked" }),
  project("running", null),
  project("archived", null, { status: "archived" }),
];

describe("项目待我处理筛选", () => {
  it("覆盖与详情一致的每种待办分类", () => {
    const expected: ProjectAttention[] = [
      "confirm",
      "review",
      "failed",
      "merge",
      "blocked",
    ];
    for (const attention of expected) {
      const match = PROJECTS.find((item) =>
        matchProjectAttention(item, attention),
      );
      expect(match?.id).toBe(attention);
      expect(PROJECT_ATTENTION_LABELS[attention]).toBeTruthy();
      expect(match && projectNeedsAttention(match)).toBe(true);
    }
    expect(countProjectAttention(PROJECTS)).toBe(expected.length);
  });

  it("待我处理只保留当前待办，不误报普通进展与归档", () => {
    const filters = toggleProjectAttention(EMPTY_PROJECT_FILTERS);
    expect(filterProjects(PROJECTS, filters).map((item) => item.id)).toEqual([
      "confirm",
      "review",
      "failed",
      "merge",
      "blocked",
    ]);
    expect(isProjectFilterActive(filters)).toBe(true);
    expect(projectNeedsAttention(project("none", null))).toBe(false);
  });

  it("待办视角与阶段筛选取交集", () => {
    const attention = toggleProjectAttention(EMPTY_PROJECT_FILTERS);
    const filters = toggleProjectStage(attention, "analysis");
    expect(filterProjects(PROJECTS, filters).map((item) => item.id)).toEqual([
      "confirm",
    ]);
  });

  it("再次点击与重置都恢复全部项目", () => {
    const active = toggleProjectAttention(EMPTY_PROJECT_FILTERS);
    expect(toggleProjectAttention(active)).toEqual(EMPTY_PROJECT_FILTERS);
    expect(resetProjectFilters()).toEqual(EMPTY_PROJECT_FILTERS);
    expect(filterProjects(PROJECTS, resetProjectFilters())).toHaveLength(
      PROJECTS.length,
    );
  });
});
