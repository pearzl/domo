import { describe, expect, it } from "vitest";
import type { Task } from "../types";
import {
  isReordableQueueTask,
  queueEdge,
  reordableQueueIds,
} from "../taskQueue";

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
    artifacts_released_at: null,
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

describe("isReordableQueueTask", () => {
  it("待启动且未执行、未拒绝、未取消的任务可调整", () => {
    expect(isReordableQueueTask(task(), false)).toBe(true);
  });

  it("归档项目中的任务不可调整", () => {
    expect(isReordableQueueTask(task(), true)).toBe(false);
  });

  it("已开始、被拒绝或已取消的任务不可调整", () => {
    expect(isReordableQueueTask(task({ stage: "executing" }), false)).toBe(false);
    expect(isReordableQueueTask(task({ attempts: 1 }), false)).toBe(false);
    expect(isReordableQueueTask(task({ rejections: 1 }), false)).toBe(false);
    expect(isReordableQueueTask(task({ status: "canceled" }), false)).toBe(false);
    expect(isReordableQueueTask(task({ stage: "completed" }), false)).toBe(false);
  });

  it("排队中的待启动任务仍可调整", () => {
    expect(isReordableQueueTask(task({ status: "queued" }), false)).toBe(true);
  });
});

describe("reordableQueueIds", () => {
  it("只返回可调整任务，并按 queue_order 与 seq 排序", () => {
    const tasks = [
      task({ id: "c", seq: 3, queue_order: 2 }),
      task({ id: "b", seq: 3, queue_order: 1 }),
      task({ id: "a", seq: 1, queue_order: 1 }),
      task({ id: "x", seq: 4, queue_order: 0, attempts: 1 }),
      task({ id: "y", seq: 5, queue_order: 0, status: "canceled" }),
    ];
    expect(reordableQueueIds(tasks, false)).toEqual(["a", "b", "c"]);
  });

  it("归档项目返回空队列", () => {
    expect(reordableQueueIds([task({ id: "a" }), task({ id: "b" })], true)).toEqual([]);
  });

  it("空任务列表返回空队列", () => {
    expect(reordableQueueIds([], false)).toEqual([]);
  });
});

describe("queueEdge", () => {
  const queue = ["a", "b", "c"];

  it("首项不能上移、可以下移", () => {
    expect(queueEdge(queue, "a")).toEqual({
      position: 1,
      size: 3,
      canMoveUp: false,
      canMoveDown: true,
    });
  });

  it("末项可以上移、不能下移", () => {
    expect(queueEdge(queue, "c")).toEqual({
      position: 3,
      size: 3,
      canMoveUp: true,
      canMoveDown: false,
    });
  });

  it("中间项可双向移动", () => {
    expect(queueEdge(queue, "b")).toEqual({
      position: 2,
      size: 3,
      canMoveUp: true,
      canMoveDown: true,
    });
  });

  it("单元素队列两端都不能移动", () => {
    expect(queueEdge(["only"], "only")).toEqual({
      position: 1,
      size: 1,
      canMoveUp: false,
      canMoveDown: false,
    });
  });

  it("不在队列中的任务位置为 0 且不能移动", () => {
    expect(queueEdge(queue, "missing")).toEqual({
      position: 0,
      size: 3,
      canMoveUp: false,
      canMoveDown: false,
    });
  });

  it("空队列不产生边界操作", () => {
    expect(queueEdge([], "a")).toEqual({
      position: 0,
      size: 0,
      canMoveUp: false,
      canMoveDown: false,
    });
  });
});
