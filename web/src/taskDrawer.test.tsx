import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { JobSummary, ProjectDetail, Task } from "./types";
import { TaskDrawer } from "./pages/ProjectPage";
import projectPageSource from "./components/TaskDrawer.tsx?raw";
import { JobPrompt, RECENT_JOB_LIMIT } from "./components/JobDetailView";
import {
  DRAWER_ACCEPTANCE_TITLE,
  DRAWER_CONTENT_SECTION_CLASS,
  DRAWER_DESCRIPTION_TITLE,
  DRAWER_FEEDBACK_TITLE,
} from "./presentation/taskDrawer";

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
      onSaveGuidance={async () => true}
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

function verificationMarkup(markup: string) {
  const start = markup.indexOf('data-drawer-block="verification"');
  if (start < 0) return "";
  return markup.slice(start, markup.indexOf("</div>", markup.indexOf("verification-note", start)) + 6);
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
    const drawer = markup.slice(leadStart(markup));
    expect(drawer).not.toContain("raw-error drawer-fold");
    // 失败原因只在 gate 面板承载，验收摘要不回退重复同一条原文。
    expect(markup).not.toContain(RAW);
    expect(verificationMarkup(markup)).not.toContain(RAW);
  });

  it("里程碑验收时原始错误只由 gate 面板承载，全文只出现一次", () => {
    const markup = renderDrawer(
      task({ stage: "completed", human_gate: "milestone", last_error: RAW }),
    );
    expect((markup.match(new RegExp(RAW, "g")) ?? []).length).toBe(1);
    expect(leadMarkup(markup)).toContain(RAW);
    expect(verificationMarkup(markup)).not.toContain(RAW);
  });

  it("任务描述与验收标准同级并列、使用同一容器类，不再有单独的“验收”标题", () => {
    const markup = renderDrawer(task({ verifier_issues: ["FEEDBACK_MARKER"] }));
    expect((markup.match(/<h3>验收<\/h3>/g) ?? []).length).toBe(0);
    expect((markup.match(/<h4>/g) ?? []).length).toBe(0);
    const sections = Array.from(
      markup.matchAll(/<section class="([^"]+)" data-drawer-block="(description|acceptance)">([\s\S]*?)<\/section>/g),
    );
    expect(sections.map((match) => match[2])).toEqual(["description", "acceptance"]);
    expect(sections.map((match) => match[1])).toEqual([
      DRAWER_CONTENT_SECTION_CLASS,
      DRAWER_CONTENT_SECTION_CLASS,
    ]);
    const [description, acceptance] = sections;
    expect(description[3]).toContain(`<h3>${DRAWER_DESCRIPTION_TITLE}</h3>`);
    expect(acceptance[3]).toContain(`<h3>${DRAWER_ACCEPTANCE_TITLE}</h3>`);
    // 两者正文都走同一个 Markdown 渲染器与同一套排版类。
    for (const match of sections) {
      expect(match[3]).toContain('class="rich-content rich-content-markdown"');
      expect(match[3]).toContain('class="rich-markdown"');
    }
    expect(acceptance[3]).toContain("<li>验收项一</li>");
    expect(markup).toContain("FEEDBACK_MARKER");
  });

  it("验收标准不再嵌套在其它区块内，且与描述相邻", () => {
    const markup = renderDrawer(task());
    const descriptionIndex = markup.indexOf("data-drawer-block=\"description\"");
    const acceptanceIndex = markup.indexOf("data-drawer-block=\"acceptance\"");
    expect(descriptionIndex).toBeGreaterThan(0);
    expect(acceptanceIndex).toBeGreaterThan(descriptionIndex);
    const between = markup.slice(descriptionIndex, acceptanceIndex);
    expect(between).toContain("</section></div>");
    expect((markup.match(/验收标准/g) ?? []).length).toBe(1);
  });

  it("验收反馈单独成区块，只在存在反馈时展示", () => {
    const withFeedback = renderDrawer(task({ verifier_issues: ["FEEDBACK_MARKER"] }));
    expect((withFeedback.match(new RegExp(`<h3>${DRAWER_FEEDBACK_TITLE}</h3>`, "g")) ?? []).length).toBe(1);
    expect(withFeedback).toContain('data-drawer-block="feedback"');
    expect(withFeedback).toContain("FEEDBACK_MARKER");

    for (const issues of [[], ["  ", ""]]) {
      const withoutFeedback = renderDrawer(task({ verifier_issues: issues }));
      expect(withoutFeedback).not.toContain(DRAWER_FEEDBACK_TITLE);
      expect(withoutFeedback).not.toContain('data-drawer-block="feedback"');
    }
  });

  it("依赖与位置：关键字段常显，其余字段折叠在默认收起的 details 中", () => {
    const markup = renderDrawer(task({ deps: ["T0"], branch: "domo/t1", worktree: "/ws/t1" }));
    const start = markup.indexOf('class="task-deps');
    expect(start).toBeGreaterThan(0);
    const section = markup.slice(start, markup.indexOf("</section>", start));
    const primary = section.slice(0, section.indexOf('<details class="drawer-fold deps-fold">'));
    for (const label of ["依赖", "排队位置", "等待原因"]) {
      expect(primary).toContain(`<span>${label}</span>`);
    }
    for (const label of ["分支", "工作区", "失败分类", "Agent"]) {
      expect(primary).not.toContain(`<span>${label}</span>`);
    }
    const detailsTag = section.slice(
      section.indexOf('<details class="drawer-fold deps-fold">'),
      section.indexOf(">", section.indexOf('<details class="drawer-fold deps-fold">')),
    );
    expect(detailsTag).not.toContain("open");
    const folded = section.slice(section.indexOf("更多位置信息"));
    for (const label of ["分支", "工作区", "失败分类", "Agent"]) {
      expect(folded).toContain(`<span>${label}</span>`);
    }
    expect(folded).toContain("/ws/t1");
    expect(primary).toContain("T0");
  });

  it("依赖与位置：归档后才出现的字段也落在折叠区", () => {
    const markup = renderDrawer(
      task({ branch: "domo/t1", artifacts_released_at: "2026-01-11T00:00:00Z" }),
    );
    const start = markup.indexOf('class="task-deps');
    const section = markup.slice(start, markup.indexOf("</section>", start));
    const folded = section.slice(section.indexOf("更多位置信息"));
    expect(folded).toContain("归档 ref");
    expect(folded).toContain("恢复命令");
  });

  it("验收摘要紧跟验收标准，无验收作业时显示未验收而非空区块", () => {
    const markup = renderDrawer(task());
    const acceptanceIndex = markup.indexOf('data-drawer-block="acceptance"');
    const verificationIndex = markup.indexOf('data-drawer-block="verification"');
    expect(verificationIndex).toBeGreaterThan(acceptanceIndex);
    const section = verificationMarkup(markup);
    expect(section).toContain("<h3>验收摘要</h3>");
    expect(section).toContain(">未验收<");
    expect(section).toContain("验收 0 次 · 拒绝 0 次");
    // 回退来源：验收记录尚未读出时标注来源与回退依据。
    expect(section).toContain("来源：");
    expect(section).toContain("还没有验收作业产出结论");
  });

  it("已完成且有验收反馈的任务渲染拒绝结论、原因与问题", () => {
    const markup = renderDrawer(
      task({
        stage: "executing",
        attempts: 2,
        rejections: 1,
        verifier_issues: ["ISSUE_MARKER"],
        last_error: "REASON_MARKER",
      }),
    );
    const section = verificationMarkup(markup);
    expect(section).toContain(">拒绝<");
    expect(section).toContain("ISSUE_MARKER");
    expect(section).toContain("验收 2 次 · 拒绝 1 次");
    // 服务端首渲染时验收记录尚未读出，来源标注为加载中；读取失败后同一区块标注回退来源。
    expect(section).toContain("来源：验收记录（加载中）");
    expect(section).toContain("data-source=\"task\"");
    expect((markup.match(/ISSUE_MARKER/g) ?? []).length).toBe(2);
  });

  it("完成且执行过的任务在验收记录不可用时按任务字段显示通过", () => {
    const markup = renderDrawer(task({ stage: "completed", attempts: 1 }));
    expect(verificationMarkup(markup)).toContain(">通过<");
  });

  it("结果摘要按 Markdown 排版并标注来源作业，没有结果时给出空状态", () => {
    const markup = renderDrawer(
      task({
        stage: "completed",
        result_summary: "## 完成内容\n\n- 第一项\n- 第二项",
      }),
      [job(1)],
    );
    const result = markup.slice(
      markup.indexOf('data-drawer-block="result"'),
      markup.indexOf("</section>", markup.indexOf('data-drawer-block="result"')),
    );
    expect(result).toContain("<h2");
    expect(result).toContain("<ul");
    expect(result).toContain("<li>第一项</li>");
    expect(result).toContain('class="rich-content rich-content-markdown"');
    expect(result).toContain("来自第 1 次执行作业");
    expect(result).not.toContain("## 完成内容");

    const empty = renderDrawer(task({ stage: "completed" }), [job(1)]);
    const emptyResult = empty.slice(
      empty.indexOf('data-drawer-block="result"'),
      empty.indexOf("</section>", empty.indexOf('data-drawer-block="result"')),
    );
    expect(emptyResult).toContain("rich-content-empty");
    expect(emptyResult).toContain("执行作业产出结果后");
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
