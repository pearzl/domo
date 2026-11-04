import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { JobSummary, ProjectDetail, Task } from "./types";
import { TaskDrawer } from "./pages/ProjectPage";
import { JobDetailView, JobRecordList, RECENT_JOB_LIMIT } from "./components/JobDetailView";
import { TaskHistoryBlock } from "./components/TaskHistory";
import taskHistorySource from "./components/TaskHistory.tsx?raw";
import {
  TASK_HISTORY_EMPTY,
  TASK_HISTORY_FILTER_LABEL,
  TASK_HISTORY_FILTER_EMPTY,
  TASK_HISTORY_TITLE,
  taskHistory,
  unlinkedVerifiersLabel,
} from "./presentation/taskHistory";
import type { VerificationHistory, VerificationRecord } from "./types";

function at(minute: number): string {
  return `2026-01-10T10:${String(minute).padStart(2, "0")}:00Z`;
}

function beforeFirst(minute: number): string {
  return `2026-01-10T09:${String(minute).padStart(2, "0")}:00Z`;
}

function job(
  id: string,
  role: "executor" | "verifier",
  minute: number,
  overrides: Partial<JobSummary> = {},
): JobSummary {
  return {
    execution_job_id: role === "verifier" ? "e1" : null,
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

/// 验收记录：默认通过，可按需改为拒绝或未关联执行轮。
function record(
  overrides: Partial<VerificationRecord> & { id: string; round: number },
): VerificationRecord {
  return {
    execution_job_id: "e1",
    attempt: overrides.round,
    status: "succeeded",
    passed: true,
    reason: "符合验收标准",
    issues: [],
    parsed: true,
    parse_error: null,
    raw_excerpt: null,
    agent_id: null,
    agent_name: "验收者",
    started_at: null,
    finished_at: null,
    usage_known: true,
    ...overrides,
  };
}

function history(verifications: VerificationRecord[]): VerificationHistory {
  return {
    project_id: "p1",
    task_id: "t1",
    total: verifications.length,
    limit: 50,
    truncated: false,
    verifications,
  };
}

/// 一次执行 + 三次验收：拒绝、验收进程异常、最终通过，全部归入第 1 轮。
function oneRoundHistory() {
  return taskHistory(
    [
      job("e1", "executor", 0),
      job("v1", "verifier", 2),
      job("v2", "verifier", 3, {
        status: "failed",
        error: "验收进程异常退出",
      }),
      job("v3", "verifier", 4),
    ],
    history([
      record({ id: "v1", round: 1, passed: false, reason: "缺少测试" }),
      record({ id: "v3", round: 1, passed: true, reason: "补充测试后通过" }),
    ]),
    "ready",
  );
}

function renderHistory(value = oneRoundHistory()) {
  return renderToStaticMarkup(<TaskHistoryBlock history={value} onJumpToJob={() => {}} />);
}

describe("TaskHistoryBlock 按执行轮渲染", () => {
  it("长历史默认展示最近三轮，最新在前，保留全部轮次入口", () => {
    const value = taskHistory(Array.from({ length: 20 }, (_, index) => job(`e${index}`, "executor", index)), history([]), "ready");
    const markup = renderHistory(value);
    expect(markup.match(/id="task-round-\d+"/g)).toHaveLength(3);
    expect(markup).toContain("第 20 轮");
    expect(markup).toContain("第 18 轮");
    expect(markup).not.toContain("第 17 轮");
    expect(markup).toContain("展开全部 20 轮");
    expect(markup.indexOf("第 20 轮")).toBeLessThan(markup.indexOf("第 19 轮"));
    expect(markup).toContain('<details class="task-history-body">');
  });
  it("一次执行 + 三次验收（两次失败）只渲染 1 个轮次并汇总本轮验收", () => {
    const markup = renderHistory();
    expect(markup).toContain(TASK_HISTORY_TITLE);
    expect((markup.match(/id="task-round-\d+"/g) ?? [])).toHaveLength(1);
    expect((markup.match(/class="task-history-round[ "]/g) ?? [])).toHaveLength(1);
    expect(markup).not.toContain("第 2 轮");
    expect(markup).not.toContain("尚未产出结果");
    expect(markup).toContain("1 个执行轮次 · 失败 1 轮");
    expect(markup).toContain("验收 3 次");
    expect(markup).toContain("失败 2 次");
    expect(markup).toContain("最近通过");
    expect(markup).toContain(TASK_HISTORY_FILTER_LABEL);
  });

  it("轮内展开“本轮验收”，逐条展示验收者、尝试序号与状态，不渲染原因长文本", () => {
    const markup = renderHistory();
    expect(markup).toContain("本轮验收");
    expect(markup).toContain("3 次尝试");
    expect(markup).toContain("尝试 1");
    expect(markup).toContain("尝试 2");
    expect(markup).toContain("尝试 3");
    expect((markup.match(/class="task-history-attempt"/g) ?? [])).toHaveLength(3);
    expect(markup).not.toContain("task-history-attempt-reason");
    expect(markup).not.toContain("task-history-round-result");

    const [first, second, third] = markup
      .split('<li class="task-history-attempt"')
      .slice(1);
    expect(first).toContain("尝试 1");
    expect(first).toContain("验收者2");
    expect(first).toContain("拒绝");
    expect(first).not.toContain("缺少测试");
    expect(first).toContain("badge-danger");
    expect(first).toContain('data-failed="true"');
    expect(second).toContain("尝试 2");
    expect(second).toContain("验收者3");
    expect(second).toContain("验收异常");
    expect(second).not.toContain("验收进程异常退出");
    expect(second).toContain("badge-danger");
    expect(second).toContain('data-failed="true"');
    expect(third).toContain("尝试 3");
    expect(third).toContain("验收者4");
    expect(third).toContain("通过");
    expect(third).not.toContain("补充测试后通过");
    expect(third).toContain("badge-success");
    expect(third).toContain('data-failed="false"');
    expect(third).not.toContain("待验收");
  });

  it("历史不渲染执行结果文本，长文本只留在作业视图", () => {
    const markup = renderHistory();
    expect(markup).not.toContain("本次执行未产出结果");
    expect(markup).not.toContain("更多细节");
    expect(markup).not.toContain("task-history-round-result");
  });

  it("历史不再为结果首行读取作业详情", () => {
    expect(taskHistorySource).not.toContain("getJob");
    expect(taskHistorySource).not.toContain("useJobResults");
    expect(taskHistorySource).not.toContain("resultText");
    expect(taskHistorySource).not.toContain("resultLine");
  });

  it("两次执行两次验收渲染出第 1 轮与第 2 轮", () => {
    const markup = renderHistory(
      taskHistory(
        [
          job("e1", "executor", 0),
          job("v1", "verifier", 2),
          job("e2", "executor", 5),
          job("v2", "verifier", 7, { execution_job_id: "e2" }),
        ],
        history([
          record({ id: "v1", round: 1 }),
          record({ id: "v2", round: 2, passed: false, reason: "缺少测试" }),
        ]),
        "ready",
      ),
    );
    expect((markup.match(/id="task-round-\d+"/g) ?? [])).toHaveLength(2);
    expect(markup).toContain("第 1 轮");
    expect(markup).toContain("第 2 轮");
    expect(markup).toContain("2 个执行轮次 · 失败 1 轮");

    const first = markup.slice(markup.indexOf('id="task-round-1"'));
    const second = markup.slice(markup.indexOf('id="task-round-2"'), markup.indexOf('id="task-round-1"'));
    expect(markup.indexOf('id="task-round-2"')).toBeLessThan(markup.indexOf('id="task-round-1"'));
    expect(first).toContain("最近通过");
    expect(second).toContain("最近拒绝");
    expect(second).toContain("拒绝");
    expect(second).not.toContain("缺少测试");
  });

  it("未关联执行轮的验收作业在轮次列表上方给出数量与最近状态提示", () => {
    const early = job("v0", "verifier", 0, { execution_job_id: null, created_at: beforeFirst(59) });
    const unanchored = job("v1", "verifier", 6, {
      execution_job_id: null,
      status: "failed",
      error: "验收进程异常退出",
    });
    const value = taskHistory(
      [job("e1", "executor", 5), early, unanchored],
      history([record({ id: "v1", round: 0, passed: false, reason: "缺少测试" })]),
      "ready",
    );
    expect(value.unlinkedVerifiers).toBe(2);
    expect(value.unlinkedJobs.map((entry) => entry.id)).toEqual(["v0", "v1"]);

    const markup = renderHistory(value);
    expect(markup).toContain(unlinkedVerifiersLabel(2));
    expect(markup).toContain("最近一次");
    expect(markup).toContain("失败");
    expect(markup).toContain("拒绝");
    expect(markup).toContain("task-history-unlinked");
    expect(markup.indexOf("task-history-unlinked")).toBeLessThan(
      markup.indexOf('id="task-round-1"'),
    );
  });

  it("没有作业时保留空状态且不显示筛选按钮", () => {
    const markup = renderHistory(taskHistory([], null, "loading"));
    expect(markup).toContain(TASK_HISTORY_EMPTY);
    expect(markup).not.toContain(TASK_HISTORY_FILTER_LABEL);
    expect(markup).not.toContain("task-history-round");
  });

  it("问题轮筛选按钮沿用“只看问题轮”并在没有问题时不可用", () => {
    const failed = renderHistory();
    expect(failed).toContain(TASK_HISTORY_FILTER_LABEL);
    expect(failed).toContain('aria-pressed="false"');
    expect(failed).not.toContain('disabled=""');

    const passed = renderHistory(
      taskHistory(
        [job("e1", "executor", 0), job("v1", "verifier", 2)],
        history([record({ id: "v1", round: 1 })]),
        "ready",
      ),
    );
    expect(passed).toContain("失败 0 轮");
    expect(passed).toContain("disabled");
    expect(TASK_HISTORY_FILTER_EMPTY).toBe("没有失败的轮次。");
  });
});

describe("TaskDrawer 执行与验收历史", () => {
  it("执行历史位于验收摘要之后，按执行轮渲染", () => {
    const current = task({ stage: "completed" });
    const markup = renderToStaticMarkup(
      <TaskDrawer
        task={current}
        detail={detail(
          [current],
          [
            job("e1", "executor", 0),
            job("v1", "verifier", 2),
            job("v2", "verifier", 3, { status: "failed", error: "验收进程异常退出" }),
            job("v3", "verifier", 4),
          ],
        )}
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
    expect((markup.match(/id="task-round-\d+"/g) ?? [])).toHaveLength(1);
    expect(markup).toContain("验收 3 次");
    expect(markup).toContain(TASK_HISTORY_FILTER_LABEL);
    // 不新增写操作：执行历史区块内只有筛选与跳转按钮。
    const start = markup.indexOf('data-drawer-block="history"');
    const section = markup.slice(start, markup.indexOf("</section>", start));
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
    const details = markup.slice(
      markup.indexOf("<details"),
      markup.indexOf(">", markup.indexOf("<details")) + 1,
    );
    expect(details).toContain("open");
    expect(markup).toContain("作业活动");
  });
});
