import { describe, expect, it } from "vitest";
import type { AuditEvent, JobSummary, Project, Task } from "../types";
import {
  RECENT_EVENT_LIMIT,
  executionNow,
  formatElapsedLabel,
  isKeyEvent,
  isWaitingTask,
  jobElapsedMs,
  queuedTasks,
  recentKeyEvents,
  runningJobs,
} from "./execution";

const NOW = Date.parse("2026-01-10T12:00:00Z");

function project(overrides: Partial<Project> = {}): Project {
  return {
    id: "p1",
    name: "示例项目",
    goal: "交付一个可用的功能",
    repo_path: "/repo",
    base_branch: "main",
    branch: "domo/p-1",
    workspace: "/ws",
    stage: "executing",
    status: "running",
    blocked_reason: null,
    report: null,
    report_feedback: null,
    replan_reason: null,
    replan_source: null,
    replan_requested_at: null,
    confirm_mode: "manual",
    confirmed_at: null,
    completion_mode: "tasks",
    merged_at: null,
    merged_commit: null,
    created_at: "2026-01-10T10:00:00Z",
    updated_at: "2026-01-10T11:00:00Z",
    ...overrides,
  };
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

function job(overrides: Partial<JobSummary> = {}): JobSummary {
  return {
    id: "j1",
    project_id: "p1",
    task_id: null,
    role: "executor",
    agent_id: "a1",
    agent_name: "执行者 A",
    status: "running",
    attempt: 1,
    error: null,
    exit_code: null,
    failure_kind: null,
    failure_summary: null,
    input_tokens: null,
    output_tokens: null,
    cache_read_tokens: null,
    cache_write_tokens: null,
    reasoning_tokens: null,
    cost: null,
    duration_ms: null,
    usage_known: false,
    started_at: "2026-01-10T11:55:00Z",
    finished_at: null,
    created_at: "2026-01-10T11:54:00Z",
    ...overrides,
  };
}

function event(overrides: Partial<AuditEvent> = {}): AuditEvent {
  return {
    id: 1,
    project_id: "p1",
    task_id: null,
    job_id: null,
    kind: "task_completed",
    message: "任务完成",
    message_length: 4,
    message_truncated: false,
    created_at: "2026-01-10T11:00:00Z",
    ...overrides,
  };
}

function taskMap(tasks: Task[]): Map<string, Task> {
  const map = new Map<string, Task>();
  for (const item of tasks) map.set(item.id, item);
  return map;
}

describe("runningJobs", () => {
  it("只保留运行中的作业", () => {
    const tasks = taskMap([task({ id: "t1", key: "T1", title: "实现接口" })]);
    const items = runningJobs(
      [
        job({ id: "j1", status: "running", task_id: "t1" }),
        job({ id: "j2", status: "succeeded" }),
        job({ id: "j3", status: "failed" }),
        job({ id: "j4", status: "canceled" }),
      ],
      tasks,
      NOW,
    );
    expect(items.map((item) => item.id)).toEqual(["j1"]);
  });

  it("区分项目作业与任务作业并带出角色、Agent、任务 key 与标题", () => {
    const tasks = taskMap([task({ id: "t1", key: "T3", title: "事件时间线" })]);
    const items = runningJobs(
      [job({ id: "j1", role: "analysis", task_id: null }), job({ id: "j2", task_id: "t1" })],
      tasks,
      NOW,
    );
    expect(items).toHaveLength(2);
    expect(items[0].roleLabel).toBe("项目分析");
    expect(items[0].taskKey).toBeNull();
    expect(items[0].taskTitle).toBeNull();
    expect(items[1].roleLabel).toBe("执行");
    expect(items[1].agentName).toBe("执行者 A");
    expect(items[1].taskKey).toBe("T3");
    expect(items[1].taskTitle).toBe("事件时间线");
  });

  it("任务不在详情里时保留空任务信息", () => {
    const items = runningJobs([job({ id: "j1", task_id: "missing" })], taskMap([]), NOW);
    expect(items[0].taskKey).toBeNull();
    expect(items[0].taskTitle).toBeNull();
  });

  it("按开始时间先后排列，开始时间未知的排在最后", () => {
    const items = runningJobs(
      [
        job({ id: "late", started_at: "2026-01-10T11:58:00Z" }),
        job({ id: "unknown", started_at: null }),
        job({ id: "early", started_at: "2026-01-10T11:50:00Z" }),
      ],
      taskMap([]),
      NOW,
    );
    expect(items.map((item) => item.id)).toEqual(["early", "late", "unknown"]);
  });

  it("已运行时长按参照时刻计算", () => {
    const items = runningJobs([job({ id: "j1" })], taskMap([]), NOW);
    expect(items[0].elapsedMs).toBe(5 * 60 * 1000);
  });

  it("开始时间缺失或不可解析时运行时长为未知", () => {
    const items = runningJobs(
      [job({ id: "j1", started_at: null }), job({ id: "j2", started_at: "not-a-time" })],
      taskMap([]),
      NOW,
    );
    expect(items[0].elapsedMs).toBeNull();
    expect(items[1].elapsedMs).toBeNull();
    expect(formatElapsedLabel(items[0].elapsedMs)).toBe("时长未知");
  });
});

describe("jobElapsedMs", () => {
  it("已有结束时刻时取开始到结束", () => {
    expect(
      jobElapsedMs(
        job({
          started_at: "2026-01-10T11:50:00Z",
          finished_at: "2026-01-10T11:55:00Z",
        }),
        NOW,
      ),
    ).toBe(5 * 60 * 1000);
  });

  it("开始时间晚于参照时刻或缺失时返回 null", () => {
    expect(jobElapsedMs(job({ started_at: "2026-01-10T13:00:00Z" }), NOW)).toBeNull();
    expect(jobElapsedMs(job({ started_at: null }), NOW)).toBeNull();
    expect(formatElapsedLabel(null)).toBe("时长未知");
  });

  it("按秒、分、时展示时长", () => {
    expect(formatElapsedLabel(42_000)).toBe("42 秒");
    expect(formatElapsedLabel(125_000)).toBe("2 分 5 秒");
    expect(formatElapsedLabel(3 * 3600_000 + 20 * 60_000)).toBe("3 时 20 分");
  });
});

describe("queuedTasks", () => {
  it("收集排队与待启动任务，排除运行、完成、取消、暂停与阻塞任务", () => {
    const items = queuedTasks([
      task({ id: "t1", key: "T1", stage: "pending" }),
      task({ id: "t2", key: "T2", status: "queued", queue_position: 1 }),
      task({ id: "t3", key: "T3", status: "running", stage: "executing" }),
      task({ id: "t4", key: "T4", stage: "completed", status: "succeeded" }),
      task({ id: "t5", key: "T5", status: "canceled" }),
      task({ id: "t6", key: "T6", status: "paused" }),
      task({ id: "t7", key: "T7", status: "blocked" }),
      task({ id: "t8", key: "T8", stage: "verifying", status: "queued" }),
    ]);
    expect(items.map((item) => item.key)).toEqual(["T2", "T1"]);
  });

  it("按队列位置排序，未入队任务按任务序号排在后面", () => {
    const items = queuedTasks([
      task({ id: "t1", key: "T1", seq: 1, stage: "pending" }),
      task({ id: "t2", key: "T2", seq: 2, status: "queued", queue_position: 2 }),
      task({ id: "t3", key: "T3", seq: 3, status: "queued", queue_position: 1 }),
      task({ id: "t4", key: "T4", seq: 4, stage: "pending" }),
    ]);
    expect(items.map((item) => item.key)).toEqual(["T3", "T2", "T1", "T4"]);
    expect(items[0].positionLabel).toBe("队列第 1 位");
    expect(items[2].positionLabel).toBe("未入队");
  });

  it("保留服务端等待原因，缺失时按状态补默认说明", () => {
    const items = queuedTasks([
      task({ id: "t1", key: "T1", stage: "pending", waiting_reason: "等待前置任务：T0" }),
      task({ id: "t2", key: "T2", status: "queued", queue_position: 1 }),
      task({ id: "t3", key: "T3", stage: "pending", waiting_reason: "   " }),
    ]);
    const byKey = new Map(items.map((item) => [item.key, item]));
    expect(byKey.get("T1")?.waitingReason).toBe("等待前置任务：T0");
    expect(byKey.get("T2")?.waitingReason).toBe("等待派发");
    expect(byKey.get("T3")?.waitingReason).toBe("等待调度");
  });

  it("空任务列表返回空队列", () => {
    expect(queuedTasks([])).toEqual([]);
  });

  it("等待判定排除暂停、阻塞、运行、完成与取消", () => {
    expect(isWaitingTask(task({ stage: "pending" }))).toBe(true);
    expect(isWaitingTask(task({ status: "queued", stage: "pending" }))).toBe(true);
    expect(isWaitingTask(task({ status: "pausing", stage: "pending" }))).toBe(false);
    expect(isWaitingTask(task({ status: "blocked", stage: "pending" }))).toBe(false);
    expect(isWaitingTask(task({ status: "running", stage: "pending" }))).toBe(false);
    expect(isWaitingTask(task({ status: "succeeded", stage: "completed" }))).toBe(false);
    expect(isWaitingTask(task({ status: "canceled", stage: "pending" }))).toBe(false);
  });
});

describe("recentKeyEvents", () => {
  it("排除配置类事件与普通作业阶段，保留失败、卡死与恢复", () => {
    expect(isKeyEvent("project_agents_updated")).toBe(false);
    expect(isKeyEvent("job_started")).toBe(false);
    expect(isKeyEvent("job_queued")).toBe(false);
    expect(isKeyEvent("job_failed")).toBe(true);
    expect(isKeyEvent("job_stalled")).toBe(true);
    expect(isKeyEvent("job_recovered")).toBe(true);
    expect(isKeyEvent("job_stall_recovered")).toBe(true);
    expect(isKeyEvent("task_completed")).toBe(true);
  });

  it("只取最近三条关键事件，最新在前", () => {
    const events = [
      event({ id: 1, kind: "task_created", created_at: "2026-01-10T11:00:00Z" }),
      event({ id: 2, kind: "project_agents_updated", created_at: "2026-01-10T11:01:00Z" }),
      event({ id: 3, kind: "task_completed", created_at: "2026-01-10T11:02:00Z" }),
      event({ id: 4, kind: "job_failed", created_at: "2026-01-10T11:03:00Z" }),
      event({ id: 5, kind: "planned", created_at: "2026-01-10T11:04:00Z" }),
    ];
    const items = recentKeyEvents(events, taskMap([]));
    expect(items).toHaveLength(RECENT_EVENT_LIMIT);
    expect(items.map((item) => item.id)).toEqual([5, 4, 3]);
  });

  it("带出事件所属任务的 key", () => {
    const tasks = taskMap([task({ id: "t1", key: "T3" })]);
    const items = recentKeyEvents([event({ id: 1, task_id: "t1", job_id: "j1" })], tasks);
    expect(items[0].taskKey).toBe("T3");
    expect(items[0].jobId).toBe("j1");
  });

  it("配置类事件单独出现时不进入关键事件", () => {
    const items = recentKeyEvents(
      [event({ id: 1, kind: "agent_usage_reset" }), event({ id: 2, kind: "agent_unavailable" })],
      taskMap([]),
    );
    expect(items).toEqual([]);
  });
});

describe("executionNow", () => {
  it("汇总运行中作业、排队任务与最近关键事件", () => {
    const tasks = [task({ id: "t1", key: "T1", title: "实现接口", queue_position: 1, status: "queued" })];
    const view = executionNow({
      project: project(),
      tasks,
      jobs: [
        job({ id: "j1", task_id: "t1" }),
        job({ id: "j2", status: "failed", failure_kind: "task" }),
      ],
      events: [
        event({ id: 1, kind: "job_failed", job_id: "j2", created_at: "2026-01-10T11:00:00Z" }),
      ],
      now: NOW,
    });
    expect(view.running.map((item) => item.id)).toEqual(["j1"]);
    expect(view.queue.map((item) => item.key)).toEqual(["T1"]);
    expect(view.events.map((item) => item.kind)).toEqual(["job_failed"]);
    expect(view.idle.title).toBe("下一个排队任务：T1");
    expect(view.idle.detail).toContain("实现接口");
  });

  it("没有运行中作业时优先说明阻塞原因", () => {
    const view = executionNow({
      project: project({ status: "blocked", blocked_reason: "等待人工裁决\n第二行" }),
      tasks: [task({ id: "t1", key: "T1", status: "queued", queue_position: 1 })],
      jobs: [],
      events: [],
      now: NOW,
    });
    expect(view.running).toEqual([]);
    expect(view.idle.tone).toBe("danger");
    expect(view.idle.title).toBe("等待人工处理");
    expect(view.idle.detail).toBe("等待人工裁决");
  });

  it("没有作业也没有排队任务时给出阶段说明", () => {
    const view = executionNow({
      project: project({ stage: "summarizing", status: "running" }),
      tasks: [],
      jobs: [],
      events: [],
      now: NOW,
    });
    expect(view.idle.tone).toBe("muted");
    expect(view.idle.title).toBe("没有运行中的作业");
    expect(view.idle.detail).toContain("总结阶段");
  });

  it("暂停项目说明恢复后可继续派发", () => {
    const view = executionNow({
      project: project({ status: "paused" }),
      tasks: [],
      jobs: [],
      events: [],
      now: NOW,
    });
    expect(view.idle.tone).toBe("warn");
    expect(view.idle.title).toBe("项目已暂停");
  });
});
