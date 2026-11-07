import { describe, expect, it } from "vitest";
import type { AnalysisProposal, ProjectDetail } from "../types";
import { projectPageView } from "./projectPageView";

function detail(proposal: AnalysisProposal): ProjectDetail {
  return {
    project: {
      id: "p1",
      stage: "analysis",
      status: "blocked",
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
    kind: "plan",
    summary: "方案",
    goal: "目标",
    assumptions: [],
    acceptance: [],
    tasks: [],
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

describe("重规划草稿确认", () => {
  it("执行阶段保留历史方案但不显示待确认徽标", () => {
    const value = detail(proposal(false));
    value.project.stage = "executing";
    value.project.confirmed_at = "2026-01-10T10:00:00Z";
    expect(projectPageView(value, input).badgeInput.proposalPending).toBe(false);
    value.project.stage = "analysis";
    expect(projectPageView(value, input).badgeInput.proposalPending).toBe(true);
  });
  it("历史任务不会阻止重规划草稿确认", () => {
    expect(projectPageView(detail(proposal(true)), input).confirmBlockedReason).toBeNull();
  });

});
