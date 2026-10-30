import { describe, expect, it } from "vitest";
import {
  JOB_STATUS_LABELS,
  JOB_STATUS_ORDER,
  PROJECT_STAGE_ORDER,
  PROJECT_STATUS_ORDER,
  STAGE_LABELS,
  STATUS_LABELS,
  TASK_STAGE_LABELS,
  TASK_STAGE_ORDER,
  TASK_STATUS_LABELS,
  jobStatusLabel,
  projectStatusLabel,
  stageLabel,
  taskStageLabel,
  taskStateLabel,
  taskStatusLabel,
} from "../ui";
import { executionTaskGroups } from "./projectStatus";
import type { Task } from "../types";

/// 界面里出现的每个枚举值都必须有中文标签，未覆盖时回落到原值会露出英文。
const CHINESE = /[\u4e00-\u9fa5]/;

function expectAllLabeled(
  order: readonly string[],
  labels: Record<string, string>,
  name: string,
) {
  for (const value of order) {
    const label = labels[value];
    expect(label, `${name} 缺少 ${value} 的中文标签`).toBeTruthy();
    expect(CHINESE.test(label), `${name}.${value} 必须是中文：${label}`).toBe(true);
  }
}

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "t1",
    project_id: "p1",
    seq: 1,
    queue_order: 0,
    queue_position: null,
    key: "T1",
    title: "任务",
    description: "",
    acceptance: [],
    deps: [],
    parent_id: null,
    milestone: false,
    human_gate: null,
    forced_agent_id: null,
    forced_agent_role: null,
    executor_agent_id: null,
    verifier_agent_id: null,
    stage: "pending",
    status: null,
    agent_id: null,
    last_agent_id: null,
    worktree: null,
    branch: null,
    attempts: 0,
    rejections: 0,
    last_error: null,
    last_failure_kind: null,
    waiting_reason: null,
    result_summary: null,
    verifier_issues: [],
    guidance: "",
    created_at: "2026-01-10T10:00:00Z",
    updated_at: "2026-01-10T10:00:00Z",
    ...overrides,
  };
}

describe("状态文案", () => {
  it("项目阶段都有中文标签", () => {
    expectAllLabeled(PROJECT_STAGE_ORDER, STAGE_LABELS, "项目阶段");
    expect(PROJECT_STAGE_ORDER).toEqual(Object.keys(STAGE_LABELS));
  });

  it("项目状态都有中文标签", () => {
    expectAllLabeled(PROJECT_STATUS_ORDER, STATUS_LABELS, "项目状态");
    expect(PROJECT_STATUS_ORDER).toEqual(Object.keys(STATUS_LABELS));
  });

  it("任务阶段都有中文标签", () => {
    expectAllLabeled(TASK_STAGE_ORDER, TASK_STAGE_LABELS, "任务阶段");
    expect(TASK_STAGE_ORDER).toEqual(Object.keys(TASK_STAGE_LABELS));
  });

  it("任务状态都有中文标签", () => {
    expectAllLabeled(
      ["running", "blocked", "queued", "paused", "pausing", "canceled"],
      TASK_STATUS_LABELS,
      "任务状态",
    );
  });

  it("作业状态都有中文标签", () => {
    expectAllLabeled(JOB_STATUS_ORDER, JOB_STATUS_LABELS, "作业状态");
    expect(JOB_STATUS_ORDER).toEqual(Object.keys(JOB_STATUS_LABELS));
  });

  it("标签查询函数不会把英文枚举漏到界面", () => {
    for (const value of PROJECT_STAGE_ORDER) {
      expect(stageLabel(value)).toBe(STAGE_LABELS[value]);
      expect(CHINESE.test(stageLabel(value))).toBe(true);
    }
    for (const value of PROJECT_STATUS_ORDER) {
      expect(projectStatusLabel(value)).toBe(STATUS_LABELS[value]);
      expect(CHINESE.test(projectStatusLabel(value) ?? "")).toBe(true);
    }
    for (const value of TASK_STAGE_ORDER) {
      expect(taskStageLabel(value)).toBe(TASK_STAGE_LABELS[value]);
      expect(CHINESE.test(taskStageLabel(value))).toBe(true);
    }
    for (const value of Object.keys(TASK_STATUS_LABELS)) {
      expect(taskStatusLabel(value)).toBe(TASK_STATUS_LABELS[value]);
      expect(CHINESE.test(taskStatusLabel(value) ?? "")).toBe(true);
    }
    for (const value of JOB_STATUS_ORDER) {
      expect(jobStatusLabel(value)).toBe(JOB_STATUS_LABELS[value]);
      expect(CHINESE.test(jobStatusLabel(value))).toBe(true);
    }
  });

  it("任务状态文案优先反映取消、阻塞、运行与暂停", () => {
    expect(taskStateLabel("executing", "blocked")).toBe("阻塞");
    expect(taskStateLabel("executing", "running")).toBe("运行中");
    expect(taskStateLabel("pending", "canceled")).toBe("已取消");
    expect(taskStateLabel("pending", "paused")).toBe("已暂停");
    expect(taskStateLabel("pending", "queued")).toBe("队列中");
    expect(taskStateLabel("completed", null)).toBe("完成");
    expect(taskStateLabel("verifying", null)).toBe("验收");
  });

  it("概览的任务分组文案与 ui 的任务阶段文案一致", () => {
    const groups = executionTaskGroups([
      task({ id: "a", key: "T1", stage: "executing", status: "running" }),
      task({ id: "b", key: "T2", stage: "verifying", status: "running" }),
      task({ id: "c", key: "T3", stage: "pending", status: "queued" }),
      task({ id: "d", key: "T4", stage: "pending" }),
      task({ id: "e", key: "T5", stage: "completed" }),
    ]);
    const labels = [
      ...groups.running,
      ...groups.queued,
      ...groups.starting,
    ].map((item) => item.stageLabel);
    expect(labels).toEqual([
      TASK_STAGE_LABELS.executing,
      TASK_STAGE_LABELS.verifying,
      TASK_STAGE_LABELS.pending,
      TASK_STAGE_LABELS.pending,
    ]);
  });
});
