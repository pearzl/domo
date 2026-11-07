import { createElement } from "react";
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type {
  AnalysisProposal,
  AuditEvent,
  JobSummary,
  Message,
  Project,
  ProjectDetail,
  Task,
  UsageTotals,
} from "../types";
import { ProjectOverview } from "../components/ProjectOverview";
import { ProjectTopBlocks } from "../components/ProjectTopBlocks";
import {
  PROJECT_BLOCK_KEYS,
  PROJECT_TOP_BLOCK_KEYS,
  projectBlockOrder,
  projectTabBadge,
  resolveProjectTab,
  tabDirectionForKey,
  nextProjectTab,
  type ProjectBlockKey,
} from "./projectLayout";
import projectPageSource from "../pages/ProjectPage.tsx?raw";
import projectHeaderSource from "../components/ProjectHeader.tsx?raw";
import overviewSource from "../components/ProjectOverview.tsx?raw";
import usageSummarySource from "../components/UsageSummary.tsx?raw";

const STAGES = ["analysis", "executing", "summarizing", "completed"] as const;

describe("projectBlockOrder 阶段顺序", () => {
  it("所有阶段默认首屏都是概览", () => {
    for (const stage of [...STAGES, null, undefined, "unknown"] as const) {
      const order = projectBlockOrder(stage);
      expect(order[0]).toBe("overview");
      expect(order.at(-1)).toBe("info");
      expect(resolveProjectTab(stage, null)).toBe("overview");
    }
  });

  it("阶段顺序保留分析、执行与报告的工作路径", () => {
    expect(projectBlockOrder("analysis").slice(1, 4)).toEqual([
      "analysis",
      "tasks",
      "report",
    ]);
    expect(projectBlockOrder("executing").slice(1, 4)).toEqual([
      "tasks",
      "analysis",
      "report",
    ]);
    expect(projectBlockOrder("summarizing").slice(1, 4)).toEqual([
      "report",
      "tasks",
      "analysis",
    ]);
  });

  it("阶段未知时回退到概览且不抛错", () => {
    for (const stage of [null, undefined, "", "archived", "running"]) {
      expect(() => projectBlockOrder(stage)).not.toThrow();
      expect(projectBlockOrder(stage)[0]).toBe("overview");
      expect(resolveProjectTab(stage, null)).toBe("overview");
    }
  });

  it("始终保留全部选项卡，且每个只出现一次", () => {
    for (const stage of STAGES) {
      const order = projectBlockOrder(stage);
      expect([...order].sort()).toEqual([...PROJECT_BLOCK_KEYS].sort());
    }
    for (const stage of [null, undefined, "unknown"]) {
      const order = projectBlockOrder(stage);
      expect(order).toHaveLength(PROJECT_BLOCK_KEYS.length);
      expect(new Set(order).size).toBe(PROJECT_BLOCK_KEYS.length);
    }
  });

  it("URL 中的失效选项卡回退到阶段默认值", () => {
    expect(resolveProjectTab("analysis", "overview")).toBe("overview");
    expect(resolveProjectTab("analysis", "unknown")).toBe("overview");
    expect(resolveProjectTab("executing", "report")).toBe("report");
  });

  it("返回的数组可安全修改，不影响下一次调用", () => {
    const first = projectBlockOrder("executing");
    first.reverse();
    expect(projectBlockOrder("executing")[0]).toBe("overview");
  });
});

describe("projectTabBadge 选项卡徽标", () => {
  const input = { taskCount: 3, proposalPending: true, reportReady: true };

  it("任务显示数量、分析与报告显示标记", () => {
    expect(projectTabBadge("tasks", input)).toBe("任务 3");
    expect(projectTabBadge("analysis", input)).toBe("待确认方案");
    expect(projectTabBadge("report", input)).toBe("报告就绪");
  });

  it("没有内容时不显示徽标", () => {
    const empty = { taskCount: 0, proposalPending: false, reportReady: false };
    expect(projectTabBadge("tasks", empty)).toBeNull();
    expect(projectTabBadge("analysis", empty)).toBeNull();
    expect(projectTabBadge("report", empty)).toBeNull();
    expect(projectTabBadge("overview", input)).toBeNull();
    expect(
      projectTabBadge("tasks", { ...empty, taskCount: null }),
    ).toBeNull();
  });
});

describe("选项卡键盘方向", () => {
  const order = projectBlockOrder("analysis");

  it("左右方向键在顺序内移动并在两端循环", () => {
    expect(nextProjectTab(order, "analysis", "right")).toBe("tasks");
    expect(nextProjectTab(order, "overview", "left")).toBe("info");
    expect(nextProjectTab(order, "info", "right")).toBe("overview");
  });

  it("Home 与 End 直达首尾", () => {
    expect(nextProjectTab(order, "report", "first")).toBe("overview");
    expect(nextProjectTab(order, "report", "last")).toBe("info");
  });

  it("只识别方向键与 Home/End", () => {
    expect(tabDirectionForKey("ArrowLeft")).toBe("left");
    expect(tabDirectionForKey("ArrowRight")).toBe("right");
    expect(tabDirectionForKey("Home")).toBe("first");
    expect(tabDirectionForKey("End")).toBe("last");
    expect(tabDirectionForKey("Enter")).toBeNull();
    expect(tabDirectionForKey(" ")).toBeNull();
  });
});

describe("项目详情页区块布局", () => {
  it("主体区块按 projectBlockOrder 渲染为选项卡与 tabpanel", () => {
    expect(projectPageSource).toContain("projectBlockOrder(project.stage)");
    expect(projectPageSource).toContain("<ProjectTabs");
    expect(projectPageSource).toContain('role="tabpanel"');
    expect(projectPageSource).toContain('aria-labelledby={tabButtonId(activeBlock)}');
    expect(projectPageSource).not.toContain("TAB_LABELS");
  });

  it("概览从选项卡面板渲染，不再作为页面固定区块", () => {
    const header = projectPageSource.indexOf("<ProjectHeader");
    const tabs = projectPageSource.indexOf("<ProjectTabs");
    const overview = projectPageSource.indexOf("<ProjectOverview");
    expect(header).toBeGreaterThan(0);
    expect(tabs).toBeGreaterThan(header);
    expect(overview).toBeGreaterThan(tabs);
  });

  it("项目 Agent 池与活动审计交给顶层区块，且排在选项卡之后", () => {
    const tabs = projectPageSource.indexOf("<ProjectTabs");
    const top = projectPageSource.indexOf("<ProjectTopBlocks");
    expect(top).toBeGreaterThan(tabs);
    expect(PROJECT_TOP_BLOCK_KEYS).toEqual(["agents", "activity"]);
  });

  it("页头交给 ProjectHeader，并展示阶段、状态、进度与运行作业数", () => {
    expect(projectPageSource).toContain("<ProjectHeader");
    expect(projectHeaderSource).toContain("<ProjectStatusBar");
    expect(projectHeaderSource).toContain("任务进度");
    expect(projectHeaderSource).toContain("运行作业");
  });

  it("项目源信息默认收起，并提供逐项复制入口", () => {
    const start = projectHeaderSource.indexOf(
      '<details className="project-source">',
    );
    expect(start).toBeGreaterThan(0);
    const openTag = projectHeaderSource.slice(
      start,
      projectHeaderSource.indexOf(">", start),
    );
    expect(openTag).not.toContain("open");
    expect(projectHeaderSource).toContain("<CopyButton");
  });

  it("失败作业深链在信息页展开并定位目标记录", () => {
    expect(projectPageSource).toContain("revealJobId={selectedJobId}");
    expect(projectPageSource).toContain(
      "focusNonce={job.id === selectedJobId ? 1 : 0}",
    );
    expect(projectPageSource).toContain("selectTab(tab, jobId ?? null)");
  });

  it("项目作业区使用默认最近记录与展开入口", () => {
    const start = projectPageSource.indexOf("项目分析 / 直接完成 / 总结作业");
    const section = projectPageSource.slice(
      start,
      projectPageSource.indexOf(") : null}", start),
    );
    expect(start).toBeGreaterThan(0);
    expect(section).toContain("<JobRecordList");
    expect(section).not.toContain("limit={0}");
  });
});

function project(stage: string, overrides: Partial<Project> = {}): Project {
  return {
    id: "p1",
    name: "示例项目",
    goal: "交付示例",
    repo_path: "/repo",
    base_branch: "main",
    branch: "domo/p1",
    workspace: "/ws",
    stage,
    status: null,
    blocked_reason: null,
    report: null,
    report_feedback: null,
    replan_reason: null,
    replan_source: null,
    replan_requested_at: null,
    confirm_mode: "manual",
    confirmed_at: null,
    merged_at: null,
    merged_commit: null,
    created_at: "2026-01-10T10:00:00Z",
    updated_at: "2026-01-10T10:00:00Z",
    ...overrides,
  };
}

function task(index: number, overrides: Partial<Task> = {}): Task {
  return {
    id: `t${index}`,
    project_id: "p1",
    seq: index,
    queue_order: index,
    queue_position: index,
    key: `T${index}`,
    title: `任务 ${index}`,
    description: "",
    acceptance: [],
    deps: [],
    parent_id: null,
    milestone: false,
    human_gate: null,
    status: null,
    stage: "pending",
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
  } as unknown as Task;
}

function job(status: string, overrides: Partial<JobSummary> = {}): JobSummary {
  return {
    id: `job-${status}`,
    project_id: "p1",
    task_id: null,
    role: "analysis",
    agent_id: null,
    agent_name: "规划 Agent",
    attempt: 1,
    status,
    exit_code: null,
    error: null,
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
    started_at: "2026-01-10T10:00:00Z",
    finished_at: null,
    created_at: "2026-01-10T10:00:00Z",
    ...overrides,
  } as unknown as JobSummary;
}

function event(id: number, kind: string, message: string): AuditEvent {
  return {
    id,
    project_id: "p1",
    task_id: null,
    job_id: null,
    kind,
    message,
    message_length: message.length,
    message_truncated: false,
    created_at: "2026-01-10T10:05:00Z",
  };
}

function message(id: string, role: string, content: string): Message {
  return {
    id,
    project_id: "p1",
    role,
    content,
    created_at: "2026-01-10T10:00:00Z",
  } as unknown as Message;
}

function proposal(): AnalysisProposal {
  return {
    kind: "plan",
    summary: "拆成两个任务",
    goal: null,
    assumptions: [],
    acceptance: [],
    tasks: [],
  };
}

function detail(overrides: Partial<ProjectDetail> = {}): ProjectDetail {
  return {
    attention_items: [],
    project: project("executing"),
    tasks: [],
    jobs: [],
    messages: [],
    events: [
      event(1, "task_failed", "T1 验收失败，需要人工裁决"),
      event(2, "project_agents_updated", "项目 Agent 池已更新"),
    ],
    agents: [],
    agent_pool: { project_id: "p1", roles: [] },
    proposal: null,
    usage: {
      totals: {
        jobs: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        cost: 0,
        duration_ms: 0,
        known_jobs: 0,
        unknown_jobs: 0,
        known_metrics: { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0, cost: 0 },
      },
      by_agent: [],
      by_role: [],
    },
    ...overrides,
  } as unknown as ProjectDetail;
}

function usageTotals(overrides: Partial<UsageTotals> = {}): UsageTotals {
  const known = overrides.known_jobs ?? 0;
  return {
    jobs: 0,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    reasoning_tokens: 0,
    cost: 0,
    duration_ms: 0,
    known_jobs: 0,
    unknown_jobs: 0,
    known_metrics: { input_tokens: known, output_tokens: known, cache_read_tokens: known, cache_write_tokens: known, reasoning_tokens: known, cost: known },
    ...overrides,
  };
}

function withUsage(overrides: Partial<UsageTotals>): ProjectDetail {
  const totals = usageTotals(overrides);
  return detail({ usage: { totals, by_agent: [], by_role: [] } });
}

function renderOverview(input: ProjectDetail): string {
  return renderToStaticMarkup(
    createElement(ProjectOverview, { detail: input }),
  );
}

describe("ProjectOverview 当前需要你处理", () => {
  it("不再出现“现在在做什么”，运行中作业与排队任务并入待办摘要", () => {
    const markup = renderOverview(
      detail({
        project: project("executing"),
        tasks: [
          task(1, { stage: "executing", status: "running" }),
          task(2),
        ],
        jobs: [job("running")],
      }),
    );
    expect(markup).not.toContain("现在在做什么");
    expect(markup).toContain("当前进展");
    expect(markup).toContain("运行中作业");
    expect(markup).not.toContain("排队任务");
    expect(markup).toContain("待启动");
    expect(markup).toContain("T2");
    expect(markup).toContain("最近关键事件");
    expect(markup).toContain("T1 验收失败，需要人工裁决");
    expect(markup).not.toContain("项目 Agent 池已更新");
  });

  it("分析阶段给出“去确认方案”跳转", () => {
    const markup = renderOverview(
      detail({
        project: project("analysis"),
        proposal: proposal(),
      }),
    );
    expect(markup).toContain("去确认方案");
    expect(markup).not.toContain("现在在做什么");
    // 跳转按钮只在提供回调时可用，不提供回调时仍可渲染但点击不切换。
    const withCallback = renderToStaticMarkup(
      createElement(ProjectOverview, {
        detail: detail({ project: project("analysis"), proposal: proposal() }),
        onOpenTab: () => {},
      }),
    );
    expect(withCallback).toContain("去确认方案");
  });

  it("报告阶段给出“去验收报告”跳转", () => {
    const markup = renderOverview(
      detail({
        project: project("summarizing", { report: "# 报告" }),
      }),
    );
    expect(markup).toContain("去验收报告");
    expect(markup).not.toContain("现在在做什么");
    const withCallback = renderToStaticMarkup(
      createElement(ProjectOverview, {
        detail: detail({ project: project("summarizing", { report: "# 报告" }) }),
        onOpenTab: () => {},
      }),
    );
    expect(withCallback).toContain("去验收报告");
  });

  it("项目级失败显示处置说明与查看失败作业入口", () => {
    const markup = renderOverview(
      detail({
        project: project("analysis", {
          status: "blocked",
          blocked_reason: "规划服务不可用",
        }),
        attention_items: [{ id: "p1", kind: "failed", task: null, reason: "规划服务不可用",
          since: "2026-01-10T10:00:00Z", affected_tasks: 0, blocks_milestone: false,
          blocks_summary: true, project_advancing: false }],
        jobs: [job("failed", { id: "job-failed" })],
      }),
    );
    expect(markup).toContain("当前需要你处理");
    expect(markup).toContain("处理失败作业");
    expect(markup).toContain("查看失败作业");
  });

  it("待合入显示审阅报告与合入入口，已交付不显示待办", () => {
    const pending = renderOverview(detail({ project: project("completed") }));
    expect(pending).toContain("审阅项目报告");
    expect(pending).toContain("合入项目");

    const delivered = renderOverview(
      detail({
        project: project("completed", { merged_at: "2026-01-10T11:00:00Z" }),
      }),
    );
    expect(delivered).toContain("当前进展");
    expect(delivered).not.toContain("合入项目");
    expect(delivered).not.toContain("当前需要你处理");
  });

  it("保留状态与任务、项目用时统计，不渲染术语卡片", () => {
    const markup = renderOverview(detail());
    expect(markup).not.toContain("术语说明");
    expect(markup).toContain("项目概览");
    expect(markup).toContain("任务完成");
    expect(markup).toContain("项目用时");
  });

  it("统计区与用量摘要并排于待办下方，作业数与用量覆盖各只出现一次", () => {
    const markup = renderOverview(
      withUsage({
        jobs: 4,
        input_tokens: 12000,
        output_tokens: 3400,
        cost: 0.42,
        duration_ms: 65000,
        known_jobs: 3,
        unknown_jobs: 1,
      }),
    );
    expect(markup).toContain('class="overview-stats"');
    expect(markup).toContain('data-block="usage-summary"');
    expect(markup).toContain("当前进展");
    // 待办占整行在上，统计与用量摘要同处其下。
    const todo = markup.indexOf('class="overview-todo');
    const stats = markup.indexOf('class="overview-stats"');
    const usage = markup.indexOf('data-block="usage-summary"');
    expect(todo).toBeGreaterThan(0);
    expect(stats).toBeGreaterThan(todo);
    expect(usage).toBeGreaterThan(todo);
    // 作业数与用量记录覆盖只由用量摘要给出。
    const usageSummary = markup.slice(usage, markup.indexOf("</section>", usage));
    expect(usageSummary).toContain("作业数");
    expect(usageSummary).toContain("用量记录覆盖 3/4");
    for (const label of ["作业数", "用量记录覆盖"]) {
      const total = (markup.match(new RegExp(label, "g")) ?? []).length;
      const inside =
        (usageSummary.match(new RegExp(label, "g")) ?? []).length;
      expect(inside, `${label} 应出现在用量摘要内`).toBeGreaterThan(0);
      expect(total, `${label} 在概览中重复出现`).toBe(inside);
    }
    // 统计区保留任务完成与项目用时，不再重复作业失败与用量未知说明。
    const statsBlock = markup.slice(
      stats,
      markup.indexOf('class="usage-summary"', stats),
    );
    expect(statsBlock).toContain("任务完成");
    expect(statsBlock).toContain("项目用时");
    expect(statsBlock).not.toContain("作业数");
    expect(statsBlock).not.toContain("用量记录覆盖");
    expect(overviewSource).not.toContain("无失败作业");
    expect(overviewSource).not.toContain("个作业用量未知");
    expect(usageSummarySource).toContain("作业数");
  });
});

describe("分析阶段无方案时的待办跳转", () => {
  it("等待回复与等待分析都指向项目分析选项卡", () => {
    const waiting = renderOverview(
      detail({
        project: project("analysis"),
        messages: [
          message("m1", "user", "开始分析"),
          message("m2", "planner", "需要支持哪些浏览器？"),
        ],
      }),
    );
    expect(waiting).toContain("去项目分析");
    expect(waiting).toContain("需要支持哪些浏览器？");
    expect(waiting).not.toContain("去确认方案");

    const idle = renderOverview(
      detail({
        project: project("analysis"),
        messages: [message("m1", "user", "开始分析")],
      }),
    );
    expect(idle).toContain("去项目分析");
  });
});

describe("概览内嵌用量摘要", () => {
  it("概览包含用量摘要与明细链接，页面不再有独立的项目用量面板", () => {
    const markup = renderOverview(
      withUsage({
        jobs: 4,
        input_tokens: 12000,
        output_tokens: 3400,
        cost: 0.42,
        duration_ms: 65000,
        known_jobs: 4,
      }),
    );
    expect(markup).toContain('data-block="usage-summary"');
    expect(markup).toContain("用量");
    expect(markup).toContain('href="/usage?project_id=p1"');
    expect(markup).toContain("查看用量明细");
    expect(markup).toContain("输入 token");
    expect(markup).toContain("12,000");
    expect(markup).toContain("3,400");
    expect(markup).toContain("0.4200");
    expect(markup).toContain("1 分 5 秒");
    expect(markup).toContain("用量记录覆盖 4/4");
    // 摘要与待办同处概览网格；待办保持“当前需要你处理”。
    expect(markup).toContain("当前进展");
    expect(markup.indexOf("当前需要你处理")).toBeLessThan(
      markup.indexOf('data-block="usage-summary"'),
    );
    // 独立的用量面板（标题与折叠块）不再出现在项目中。
    expect(markup).not.toContain("项目用量（");
    expect(projectPageSource).not.toContain("项目用量（");
    expect(projectPageSource).not.toContain("<UsagePanel");
  });

  it("完全不知道用量的项目显示“—”并附覆盖说明", () => {
    const markup = renderOverview(
      withUsage({
        jobs: 3,
        duration_ms: 90000,
        known_jobs: 0,
        unknown_jobs: 3,
      }),
    );
    expect(markup).toContain("—");
    expect(markup).toContain("用量记录覆盖 0/3");
    expect(markup).toContain("全部 3 个作业未记录用量");
    // 作业数与时长照常显示，不当作未知。
    expect(markup).toContain("3 个");
    expect(markup).toContain("1 分 30 秒");
  });

  it("部分覆盖时合计只含已记录部分并标注作业数", () => {
    const markup = renderOverview(
      withUsage({
        jobs: 4,
        input_tokens: 500,
        output_tokens: 60,
        cost: 0.1,
        duration_ms: 1000,
        known_jobs: 2,
        unknown_jobs: 2,
      }),
    );
    expect(markup).toContain("用量记录覆盖 2/4");
    expect(markup).toContain("已记录 2/4 个作业的用量");
    expect(markup.match(/aria-label="部分合计：已记录 2\/4 个作业"/g) ?? []).toHaveLength(3);
    expect(markup).not.toContain("全部 4 个作业未记录用量");
  });
});

/// 样式表不是 JS 模块，?raw 取到的是空串，直接读源文件原文。
const STYLES = readFileSync(
  new URL("../styles.css", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1"),
  "utf8",
);

describe("概览网格样式", () => {
  const styles = STYLES.replace(/\s+/g, " ");

  /// 取出 `.selector { … }` 的声明块，选择器需完整匹配。
  function block(selector: string): string {
    const start = styles.indexOf(`${selector} {`);
    expect(start, `缺少 ${selector} 规则`).toBeGreaterThanOrEqual(0);
    return styles.slice(start, styles.indexOf("}", start) + 1);
  }

  /// 媒体查询块内的文本，按大括号配对闭合。
  function media(maxWidth: number): string {
    const marker = `@media (max-width: ${maxWidth}px) {`;
    const start = styles.indexOf(marker);
    expect(start, `缺少 ${marker} 媒体查询`).toBeGreaterThanOrEqual(0);
    let depth = 0;
    for (let index = start + marker.length - 1; index < styles.length; index += 1) {
      if (styles[index] === "{") depth += 1;
      if (styles[index] === "}") {
        depth -= 1;
        if (depth === 0) return styles.slice(start, index + 1);
      }
    }
    return styles.slice(start);
  }

  it("宽屏待办占整行，统计与用量摘要同行等宽", () => {
    const grid = block(".overview-grid");
    expect(grid).toContain(
      'grid-template-areas: "todo todo" "stats usage";',
    );
    expect(grid).toContain(
      "grid-template-columns: repeat(2, minmax(0, 1fr));",
    );
    // 旧的统计列跨两行布局已删除：全文件 grid-template-areas 只剩概览两处与任务行一处。
    expect(styles.match(/grid-template-areas/g) ?? []).toHaveLength(3);
    expect(styles).not.toContain('"stats todo"');
    expect(styles).not.toContain('grid-template-areas: "stats usage"');
    // 统计卡宽屏 2×2 紧凑排布，用量卡片按小宽度两列自适应。
    expect(block(".overview-stats")).toContain(
      "grid-template-columns: repeat(2, minmax(0, 1fr));",
    );
    expect(block(".usage-summary-cards")).toContain(
      "repeat(auto-fit, minmax(80px, 1fr))",
    );
  });

  it("960px 及以下依次堆叠待办、用量摘要与统计区", () => {
    const narrow = media(960);
    expect(narrow).toContain('grid-template-areas: "todo" "usage" "stats";');
    expect(narrow).toContain("grid-template-columns: minmax(0, 1fr);");
    expect(narrow).toContain(".overview-stats { grid-template-columns: minmax(0, 1fr); }");
  });
});

describe("顶层区块集合", () => {
  it("项目 Agent 池与活动审计同处顶层，活动与审计默认折叠", () => {
    const markup = renderToStaticMarkup(
      createElement(ProjectTopBlocks, { detail: detail(), onRefresh: async () => {} }),
    );
    expect(markup).toContain('data-block="agents"');
    expect(markup).toContain('data-block="activity"');
    expect(markup).toContain("项目 Agent 池");
    expect((markup.match(/data-block="/g) ?? []).length).toBe(
      PROJECT_TOP_BLOCK_KEYS.length,
    );
    const activity = markup.slice(markup.indexOf('data-block="activity"'));
    expect(activity).toContain("<details>");
    expect(activity).not.toContain("<details open");
    expect(markup).not.toContain("项目设置");
  });
});

describe("选项卡键集合", () => {
  it("覆盖概览、项目分析、任务、项目报告与信息页", () => {
    const keys: ProjectBlockKey[] = [...PROJECT_BLOCK_KEYS];
    expect(keys).toEqual([
      "overview",
      "analysis",
      "tasks",
      "report",
      "info",
    ]);
  });
});
