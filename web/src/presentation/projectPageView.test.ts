import { describe, expect, it } from "vitest";
import type { AnalysisProposal, ProjectDetail } from "../types";
import { projectPageView } from "./projectPageView";

function detail(proposal: AnalysisProposal): ProjectDetail {
  return {
    project: {
      id: "p1",
      stage: "analysis",
      status: "blocked",
      completion_mode: "tasks",
      confirm_mode: "manual",
    },
    tasks: [
      {
        id: "old",
        project_id: "p1",
        key: "T1",
        stage: "executing",
        status: "queued",
        deps: [],
      },
    ],
    jobs: [],
    messages: [],
    events: [],
    agents: [],
    proposal,
  } as unknown as ProjectDetail;
}

function proposal(replan: boolean): AnalysisProposal {
  return {
    kind: "direct",
    summary: "直接完成",
    goal: "目标",
    assumptions: [],
    acceptance: [],
    tasks: [],
    plan: [],
    verification: [],
    skip_confirmation: false,
    replan_impact: replan
      ? {
          high_impact: true,
          canceled_task_keys: ["T1"],
          retained_task_keys: [],
          added_task_keys: [],
          acceptance_changed: false,
        }
      : null,
  };
}

const input = {
  activeTab: "analysis",
  selectedTaskId: null,
  taskQuery: "",
  taskFilter: "all" as const,
  taskDraft: { key: "", title: "", description: "", acceptance: "", deps: [], milestone: false },
  message: "",
  busy: false,
  confirming: false,
};

describe("direct 重规划草稿确认", () => {
  it("历史任务不会阻止 direct 重规划草稿确认", () => {
    expect(projectPageView(detail(proposal(true)), input).confirmBlockedReason).toBeNull();
  });

  it("初始 direct 方案仍受历史任务约束", () => {
    expect(projectPageView(detail(proposal(false)), input).confirmBlockedReason).toBe(
      "项目已有任务，不能直接完成。",
    );
  });
});
