import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { JobActivity, JobActivityItem, JobSummary } from "./types";
import {
  JobDetailView,
  JobPrompt,
  JobRecordList,
  RECENT_JOB_LIMIT,
  recentJobs,
} from "./components/JobDetailView";
import {
  ActivityStream,
  JobActivity as JobActivityView,
  RECENT_ACTIVITY_LIMIT,
  recentActivityItems,
} from "./components/JobActivity";
import { LiveActivitySummary } from "./pages/ProjectPage";
import {
  ACTIVITY_WAITING_TEXT,
  activityProgressText,
} from "./presentation/activity";

const STDOUT_MARKER = "STDOUT_MARKER_a1b2c3";
const STDERR_MARKER = "STDERR_MARKER_d4e5f6";

function job(index: number): JobSummary & { result?: string | null } {
  return {
    id: `job-${index}`,
    project_id: "p1",
    task_id: null,
    role: "analysis",
    agent_id: null,
    agent_name: `Agent ${index}`,
    attempt: 1,
    status: "finished",
    exit_code: 0,
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
    started_at: null,
    finished_at: null,
    created_at: "2026-01-10T10:00:00Z",
    result: `第 ${index} 次执行结果`,
  };
}

function jobs(count: number): JobSummary[] {
  return Array.from({ length: count }, (_, index) => job(index + 1));
}

function message(text: string): JobActivityItem {
  return {
    kind: "message",
    text,
    tool: null,
    target: null,
    status: null,
    error: null,
  };
}

function reasoning(text: string): JobActivityItem {
  return { kind: "reasoning", text, tool: null, target: null, status: null, error: null };
}

function tool(
  name: string,
  status: string,
  target: string | null = null,
  error: string | null = null,
): JobActivityItem {
  return { kind: "tool", text: null, tool: name, target, status, error };
}

function activity(items: JobActivityItem[]): JobActivity {
  return {
    format: "opencode",
    bytes: 42,
    items,
    summary: {
      messages: items.filter((entry) => entry.kind === "message").length,
      reasoning: items.filter((entry) => entry.kind === "reasoning").length,
      tool_calls: items.filter((entry) => entry.kind === "tool").length,
      tool_failures: items.filter((entry) => entry.kind === "tool" && entry.error)
        .length,
      steps: items.filter((entry) => entry.kind === "step").length,
      truncated: false,
    },
  };
}

describe("ActivityStream 思维链主视图", () => {
  const sample = activity([
    reasoning("先梳理目标与边界"),
    tool("read", "completed", "README.md"),
    {
      kind: "step",
      text: "准备工具",
      tool: null,
      target: null,
      status: null,
      error: null,
    },
    reasoning("## 下一步\n\n检查依赖关系"),
    message("开始实现"),
  ]);

  function render(overrides: Partial<Parameters<typeof ActivityStream>[0]> = {}) {
    return renderToStaticMarkup(
      <ActivityStream
        activity={sample}
        running={false}
        now={1_000_000}
        lastUpdatedAt={null}
        {...overrides}
      />,
    );
  }

  it("思维链条目渲染为独立元素且默认展开", () => {
    const markup = render();
    expect((markup.match(/data-activity-kind="reasoning"/g) ?? []).length).toBe(2);
    expect(markup).toContain("先梳理目标与边界");
    expect(markup).toContain("检查依赖关系");
    expect(markup).toContain('class="activity-reasoning" open');
    expect(markup).toContain("思维链");
  });

  it("工具与步骤合并为可折叠的紧凑分组并保留失败高亮", () => {
    const markup = render({
      activity: activity([
        tool("bash", "error", "npm test", "exit 1"),
        { kind: "step", text: "准备工具", tool: null, target: null, status: null, error: null },
        tool("read", "completed", "a.ts"),
      ]),
    });
    expect(markup).toContain('class="activity-tool-group activity-compact"');
    expect(markup).toContain("工具与步骤 ×3");
    expect(markup).toContain("1 失败");
    expect(markup).toContain("activity-tool-failed");
    expect(markup).toContain("npm test");
  });

  it("页面标记中不出现“原始输出”入口", () => {
    const markup = render();
    expect(markup).not.toContain("原始输出");
    expect(markup).not.toContain("live-output");
  });

  it("上滚后出现“回到最新”按钮，跟随时不出现", () => {
    const following = render({ following: true, onFollow: () => {} });
    expect(following).not.toContain("回到最新");
    const scrolledUp = render({ following: false, onFollow: () => {} });
    expect(scrolledUp).toContain("回到最新");
  });

  it("头部展示最近更新时间与相对时间，不使用已运行秒数", () => {
    const markup = render({ lastUpdatedAt: 1_000_000, now: 1_003_400 });
    expect(markup).toContain("最近更新");
    expect(markup).toContain("3 秒前");
    expect(markup).not.toContain("已运行");
  });

  it("运行中展示字符滚动的进展提示与状态点", () => {
    const markup = render({ running: true });
    expect(markup).toContain("activity-progress-track");
    expect(markup).toContain("进展 ·");
    expect(markup).toContain("activity-dot-live");
  });
});

describe("LiveActivitySummary 渲染", () => {
  function render(overrides: Partial<Parameters<typeof LiveActivitySummary>[0]> = {}) {
    return renderToStaticMarkup(
      <LiveActivitySummary
        jobId="job-live"
        activity={activity([reasoning("正在梳理项目结构")])}
        running
        now={1_000_000}
        lastUpdatedAt={1_000_000}
        loading={false}
        error=""
        {...overrides}
      />,
    );
  }

  it("以思维链为主视图且不提供原始输出入口", () => {
    const markup = render();
    expect(markup).toContain("规划者（实时进展）");
    expect(markup).toContain("正在梳理项目结构");
    expect(markup).toContain("思维链");
    expect(markup).not.toContain("原始输出");
  });

  it("没有活动时展示等待文案", () => {
    const markup = render({ activity: activity([]) });
    expect(markup).toContain(ACTIVITY_WAITING_TEXT);
  });
});

describe("活动进展文案", () => {
  it("优先取最新思维链，其次消息", () => {
    expect(
      activityProgressText([
        message("旧消息"),
        reasoning("先检查依赖"),
        tool("read", "completed", "a.ts"),
      ]),
    ).toBe("思考 · 先检查依赖");
    expect(activityProgressText([message("开始执行")])).toBe("开始执行");
    expect(activityProgressText([])).toBe("正在思考…");
  });
});

describe("记录可见切片", () => {
  it("recentJobs 默认只取最近若干条，展开后返回全部", () => {
    const all = jobs(8);
    expect(recentJobs(all, RECENT_JOB_LIMIT, false).map((item) => item.agent_name)).toEqual([
      "Agent 6",
      "Agent 7",
      "Agent 8",
    ]);
    expect(recentJobs(all, RECENT_JOB_LIMIT, true)).toHaveLength(8);
    expect(recentJobs(all, 0, false)).toHaveLength(8);
  });

  it("recentActivityItems 默认只取最近若干条，展开后返回全部", () => {
    const all = Array.from({ length: RECENT_ACTIVITY_LIMIT + 5 }, (_, index) => index);
    const shown = recentActivityItems(all, RECENT_ACTIVITY_LIMIT, false);
    expect(shown).toHaveLength(RECENT_ACTIVITY_LIMIT);
    expect(shown.at(-1)).toBe(all.length - 1);
    expect(recentActivityItems(all, RECENT_ACTIVITY_LIMIT, true)).toHaveLength(all.length);
  });
});

describe("JobActivityView 渲染", () => {
  it("不再提供原始输出入口，也不挂载 stdout / stderr", () => {
    const markup = renderToStaticMarkup(
      <JobActivityView jobId="job-1" running={false} />,
    );
    expect(markup).toContain("作业活动");
    expect(markup).not.toContain("原始输出");
    expect(markup).not.toContain("live-output");
    expect(markup).not.toContain(STDOUT_MARKER);
    expect(markup).not.toContain(STDERR_MARKER);
  });
});

describe("JobDetailView 折叠", () => {
  it("提示词位于默认收起的 details 中", () => {
    const markup = renderToStaticMarkup(<JobPrompt value={"第一行\n第二行"} />);
    expect(markup).toContain("<details");
    expect(markup).toContain("提示词");
    expect(markup).not.toContain("<details open");
    expect(markup).toContain("<summary>提示词（2 行）</summary>");
  });

  it("作业展开后结果与活动保持可达", () => {
    const markup = renderToStaticMarkup(
      <JobDetailView job={{ ...job(1), status: "running" }} />,
    );
    expect(markup).toContain("作业活动");
    expect(markup).not.toContain("原始输出");
  });
});

describe("JobRecordList 渲染", () => {
  function render(count: number) {
    return renderToStaticMarkup(
      <JobRecordList
        jobs={jobs(count)}
        renderJob={(item) => <div key={item.id}>{item.agent_name}</div>}
      />,
    );
  }

  it("默认只渲染最近若干条并提供展开全部", () => {
    const markup = render(8);
    expect((markup.match(/Agent \d+/g) ?? []).length).toBe(RECENT_JOB_LIMIT);
    expect(markup).toContain("展开全部");
    expect(markup).toContain(`还有 ${8 - RECENT_JOB_LIMIT} 条`);
  });

  it("不超过上限时不显示展开入口", () => {
    const markup = render(RECENT_JOB_LIMIT);
    expect((markup.match(/Agent \d+/g) ?? []).length).toBe(RECENT_JOB_LIMIT);
    expect(markup).not.toContain("展开全部");
  });

  it("空列表给出占位文案", () => {
    expect(render(0)).toContain("还没有作业");
  });
});
