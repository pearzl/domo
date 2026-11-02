import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { JobSummary, ProjectDetail, Task } from "./types";
import { TaskDrawer } from "./pages/ProjectPage";
import { JobDetailView, JobRecordList, RECENT_JOB_LIMIT } from "./components/JobDetailView";
import { TaskHistoryBlock } from "./components/TaskHistory";
import {
  TASK_HISTORY_FILTER_LABEL,
  TASK_HISTORY_FILTER_EMPTY,
  TASK_HISTORY_TITLE,
  taskHistory,
} from "./presentation/taskHistory";
import type { VerificationHistory, VerificationRecord } from "./types";

function at(minute: number): string {
  return `2026-01-10T10:${String(minute).padStart(2, "0")}:00Z`;
}

function job(
  id: string,
  role: "executor" | "verifier",
  minute: number,
  overrides: Partial<JobSummary> = {},
): JobSummary {
  return {
    id,
    project_id: "p1",
    task_id: "t1",
    role,
    agent_id: null,
    agent_name: role === "executor" ? `执行者${minute}` : `验收者${minute}`,
    status: "succeeded",
    attempt: 1,
    error: null,
    exit_code: 0,
    failure_kind: null,
    failure_summary: null,
    input_tokens: 10,
    output_tokens: 20,
    cache_read_tokens: null,
    cache_write_tokens: null,
    reasoning_tokens: null,
    cost: 0.01,
    duration_ms: 60_000,
    usage_known: true,
    started_at: at(minute),
    finished_at: at(minute),
    created_at: at(minute),
    ...overrides,
  };
}

/// 3 轮执行 2 轮验收：第 1 轮通过，第 2 轮拒绝，第 3 轮待验收。
const HISTORY_JOBS: JobSummary[] = [
  job("e1", "executor", 0),
  job("v1", "verifier", 2),
  job("e2", "executor", 5, { usage_known: false, duration_ms: null }),
  job("v2", "verifier", 7, { status: "failed", error: "验收进程退出" }),
  job("e3", "executor", 10),
];

const HISTORY_RESULTS = {
  e1: "## 第一轮\n\n- 完成 A",
  e2: "第二轮执行结果首行\n细节",
};

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "t1",
    project_id: "p1",
    seq: 1,
    queue_order: 1,
    queue_position: null,
    key: "T1",
    title: "示例任务",
    description: "任务描述",
    acceptance: ["验收项一"],
    deps: [],
    parent_id: null,
    milestone: false,
    human_gate: null,
    forced_agent_id: null,
    forced_agent_role: null,
    executor_agent_id: null,
    verifier_agent_id: null,
    stage: "running",
    status: null,
    agent_id: null,
    last_agent_id: null,
    worktree: null,
    branch: null,
    artifacts_released_at: null,
    attempts: 3,
    rejections: 1,
    last_error: null,
    last_failure_kind: null,
    waiting_reason: null,
    result_summary: null,
    verifier_issues: [],
    guidance: "",
    created_at: at(0),
    updated_at: at(0),
    ...overrides,
  };
}

function detail(tasks: Task[], jobs: JobSummary[] = []): ProjectDetail {
  return {
    project: {
      id: "p1",
      name: "示例项目",
      repo_path: "/repo",
      base_branch: "main",
      branch: "domo/p1",
      workspace: "/ws",
      stage: "running",
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
      created_at: at(0),
      updated_at: at(0),
    },
    tasks,
    jobs,
    agents: [],
  } as unknown as ProjectDetail;
}

/// 验收记录：第 1 轮通过，第 2 轮拒绝。
function record(
  overrides: Partial<VerificationRecord> & { id: string; round: number },
): VerificationRecord {
  return {
    attempt: overrides.round,
    status: "succeeded",
    passed: true,
    reason: "符合验收标准",
    issues: [],
    parsed: true,
    parse_error: null,
    raw_excerpt: null,
    agent_id: null,
    agent_name: "验收者2",
    started_at: null,
    finished_at: null,
    usage_known: true,
    ...overrides,
  };
}

const HISTORY_RECORDS: VerificationHistory = {
  project_id: "p1",
  task_id: "t1",
  total: 2,
  limit: 50,
  truncated: false,
  verifications: [
    record({ id: "v1", round: 1 }),
    record({ id: "v2", round: 2, passed: false, reason: "验收进程退出" }),
  ],
};

function renderHistory(
  history = taskHistory(HISTORY_JOBS, HISTORY_RECORDS, "ready", HISTORY_RESULTS),
) {
  return renderToStaticMarkup(<TaskHistoryBlock history={history} onJumpToJob={() => {}} />);
}

describe("TaskHistoryBlock 渲染", () => {
  it("3 轮执行 2 轮验收渲染出 3 条轮次记录与“只看失败”筛选按钮", () => {
    const markup = renderHistory();
    expect(markup).toContain(TASK_HISTORY_TITLE);
    expect((markup.match(/id="task-round-\d+"/g) ?? [])).toHaveLength(3);
    expect((markup.match(/class="task-history-round[ "]/g) ?? [])).toHaveLength(3);
    expect(markup).toContain("第 1 轮");
    expect(markup).toContain("第 2 轮");
    expect(markup).toContain("第 3 轮");
    expect(markup).toContain(TASK_HISTORY_FILTER_LABEL);
    expect(markup).toContain("<button");
    expect(markup).toContain('aria-pressed="false"');
    expect(markup).toContain("3 轮 · 失败 1 轮");
  });

  it("每轮展示 Agent、状态、时长、用量是否已知、结果摘要首行与验收结论", () => {
    const markup = renderHistory();
    const first = markup.slice(
      markup.indexOf('id="task-round-1"'),
      markup.indexOf('id="task-round-2"'),
    );
    expect(first).toContain("执行者0");
    expect(first).toContain("成功");
    expect(first).toContain("1 分");
    expect(first).toContain("用量已知");
    expect(first).toContain("第一轮");
    expect(first).not.toContain("## 第一轮");
    expect(first).toContain("通过");
    expect(first).toContain("验收者2");

    const second = markup.slice(
      markup.indexOf('id="task-round-2"'),
      markup.indexOf('id="task-round-3"'),
    );
    expect(second).toContain("用量未知");
    expect(second).toContain("时长未知");
    expect(second).toContain("第二轮执行结果首行");
    expect(second).toContain("拒绝");
    expect(second).toContain("验收原因：验收进程退出");

    const third = markup.slice(markup.indexOf('id="task-round-3"'));
    expect(third).toContain("未验收");
    expect(third).toContain("尚未产出结果");
  });

  it("没有失败轮次时筛选按钮不可用", () => {
    const markup = renderToStaticMarkup(
      <TaskHistoryBlock
        history={taskHistory([HISTORY_JOBS[0], HISTORY_JOBS[1]], null, "loading")}
        onJumpToJob={() => {}}
      />,
    );
    expect(markup).toContain(TASK_HISTORY_FILTER_LABEL);
    expect(markup).toContain("disabled");
    expect(markup).toContain("失败 0 轮");
  });

  it("筛选后为空时给出空状态文案", () => {
    expect(TASK_HISTORY_FILTER_EMPTY).toBe("没有失败的轮次。");
  });
});

describe("TaskDrawer 执行与验收历史", () => {
  it("执行历史位于验收摘要之后，渲染出 3 轮记录", () => {
    const current = task({ stage: "verifying" });
    const markup = renderToStaticMarkup(
      <TaskDrawer
        task={current}
        detail={detail([current], HISTORY_JOBS)}
        onClose={() => {}}
        onRetry={() => {}}
        onRetryJob={() => {}}
        onSaveGuidance={async () => true}
        onToggleMilestone={() => {}}
        onRefresh={async () => {}}
        archived={false}
        busy={false}
        agentName={() => "—"}
      />,
    );
    expect(markup.indexOf('data-drawer-block="verification"')).toBeGreaterThan(0);
    expect(markup.indexOf('data-drawer-block="history"')).toBeGreaterThan(
      markup.indexOf('data-drawer-block="verification"'),
    );
    expect((markup.match(/id="task-round-\d+"/g) ?? [])).toHaveLength(3);
    expect(markup).toContain(TASK_HISTORY_FILTER_LABEL);
    // 不新增写操作：执行历史区块内只有筛选与跳转按钮。
    const start = markup.indexOf('data-drawer-block="history"');
    const section = markup.slice(start, markup.indexOf("</section>", start));
    // 唯一的写操作入口是“只看失败”筛选；跳转只改变本地展开状态。
    expect((section.match(/<button/g) ?? []).length).toBeGreaterThan(0);
    expect(section).not.toContain("保存");
    expect(section).not.toContain("重试");
  });

  it("没有执行或验收作业时给出空状态", () => {
    const current = task();
    const markup = renderToStaticMarkup(
      <TaskDrawer
        task={current}
        detail={detail([current])}
        onClose={() => {}}
        onRetry={() => {}}
        onRetryJob={() => {}}
        onSaveGuidance={async () => true}
        onToggleMilestone={() => {}}
        onRefresh={async () => {}}
        archived={false}
        busy={false}
        agentName={() => "—"}
      />,
    );
    expect(markup).toContain('data-drawer-block="history"');
    expect(markup).toContain("还没有执行或验收作业");
    expect(markup).not.toContain(TASK_HISTORY_FILTER_LABEL);
  });
});

describe("JobRecordList 跳转目标", () => {
  function list(count: number, revealJobId: string | null) {
    const jobs = Array.from({ length: count }, (_, index) =>
      job(`job-${index + 1}`, "executor", index),
    );
    return renderToStaticMarkup(
      <JobRecordList
        jobs={jobs}
        revealJobId={revealJobId}
        renderJob={(item) => <div key={item.id}>{item.agent_name ?? item.id}</div>}
      />,
    );
  }

  it("跳转目标在最近切片之外时仍展示全部作业", () => {
    const markup = list(RECENT_JOB_LIMIT + 2, "job-1");
    expect(markup).toContain("执行者0");
    expect(markup).not.toContain("展开全部");
  });

  it("没有跳转目标时保持既有行为", () => {
    const markup = list(RECENT_JOB_LIMIT + 2, null);
    expect(markup).not.toContain("执行者0");
    expect(markup).toContain("展开全部（还有 2 条）");
  });
});

describe("从执行历史跳转到作业详情", () => {
  it("跳转目标的作业详情默认展开", () => {
    const failed = job("job-1", "executor", 0, { status: "failed" });
    const markup = renderToStaticMarkup(
      <JobRecordList
        jobs={[failed]}
        renderJob={(item) => (
          <JobDetailView key={item.id} job={item} focusNonce={2} />
        )}
      />,
    );
    const details = markup.slice(markup.indexOf("<details"), markup.indexOf(">", markup.indexOf("<details")) + 1);
    expect(details).toContain("open");
    expect(markup).toContain("作业活动");
  });
});
