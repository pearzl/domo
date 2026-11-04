import { describe, expect, it } from "vitest";
import type { ProjectSummary } from "../types";
import { attentionItems } from "./attention";

const NOW = Date.parse("2026-01-10T12:00:00Z");

function project(overrides: Partial<ProjectSummary> = {}): ProjectSummary {
  return {
    id: "p1",
    name: "示例项目",
    goal: "交付目标",
    repo_path: "/repo",
    base_branch: "main",
    branch: "domo/p-1",
    workspace_type: "git",
    stage: "executing",
    status: "blocked",
    blocked_reason: "等待人工处理",
    attention_items: [{ id: "action", kind: "blocked", task: null, reason: null,
      since: overrides.updated_at ?? "2026-01-10T11:00:00Z", affected_tasks: 0,
      blocks_milestone: false, blocks_summary: true, project_advancing: false }],
    task_total: 1,
    task_completed: 0,
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

describe("attentionItems", () => {
  it("运行中项目可以有多个独立待办，等待时间不受项目更新影响", () => {
    const action = { kind: "review" as const, task: null, reason: "等待里程碑",
      since: "2026-01-10T10:00:00Z", affected_tasks: 2,
      blocks_milestone: true, blocks_summary: true, project_advancing: true };
    const items = attentionItems([project({ status: "running",
      updated_at: "2026-01-10T11:59:59Z",
      attention_items: [{ ...action, id: "t1" }, { ...action, id: "t2" }],
    })], NOW);
    expect(items).toHaveLength(2);
    expect(items.map(item => item.id)).toEqual(["t1", "t2"]);
    expect(items[0].waitingMs).toBe(2 * 60 * 60 * 1000);
    expect(items[0].projectAdvancing).toBe(true);
  });
  it("过滤非待办项目并按等待时长倒序", () => {
    const items = attentionItems(
      [
        project({ id: "recent", updated_at: "2026-01-10T11:50:00Z" }),
        project({ id: "old", updated_at: "2026-01-10T10:00:00Z" }),
        project({ id: "done", attention_items: [] }),
        project({ id: "archived", attention_items: [], status: "archived" }),
      ],
      NOW,
    );
    expect(items.map((item) => item.project.id)).toEqual(["old", "recent"]);
    expect(items[0].waitingMs).toBe(2 * 60 * 60 * 1000);
  });

  it("无法解析更新时间时按零等待处理", () => {
    const items = attentionItems(
      [project({ updated_at: "not-a-time" })],
      NOW,
    );
    expect(items).toHaveLength(1);
    expect(items[0].waitingMs).toBe(0);
  });
});
