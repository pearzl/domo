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
  JobActivity as JobActivityView,
  RECENT_ACTIVITY_LIMIT,
  recentActivityItems,
} from "./components/JobActivity";
import {
  LIVE_MESSAGE_LIMIT,
  LiveActivitySummary,
  liveActivityFeed,
  type LiveActivityFeed,
} from "./pages/ProjectPage";
import {
  ACTIVITY_WAITING_TEXT,
  activityDigest,
  isKeyActivity,
} from "./presentation/activity";

const STDOUT_MARKER = "STDOUT_MARKER_a1b2c3";
const STDERR_MARKER = "STDERR_MARKER_d4e5f6";

function job(index: number): JobSummary {
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
      tool_calls: items.filter((entry) => entry.kind === "tool").length,
      tool_failures: items.filter((entry) => entry.kind === "tool" && entry.error)
        .length,
      steps: items.filter((entry) => entry.kind === "step").length,
      truncated: false,
    },
  };
}

function emptyFeed(): LiveActivityFeed {
  return liveActivityFeed([]);
}

describe("实时区摘要（liveActivityFeed）", () => {
  const sample = activity([
    ...Array.from({ length: 6 }, (_, index) => message(`消息 ${index + 1}`)),
    tool("bash", "error", "npm test", "exit 1"),
    {
      kind: "step",
      text: "准备工具",
      tool: null,
      target: null,
      status: null,
      error: null,
    },
    tool("read", "completed", "a.ts"),
  ]);
  const items = activityDigest(sample, { limit: 0 }).items;

  it("有消息时以最新消息为摘要，并限制默认条数", () => {
    const collapsed = liveActivityFeed(items);
    expect(collapsed.headline).toBe("消息 6");
    expect(collapsed.messages).toHaveLength(LIVE_MESSAGE_LIMIT);
    expect(collapsed.messages.at(-1)?.text).toBe("消息 6");
    expect(collapsed.hidden).toBe(4);
  });

  it("只有工具时取工具摘要", () => {
    // 关键模式隐去成功工具，只有工具的活动需要直接看完整摘要。
    const onlyTools = activityDigest(
      activity([tool("read", "completed", "b.ts")]),
      { limit: 0 },
    ).items;
    const feed = liveActivityFeed(onlyTools);
    expect(feed.headline).toBe("read · b.ts");
    expect(feed.waiting).toBeNull();
    // 成功工具不属于关键状态，但摘要仍然可读。
    expect(feed.keys).toHaveLength(0);
    expect(feed.messages).toHaveLength(0);
  });

  it("空活动返回等待文案", () => {
    const feed = emptyFeed();
    expect(feed.headline).toBeNull();
    expect(feed.messages).toHaveLength(0);
    expect(feed.keys).toHaveLength(0);
    expect(feed.hidden).toBe(0);
    expect(feed.waiting).toBe(ACTIVITY_WAITING_TEXT);
  });

  it("关键状态只保留失败与进行中的工具", () => {
    const feed = liveActivityFeed(items, true);
    expect(feed.keys.map((entry) => entry.text)).toEqual(["bash · npm test · exit 1"]);
    // 步骤与成功工具明细不进入实时区，展开后仍被省略。
    expect(feed.hidden).toBe(2);
    expect(items.some((entry) => entry.text === "准备工具")).toBe(false);
    // 成功工具仍可作为可读消息保留，但不计入关键状态。
    expect(feed.keys.some((entry) => entry.tool === "read")).toBe(false);
    expect(feed.messages.every((entry) => entry.kind === "message")).toBe(true);
  });

  it("失败工具被关键模式保留", () => {
    expect(isKeyActivity(tool("bash", "error", "npm test"))).toBe(true);
    expect(isKeyActivity(tool("read", "completed", "a.ts"))).toBe(false);
  });

  it("展开后可见全部消息", () => {
    const expanded = liveActivityFeed(items, true);
    expect(expanded.messages).toHaveLength(6);
    expect(expanded.messages.at(0)?.text).toBe("消息 1");
    expect(expanded.hidden).toBe(2);
  });
});

describe("LiveActivitySummary 渲染", () => {
  function render(overrides: Partial<Parameters<typeof LiveActivitySummary>[0]> = {}) {
    return renderToStaticMarkup(
      <LiveActivitySummary
        feed={emptyFeed()}
        summary={null}
        loading={false}
        error=""
        rawOpen={false}
        onRawToggle={() => {}}
        onExpand={() => {}}
        jobId="job-1"
        {...overrides}
      />,
    );
  }

  it("展示可读摘要而默认不包含原始输出", () => {
    const items = activityDigest(
      activity([message("开始读取项目结构"), tool("bash", "error", "npm test", "exit 1")]),
      { limit: 0 },
    ).items;
    const markup = render({ feed: liveActivityFeed(items) });
    expect(markup).toContain("规划者（实时进展）");
    expect(markup).toContain("开始读取项目结构");
    expect(markup).toContain("bash · npm test · exit 1");
    expect(markup).toContain("<details");
    expect(markup).toContain("原始输出");
    expect(markup).not.toContain(STDOUT_MARKER);
    expect(markup).not.toContain(STDERR_MARKER);
    expect(markup).not.toContain("live-output");
  });

  it("没有活动时展示等待文案", () => {
    const markup = render();
    expect(markup).toContain(ACTIVITY_WAITING_TEXT);
  });

  it("被省略时提供展开全部入口", () => {
    const items = activityDigest(
      activity(Array.from({ length: 7 }, (_, index) => message(`m${index}`))),
      { limit: 0 },
    ).items;
    const markup = render({ feed: liveActivityFeed(items) });
    expect(markup).toContain("展开全部");
    expect(markup).toContain("还有 3 条");
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
  it("原始输出折叠默认收起，未展开时不出现 LiveOutput", () => {
    const markup = renderToStaticMarkup(
      <JobActivityView jobId="job-1" running={false} />,
    );
    expect(markup).toContain("原始输出");
    expect(markup).not.toContain("live-output");
    expect(markup).not.toContain(STDOUT_MARKER);
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
    expect(markup).toContain("原始输出");
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
