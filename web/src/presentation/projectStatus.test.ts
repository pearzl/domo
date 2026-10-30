import { describe, expect, it } from "vitest";
import type {
  AnalysisProposal,
  JobSummary,
  Message,
  Project,
  Task,
} from "../types";
import {
  executionTaskGroups,
  formatElapsed,
  projectElapsed,
  projectGuidance,
  projectStats,
  projectStatus,
  type ProjectStatusInput,
} from "./projectStatus";

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
    stage: "analysis",
    status: null,
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

function job(overrides: Partial<JobSummary> = {}): JobSummary {
  return {
    id: "j1",
    project_id: "p1",
    task_id: null,
    role: "analysis",
    agent_id: null,
    agent_name: null,
    status: "succeeded",
    attempt: 1,
    error: null,
    exit_code: 0,
    failure_kind: null,
    failure_summary: null,
    input_tokens: null,
    output_tokens: null,
    cache_read_tokens: null,
    cache_write_tokens: null,
    reasoning_tokens: null,
    cost: null,
    duration_ms: null,
    usage_known: true,
    started_at: null,
    finished_at: null,
    created_at: "2026-01-10T10:00:00Z",
    ...overrides,
  };
}

function message(overrides: Partial<Message> = {}): Message {
  return {
    id: "m1",
    project_id: "p1",
    role: "planner",
    content: "请补充验收标准。",
    created_at: "2026-01-10T10:00:00Z",
    ...overrides,
  };
}

function proposal(overrides: Partial<AnalysisProposal> = {}): AnalysisProposal {
  return {
    kind: "plan",
    summary: "方案",
    goal: null,
    assumptions: [],
    acceptance: [],
    tasks: [],
    plan: [],
    verification: [],
    skip_confirmation: false,
    ...overrides,
  };
}

function input(overrides: Partial<ProjectStatusInput> = {}): ProjectStatusInput {
  return {
    project: project(),
    tasks: [],
    jobs: [],
    messages: [],
    proposal: null,
    mergedIntoBase: false,
    now: NOW,
    ...overrides,
  };
}

describe("projectStats", () => {
  it("完成率排除已取消任务", () => {
    const stats = projectStats(
      input({
        tasks: [
          task({ id: "a", stage: "completed" }),
          task({ id: "b", stage: "completed" }),
          task({ id: "c", stage: "executing", status: "running" }),
          task({ id: "d", stage: "pending", status: "canceled" }),
          task({ id: "e", stage: "completed", status: "canceled" }),
        ],
      }),
    );
    expect(stats.taskTotal).toBe(3);
    expect(stats.taskCompleted).toBe(2);
    expect(stats.taskPercent).toBe(67);
  });

  it("没有非取消任务时完成率为 0", () => {
    const stats = projectStats(
      input({ tasks: [task({ status: "canceled" })] }),
    );
    expect(stats.taskTotal).toBe(0);
    expect(stats.taskPercent).toBe(0);
  });

  it("统计作业失败与用量未知", () => {
    const stats = projectStats(
      input({
        jobs: [
          job({ id: "j1", usage_known: true }),
          job({ id: "j2", status: "failed", usage_known: false }),
          job({ id: "j3", status: "failed", usage_known: false }),
        ],
      }),
    );
    expect(stats.jobTotal).toBe(3);
    expect(stats.jobFailed).toBe(2);
    expect(stats.usageKnown).toBe(1);
    expect(stats.usageUnknown).toBe(2);
  });

  it("带出项目用时", () => {
    const stats = projectStats(input());
    expect(stats.elapsedMs).toBe(2 * 60 * 60 * 1000);
  });
});

describe("projectElapsed", () => {
  it("进行中项目算到当前时刻", () => {
    expect(projectElapsed(project(), NOW)).toBe(2 * 60 * 60 * 1000);
  });

  it("已交付项目算到合入时刻", () => {
    const merged = project({
      stage: "completed",
      merged_at: "2026-01-10T10:30:00Z",
    });
    expect(projectElapsed(merged, NOW)).toBe(30 * 60 * 1000);
  });

  it("归档项目算到更新时间", () => {
    const archived = project({
      status: "archived",
      updated_at: "2026-01-10T11:15:00Z",
    });
    expect(projectElapsed(archived, NOW)).toBe(75 * 60 * 1000);
  });

  it("时间不可解析或倒序时未知", () => {
    expect(projectElapsed(project({ created_at: "not-a-date" }), NOW)).toBeNull();
    expect(projectElapsed(project({ created_at: "2026-02-01T00:00:00Z" }), NOW)).toBeNull();
  });
});

describe("formatElapsed", () => {
  it("按量级输出用时", () => {
    expect(formatElapsed(null)).toBe("—");
    expect(formatElapsed(-1)).toBe("—");
    expect(formatElapsed(5_000)).toBe("5 秒");
    expect(formatElapsed(5 * 60_000 + 1_000)).toBe("5 分钟");
    expect(formatElapsed(3 * 3_600_000 + 10 * 60_000)).toBe("3 小时 10 分");
    expect(formatElapsed(2 * 86_400_000 + 5 * 3_600_000)).toBe("2 天 5 小时");
  });
});

describe("projectGuidance", () => {
  it("已归档项目只读", () => {
    const guidance = projectGuidance(
      input({ project: project({ stage: "completed", status: "archived" }) }),
    );
    expect(guidance.tone).toBe("muted");
    expect(guidance.title).toBe("项目已归档");
  });

  it("暂停与暂停中项目优先给出恢复引导", () => {
    for (const status of ["paused", "pausing"]) {
      const guidance = projectGuidance(
        input({ project: project({ stage: "executing", status }) }),
      );
      expect(guidance.tone).toBe("warn");
      expect(guidance.label).toBe("已暂停");
      expect(guidance.title).toBe("恢复项目");
    }
  });

  it("分析阶段缺少目标时提示输入目标", () => {
    const guidance = projectGuidance(input({ project: project({ goal: "  " }) }));
    expect(guidance.title).toBe("输入项目目标");
    expect(guidance.tone).toBe("warn");
  });

  it("分析阶段阻塞且原因明确时需要人工处理", () => {
    const guidance = projectGuidance(
      input({
        project: project({
          status: "blocked",
          blocked_reason: "\n规划作业失败\n详细原因",
        }),
      }),
    );
    expect(guidance.tone).toBe("danger");
    expect(guidance.label).toBe("需要处理");
    expect(guidance.detail).toBe("规划作业失败");
  });

  it("分析阶段有运行中的分析作业时等待分析", () => {
    const guidance = projectGuidance(
      input({ jobs: [job({ role: "analysis", status: "running" })] }),
    );
    expect(guidance.title).toBe("等待分析");
    expect(guidance.tone).toBe("info");
  });

  it("有方案时提示确认或继续讨论", () => {
    const guidance = projectGuidance(
      input({ proposal: proposal({ kind: "direct" }) }),
    );
    expect(guidance.tone).toBe("success");
    expect(guidance.label).toBe("待确认");
  });

  it("规划者最后发言时提示回复", () => {
    const guidance = projectGuidance(
      input({
        messages: [
          message({ id: "m1", role: "user", content: "开始分析" }),
          message({ id: "m2", role: "planner", content: "需要支持哪些浏览器？" }),
        ],
      }),
    );
    expect(guidance.tone).toBe("warn");
    expect(guidance.label).toBe("待回复");
    expect(guidance.detail).toContain("需要支持哪些浏览器？");
  });

  it("总结阶段区分待验收与总结中", () => {
    const ready = projectGuidance(
      input({
        project: project({ stage: "summarizing", status: "blocked", report: "# 报告" }),
      }),
    );
    expect(ready.tone).toBe("success");
    expect(ready.label).toBe("待验收");
    expect(ready.title).toBe("验收报告");

    const pending = projectGuidance(
      input({ project: project({ stage: "summarizing" }) }),
    );
    expect(pending.title).toBe("等待报告");
  });

  it("已合入项目显示已交付与提交短哈希", () => {
    const guidance = projectGuidance(
      input({
        project: project({
          stage: "completed",
          merged_at: "2026-01-10T11:00:00Z",
          merged_commit: "0123456789abcdef",
        }),
      }),
    );
    expect(guidance.label).toBe("已交付");
    expect(guidance.detail).toBe("已合入 main（提交 0123456）。");
  });

  it("目标分支已包含项目分支时显示已交付", () => {
    const guidance = projectGuidance(
      input({ project: project({ stage: "completed" }), mergedIntoBase: true }),
    );
    expect(guidance.label).toBe("已交付");
    expect(guidance.detail).toContain("domo/p-1");
  });

  it("未合入时提示审阅并合入", () => {
    const guidance = projectGuidance(
      input({ project: project({ stage: "completed" }) }),
    );
    expect(guidance.label).toBe("待合入");
    expect(guidance.title).toBe("审阅并合入");
  });

  it("执行阶段直接完成模式区分作业运行", () => {
    const running = projectGuidance(
      input({
        project: project({ stage: "executing", completion_mode: "direct" }),
        jobs: [job({ role: "direct", status: "running" })],
      }),
    );
    expect(running.title).toBe("等待直接完成");

    const idle = projectGuidance(
      input({ project: project({ stage: "executing", completion_mode: "direct" }) }),
    );
    expect(idle.title).toBe("执行中");
  });

  it("执行阶段阻塞时提示人工处理", () => {
    const guidance = projectGuidance(
      input({
        project: project({
          stage: "executing",
          status: "blocked",
          blocked_reason: "所有任务均失败",
        }),
      }),
    );
    expect(guidance.tone).toBe("danger");
    expect(guidance.detail).toBe("所有任务均失败");
  });

  it("执行阶段无可用任务时给出检查提示", () => {
    const guidance = projectGuidance(
      input({
        project: project({ stage: "executing", status: "running" }),
        tasks: [task({ stage: "completed" })],
      }),
    );
    expect(guidance.tasks).not.toBeNull();
    expect(guidance.detail).toContain("当前没有运行、排队或待启动的任务");
  });

  it("未知状态给出查看项目状态", () => {
    const guidance = projectGuidance(
      input({ project: project({ stage: "unknown" }) }),
    );
    expect(guidance.tone).toBe("muted");
    expect(guidance.title).toBe("查看项目状态");
  });
});

describe("executionTaskGroups", () => {
  it("按运行、排队、待启动分组并排除取消与已完成任务", () => {
    const groups = executionTaskGroups([
      task({ id: "1", key: "T1", stage: "executing", status: "running" }),
      task({ id: "2", key: "T2", stage: "verifying", status: "running" }),
      task({ id: "3", key: "T3", stage: "pending", status: "queued" }),
      task({ id: "4", key: "T4", stage: "pending" }),
      task({ id: "5", key: "T5", stage: "completed" }),
      task({ id: "6", key: "T6", stage: "pending", status: "canceled" }),
      task({ id: "7", key: "T7", stage: "executing", status: "paused" }),
      task({ id: "8", key: "T8", stage: "executing", status: "blocked" }),
    ]);
    expect(groups.running.map((item) => item.key)).toEqual(["T1", "T2"]);
    expect(groups.running.map((item) => item.stageLabel)).toEqual(["执行", "验收"]);
    expect(groups.queued.map((item) => item.key)).toEqual(["T3"]);
    expect(groups.starting.map((item) => item.key)).toEqual(["T4"]);
    expect(groups.starting[0].stageLabel).toBe("待启动");
  });
});

describe("projectStatus", () => {
  it("同时返回统计与引导", () => {
    const status = projectStatus(
      input({
        project: project({ stage: "executing", status: "running" }),
        tasks: [task({ stage: "completed" }), task({ stage: "executing", status: "running", key: "T2" })],
      }),
    );
    expect(status.stats.taskPercent).toBe(50);
    expect(status.guidance.tasks?.running).toHaveLength(1);
  });
});
