import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { JobSummary, ProjectDetail, Task } from "./types";
import { TaskDrawer } from "./pages/ProjectPage";
import projectPageSource from "./pages/ProjectPage.tsx?raw";
import { JobPrompt, RECENT_JOB_LIMIT } from "./components/JobDetailView";

const RAW = "RAW_ERROR_MARKER_9f3c";

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

function job(index: number): JobSummary {
  return {
    id: `job-${index}`,
    project_id: "p1",
    task_id: "t1",
    role: "executor",
    status: "succeeded",
    attempt: 1,
    agent_id: null,
    agent_name: null,
    created_at: "2026-01-10T10:00:00Z",
    started_at: "2026-01-10T10:00:00Z",
    finished_at: "2026-01-10T10:01:00Z",
    duration_ms: 60000,
    exit_code: 0,
    error: null,
    failure_kind: null,
    failure_summary: null,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: null,
    cache_write_tokens: null,
    reasoning_tokens: null,
    cost: 0,
    usage_known: false,
    prompt_path: null,
  } as unknown as JobSummary;
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
      created_at: "2026-01-10T10:00:00Z",
      updated_at: "2026-01-10T10:00:00Z",
    },
    tasks,
    jobs,
    agents: [],
  } as unknown as ProjectDetail;
}

function renderDrawer(current: Task, jobs: JobSummary[] = []) {
  return renderToStaticMarkup(
    <TaskDrawer
      task={current}
      detail={detail([current], jobs)}
      onClose={() => {}}
      onRetry={() => {}}
      onRetryJob={() => {}}
      onSaveGuidance={() => {}}
      onToggleMilestone={() => {}}
      onRefresh={async () => {}}
      archived={false}
      busy={false}
      agentName={() => "—"}
    />,
  );
}

function leadStart(markup: string) {
  return markup.indexOf('class="drawer-lead"');
}

function leadMarkup(markup: string) {
  const start = leadStart(markup);
  const end = markup.indexOf("</section></div>", start);
  return markup.slice(start, end);
}

describe("TaskDrawer 首屏区块", () => {
  it("blocked 首区块为裁决面板", () => {
    const markup = renderDrawer(task({ status: "blocked", last_error: RAW }));
    expect(markup).toContain('data-drawer-lead="gate"');
    const lead = leadMarkup(markup);
    expect(lead).toContain('class="arbitration-panel"');
    expect(lead).toContain("人工裁决");
  });

  it("里程碑人工验收首区块为人工验收面板", () => {
    const markup = renderDrawer(task({ stage: "completed", human_gate: "milestone" }));
    expect(markup).toContain('data-drawer-lead="gate"');
    const lead = leadMarkup(markup);
    expect(lead).toContain('class="arbitration-panel"');
    expect(lead).toContain("人工验收");
  });

  it("completed 首区块为结果摘要", () => {
    const markup = renderDrawer(
      task({ stage: "completed", result_summary: "RESULT_MARKER" }),
    );
    expect(markup).toContain('data-drawer-lead="result"');
    const lead = leadMarkup(markup);
    expect(lead).toContain("结果摘要");
    expect(lead).toContain("RESULT_MARKER");
  });

  it("canceled 首区块为结果摘要", () => {
    const markup = renderDrawer(task({ status: "canceled" }));
    expect(markup).toContain('data-drawer-lead="result"');
    expect(leadMarkup(markup)).toContain("结果摘要");
  });

  it("active 首区块为控制条", () => {
    const markup = renderDrawer(task());
    expect(markup).toContain('data-drawer-lead="control"');
    const lead = leadMarkup(markup);
    expect(lead).toContain('class="task-control"');
    expect(lead).toContain("暂停");
  });

  it("抽屉内不重复展示项目级元信息", () => {
    const markup = renderDrawer(task({ status: "blocked" }));
    expect(markup).not.toContain("示例项目");
    expect(markup).not.toContain("/repo");
  });
});

describe("TaskDrawer 信息去重", () => {
  it("无 gate 时保留一个默认收起的原始错误入口", () => {
    const markup = renderDrawer(task({ last_error: RAW }));
    expect((markup.match(/查看原始错误/g) ?? []).length).toBe(1);
    const entry = markup.slice(markup.indexOf('<details class="raw-error'));
    const detailsTag = entry.slice(0, entry.indexOf(">"));
    expect(detailsTag).not.toContain("open");
    expect(entry.slice(0, entry.indexOf("查看原始错误"))).not.toContain(RAW);
  });

  it("blocked 时抽屉不额外渲染原始错误入口，由 gate 面板承载", () => {
    const markup = renderDrawer(task({ status: "blocked", last_error: RAW }));
    expect((markup.match(/查看原始错误/g) ?? []).length).toBe(0);
    expect(markup).not.toContain(RAW);
    const drawer = markup.slice(leadStart(markup));
    expect(drawer).not.toContain("raw-error drawer-fold");
  });

  it("里程碑验收时原始错误只由 gate 面板承载，全文只出现一次", () => {
    const markup = renderDrawer(
      task({ stage: "completed", human_gate: "milestone", last_error: RAW }),
    );
    expect((markup.match(new RegExp(RAW, "g")) ?? []).length).toBe(1);
    expect(leadMarkup(markup)).toContain(RAW);
  });

  it("只用“验收”区块合并验收标准与验收反馈", () => {
    const markup = renderDrawer(task({ verifier_issues: ["FEEDBACK_MARKER"] }));
    expect((markup.match(/<h3>验收<\/h3>/g) ?? []).length).toBe(1);
    expect((markup.match(/<h3>验收标准<\/h3>/g) ?? []).length).toBe(0);
    expect((markup.match(/<h3>验收反馈<\/h3>/g) ?? []).length).toBe(0);
    expect(markup).toContain("FEEDBACK_MARKER");
    expect(markup).toContain("验收项一");
  });

  it("Agent 池配置区位于作业记录之后且位于抽屉末尾", () => {
    const markup = renderDrawer(task());
    const configIndex = markup.indexOf('class="drawer-config"');
    expect(configIndex).toBeGreaterThan(0);
    const config = markup.slice(configIndex);
    expect(config).toContain("<h3>配置</h3>");
    expect(config).not.toContain("作业（");
    expect(markup.indexOf("作业（")).toBeLessThan(configIndex);
    expect(markup.indexOf("</aside>")).toBeGreaterThan(configIndex);
  });

  it("任务抽屉不再传入 defaultOpen，Agent 池使用执行者角色选项卡", () => {
    expect(projectPageSource).not.toContain("defaultOpen");
    const poolCall = projectPageSource.slice(
      projectPageSource.indexOf("<TaskAgentPoolPanel"),
    );
    expect(poolCall).toContain('activeRole="executor"');
  });

  it("Agent 池由布局尾部最后一项渲染", () => {
    expect(projectPageSource).toContain(
      "{layout.tail.map((key) => tailBlock(key))}",
    );
    const jobsCase = projectPageSource.indexOf('case "jobs":');
    const poolCase = projectPageSource.indexOf('case "agentPool":');
    expect(jobsCase).toBeGreaterThan(0);
    expect(poolCase).toBeGreaterThan(jobsCase);
    expect(projectPageSource.slice(poolCase)).toContain(
      'className="drawer-config"',
    );
  });
});

describe("TaskDrawer 作业记录", () => {
  it("默认只展示最近若干条，并可展开全部", () => {
    const jobs = Array.from({ length: RECENT_JOB_LIMIT + 2 }, (_, index) => job(index));
    const markup = renderDrawer(task(), jobs);
    expect((markup.match(/class="job"/g) ?? []).length).toBe(RECENT_JOB_LIMIT);
    expect(markup).toContain("展开全部（还有 2 条）");
  });

  it("作业条数不超过上限时不出现展开全部", () => {
    const jobs = Array.from({ length: RECENT_JOB_LIMIT }, (_, index) => job(index));
    const markup = renderDrawer(task(), jobs);
    expect((markup.match(/class="job"/g) ?? []).length).toBe(RECENT_JOB_LIMIT);
    expect(markup).not.toContain("展开全部");
  });

  it("提示词默认收起", () => {
    const markup = renderToStaticMarkup(<JobPrompt value={"PROMPT_LINE"} />);
    const details = markup.slice(0, markup.indexOf(">"));
    expect(details).toContain('class="job-block job-prompt drawer-fold"');
    expect(details).not.toContain("open");
    expect(markup).toContain("PROMPT_LINE");
  });
});
