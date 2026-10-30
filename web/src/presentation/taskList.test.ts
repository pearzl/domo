import { describe, expect, it } from "vitest";
import type { Task } from "../types";
import {
  TASK_FILTERS,
  dependencyOptions,
  filterTasks,
  hasActiveQuery,
  matchesTaskFilter,
  nextTaskKey,
  queueSummary,
  replanSummary,
  taskFilterCounts,
  validateTaskDraft,
} from "./taskList";

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

describe("matchesTaskFilter", () => {
  it("全部匹配任意任务", () => {
    expect(matchesTaskFilter(task(), "all")).toBe(true);
    expect(matchesTaskFilter(task({ status: "canceled" }), "all")).toBe(true);
  });

  it("进行中匹配运行、执行与验收阶段", () => {
    expect(matchesTaskFilter(task({ status: "running" }), "active")).toBe(true);
    expect(matchesTaskFilter(task({ stage: "executing" }), "active")).toBe(true);
    expect(matchesTaskFilter(task({ stage: "verifying" }), "active")).toBe(true);
    expect(matchesTaskFilter(task(), "active")).toBe(false);
    expect(matchesTaskFilter(task({ stage: "completed" }), "active")).toBe(false);
  });

  it("排队匹配待启动且未阻塞或暂停的任务", () => {
    expect(matchesTaskFilter(task({ status: "queued" }), "queued")).toBe(true);
    expect(matchesTaskFilter(task(), "queued")).toBe(true);
    expect(matchesTaskFilter(task({ status: "blocked" }), "queued")).toBe(false);
    expect(matchesTaskFilter(task({ status: "paused" }), "queued")).toBe(false);
    expect(matchesTaskFilter(task({ stage: "executing" }), "queued")).toBe(false);
  });

  it("待启动包含阻塞与暂停，但不含已取消", () => {
    expect(matchesTaskFilter(task(), "pending")).toBe(true);
    expect(matchesTaskFilter(task({ status: "blocked" }), "pending")).toBe(true);
    expect(matchesTaskFilter(task({ status: "paused" }), "pending")).toBe(true);
    expect(matchesTaskFilter(task({ status: "canceled" }), "pending")).toBe(false);
    expect(matchesTaskFilter(task({ stage: "completed" }), "pending")).toBe(false);
  });

  it("待处理只含阻塞、暂停与暂停中", () => {
    expect(matchesTaskFilter(task({ status: "blocked" }), "attention")).toBe(true);
    expect(matchesTaskFilter(task({ status: "paused" }), "attention")).toBe(true);
    expect(matchesTaskFilter(task({ status: "pausing" }), "attention")).toBe(true);
    expect(matchesTaskFilter(task(), "attention")).toBe(false);
    expect(matchesTaskFilter(task({ stage: "executing" }), "attention")).toBe(false);
  });

  it("完成不含已取消，已取消只含取消任务", () => {
    expect(matchesTaskFilter(task({ stage: "completed" }), "completed")).toBe(true);
    expect(
      matchesTaskFilter(task({ stage: "completed", status: "canceled" }), "completed"),
    ).toBe(false);
    expect(matchesTaskFilter(task({ status: "canceled" }), "canceled")).toBe(true);
    expect(matchesTaskFilter(task({ stage: "completed" }), "canceled")).toBe(false);
  });
});

describe("taskFilterCounts", () => {
  it("每个任务至少计入待启动或进行中之一，计数与筛选结果一致", () => {
    const tasks = [
      task({ id: "a", status: "blocked" }),
      task({ id: "b", status: "paused" }),
      task({ id: "c", stage: "executing" }),
      task({ id: "d", stage: "completed" }),
      task({ id: "e", status: "canceled" }),
      task({ id: "f", status: "queued" }),
    ];
    const counts = taskFilterCounts(tasks);
    expect(counts).toEqual({
      all: 6,
      active: 1,
      queued: 1,
      pending: 3,
      attention: 2,
      completed: 1,
      canceled: 1,
    });
    for (const { id } of TASK_FILTERS) {
      expect(counts[id]).toBe(tasks.filter((item) => matchesTaskFilter(item, id)).length);
    }
  });

  it("空任务列表所有计数为 0", () => {
    const counts = taskFilterCounts([]);
    for (const { id } of TASK_FILTERS) expect(counts[id]).toBe(0);
  });
});

describe("filterTasks", () => {
  const tasks = [
    task({ id: "a", key: "T1", title: "搭建脚手架" }),
    task({ id: "b", key: "T2", title: "实现接口", stage: "executing" }),
    task({ id: "c", key: "T10", title: "补充文档" }),
    task({ id: "d", key: "T3", title: "修复 T2 回归", status: "blocked" }),
  ];

  it("空搜索与全部标签返回原列表", () => {
    expect(filterTasks(tasks, "", "all").map((item) => item.id)).toEqual([
      "a",
      "b",
      "c",
      "d",
    ]);
  });

  it("按 key 或标题不区分大小写搜索", () => {
    expect(filterTasks(tasks, "T2", "all").map((item) => item.id)).toEqual(["b", "d"]);
    expect(filterTasks(tasks, "修复", "all").map((item) => item.id)).toEqual(["d"]);
    expect(filterTasks(tasks, "  t10 ", "all").map((item) => item.id)).toEqual(["c"]);
  });

  it("搜索与筛选同时生效", () => {
    expect(filterTasks(tasks, "T", "attention").map((item) => item.id)).toEqual(["d"]);
    expect(filterTasks(tasks, "T2", "queued")).toEqual([]);
  });

  it("无匹配返回空数组", () => {
    expect(filterTasks(tasks, "不存在的任务", "all")).toEqual([]);
    expect(filterTasks([], "", "all")).toEqual([]);
  });

  it("保持任务原有顺序", () => {
    expect(filterTasks(tasks, "t", "all").map((item) => item.key)).toEqual([
      "T1",
      "T2",
      "T10",
      "T3",
    ]);
  });
});

describe("hasActiveQuery", () => {
  it("只有空白搜索与全部标签视为无查询", () => {
    expect(hasActiveQuery("", "all")).toBe(false);
    expect(hasActiveQuery("   ", "all")).toBe(false);
    expect(hasActiveQuery("T1", "all")).toBe(true);
    expect(hasActiveQuery("", "attention")).toBe(true);
  });
});

describe("nextTaskKey", () => {
  it("没有任务时从 T1 开始", () => {
    expect(nextTaskKey([])).toBe("T1");
  });

  it("取最大编号加一，不按数量推算", () => {
    expect(nextTaskKey([task({ key: "T1" }), task({ key: "T3" })])).toBe("T4");
  });

  it("忽略非 T 编号的 key", () => {
    expect(nextTaskKey([task({ key: "setup" }), task({ key: "T2" })])).toBe("T3");
    expect(nextTaskKey([task({ key: "T1.1" }), task({ key: "T2" })])).toBe("T3");
  });

  it("跳过已被占用的编号", () => {
    const tasks = [
      task({ key: "T2" }),
      task({ key: "T3", status: "canceled" }),
    ];
    expect(nextTaskKey(tasks)).toBe("T4");
  });

  it("已取消任务仍占用编号", () => {
    expect(nextTaskKey([task({ key: "T1", status: "canceled" })])).toBe("T2");
  });
});

describe("validateTaskDraft", () => {
  const active = [
    task({ id: "a", key: "T1", stage: "completed" }),
    task({ id: "b", key: "T2", stage: "executing" }),
  ];

  it("合法草稿通过校验", () => {
    const result = validateTaskDraft(
      { key: "T3", title: "新任务", description: "描述", deps: ["T1"] },
      active,
    );
    expect(result.valid).toBe(true);
    expect(result.errors).toEqual({
      key: null,
      title: null,
      description: null,
      deps: null,
    });
  });

  it("空 key 提示不能为空", () => {
    const result = validateTaskDraft(
      { key: "   ", title: "标题", description: "描述", deps: [] },
      active,
    );
    expect(result.valid).toBe(false);
    expect(result.errors.key).toBe("任务 key 不能为空");
  });

  it("重复 key 提示已存在", () => {
    const result = validateTaskDraft(
      { key: " T2 ", title: "标题", description: "描述", deps: [] },
      active,
    );
    expect(result.valid).toBe(false);
    expect(result.errors.key).toBe("T2 已存在，请换一个 key");
  });

  it("已取消任务不占用 key", () => {
    const canceled = [task({ id: "c", key: "T9", status: "canceled" })];
    const result = validateTaskDraft(
      { key: "T9", title: "标题", description: "描述", deps: [] },
      canceled,
    );
    expect(result.valid).toBe(true);
  });

  it("key 超长提示长度上限", () => {
    const result = validateTaskDraft(
      { key: "T".repeat(33), title: "标题", description: "描述", deps: [] },
      active,
    );
    expect(result.errors.key).toContain("32");
  });

  it("空标题与空描述分别提示", () => {
    const result = validateTaskDraft(
      { key: "T3", title: "  ", description: "", deps: [] },
      active,
    );
    expect(result.valid).toBe(false);
    expect(result.errors.title).toBe("标题不能为空");
    expect(result.errors.description).toBe("描述不能为空");
  });

  it("依赖引用自身或非活动任务时提示冲突", () => {
    const self = validateTaskDraft(
      { key: "T3", title: "标题", description: "描述", deps: ["T3"] },
      active,
    );
    expect(self.errors.deps).toBe("依赖不能引用任务自身");
    const missing = validateTaskDraft(
      { key: "T3", title: "标题", description: "描述", deps: ["T1", "T99"] },
      active,
    );
    expect(missing.valid).toBe(false);
    expect(missing.errors.deps).toBe("依赖不在活动任务中：T99");
  });
});

describe("dependencyOptions", () => {
  it("展示任务 key、标题与阶段文案", () => {
    expect(
      dependencyOptions([
        task({ key: "T1", title: "脚手架", stage: "completed" }),
        task({ key: "T2", title: "接口", stage: "executing" }),
        task({ key: "T3", title: "未知", stage: "weird" }),
      ]),
    ).toEqual([
      { key: "T1", title: "脚手架", stage: "completed", stageLabel: "完成" },
      { key: "T2", title: "接口", stage: "executing", stageLabel: "执行" },
      { key: "T3", title: "未知", stage: "weird", stageLabel: "weird" },
    ]);
  });
});

describe("queueSummary", () => {
  const queue = ["a", "b", "c"];

  it("首项展示可调整位置且不能上移", () => {
    expect(queueSummary(queue, "a")).toEqual({
      position: 1,
      total: 3,
      label: "可调整 1/3",
      canMoveUp: false,
      canMoveDown: true,
    });
  });

  it("末项可以上移、不能下移", () => {
    const summary = queueSummary(queue, "c");
    expect(summary.label).toBe("可调整 3/3");
    expect(summary.canMoveUp).toBe(true);
    expect(summary.canMoveDown).toBe(false);
  });

  it("不在队列中的任务标记未入队且两端禁用", () => {
    expect(queueSummary(queue, "x")).toEqual({
      position: 0,
      total: 3,
      label: "未入队 0/3",
      canMoveUp: false,
      canMoveDown: false,
    });
  });

  it("空队列两端禁用", () => {
    const summary = queueSummary([], "a");
    expect(summary.canMoveUp).toBe(false);
    expect(summary.canMoveDown).toBe(false);
  });
});

describe("replanSummary", () => {
  it("未完成任务计入取消，已完成任务计入保留，已取消忽略", () => {
    const summary = replanSummary([
      task({ key: "T1", stage: "completed" }),
      task({ key: "T2", stage: "executing" }),
      task({ key: "T3" }),
      task({ key: "T4", status: "canceled" }),
    ]);
    expect(summary.cancelCount).toBe(2);
    expect(summary.cancelKeys).toEqual(["T2", "T3"]);
    expect(summary.keepCount).toBe(1);
    expect(summary.keepKeys).toEqual(["T1"]);
  });

  it("空任务列表两份清单为空", () => {
    expect(replanSummary([])).toEqual({
      cancelCount: 0,
      cancelKeys: [],
      keepCount: 0,
      keepKeys: [],
    });
  });
});
