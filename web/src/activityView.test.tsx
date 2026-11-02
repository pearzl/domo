import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { JobActivity, JobActivityItem } from "./types";
import { ActivityStream } from "./components/JobActivity";
import { LiveActivitySummary } from "./pages/ProjectPage";
import {
  ACTIVITY_WAITING_TEXT,
  activityHeadline,
  activityProgressText,
} from "./presentation/activity";

function item(overrides: Partial<JobActivityItem> = {}): JobActivityItem {
  return {
    kind: "message",
    text: null,
    tool: null,
    target: null,
    status: null,
    error: null,
    ...overrides,
  };
}

function reasoning(text: string): JobActivityItem {
  return item({ kind: "reasoning", text });
}

function tool(
  name: string,
  status: string,
  target: string | null,
  error: string | null = null,
): JobActivityItem {
  return item({ kind: "tool", tool: name, status, target, error });
}

function activity(items: JobActivityItem[]): JobActivity {
  return {
    format: "opencode",
    items,
    bytes: 512,
    summary: {
      messages: items.filter((entry) => entry.kind === "message").length,
      reasoning: items.filter((entry) => entry.kind === "reasoning").length,
      tool_calls: items.filter((entry) => entry.kind === "tool").length,
      tool_failures: items.filter(
        (entry) => entry.kind === "tool" && Boolean(entry.error),
      ).length,
      steps: items.filter((entry) => entry.kind === "step").length,
      truncated: false,
    },
  };
}

describe("活动面板：思维链主视图", () => {
  const sample = activity([
    reasoning("先梳理目标与边界"),
    tool("bash", "error", "npm test", "exit 1"),
    reasoning("## 下一步\n\n检查依赖关系"),
  ]);

  it("思维链条目各自渲染为独立元素并默认展开", () => {
    const markup = renderToStaticMarkup(
      <ActivityStream
        activity={sample}
        running={false}
        now={1_000_000}
        lastUpdatedAt={null}
      />,
    );
    expect((markup.match(/data-activity-kind="reasoning"/g) ?? []).length).toBe(2);
    expect(markup).toContain("先梳理目标与边界");
    expect(markup).toContain("检查依赖关系");
    expect(markup).toContain('class="activity-reasoning" open');
  });

  it("工具调用合并为可折叠分组且失败保持高亮", () => {
    const markup = renderToStaticMarkup(
      <ActivityStream
        activity={sample}
        running={false}
        now={1_000_000}
        lastUpdatedAt={null}
      />,
    );
    expect(markup).toContain("activity-compact");
    expect(markup).toContain("1 失败");
    expect(markup).toContain("activity-tool-failed");
  });

  it("页面标记中不出现原始输出入口", () => {
    const markup = renderToStaticMarkup(
      <ActivityStream
        activity={sample}
        running={false}
        now={1_000_000}
        lastUpdatedAt={null}
      />,
    );
    expect(markup).not.toContain("原始输出");
    expect(markup).not.toContain("live-output");
  });

  it("上滚后出现“回到最新”，跟随时不出现", () => {
    const follow = () => {};
    const following = renderToStaticMarkup(
      <ActivityStream
        activity={sample}
        running
        following
        now={1_000_000}
        lastUpdatedAt={null}
        onFollow={follow}
      />,
    );
    const scrolledUp = renderToStaticMarkup(
      <ActivityStream
        activity={sample}
        running
        following={false}
        now={1_000_000}
        lastUpdatedAt={null}
        onFollow={follow}
      />,
    );
    expect(following).not.toContain("回到最新");
    expect(scrolledUp).toContain("回到最新");
  });

  it("头部展示最近更新时间而非整秒截断的已运行时间", () => {
    const markup = renderToStaticMarkup(
      <ActivityStream
        activity={sample}
        running
        now={1_004_000}
        lastUpdatedAt={1_000_000}
      />,
    );
    expect(markup).toContain("最近更新");
    expect(markup).toContain("4 秒前");
    expect(markup).not.toContain("已运行");
  });
});

describe("实时进展区：思维链主视图", () => {
  it("渲染思维链且不提供原始输出入口", () => {
    const markup = renderToStaticMarkup(
      <LiveActivitySummary
        activity={activity([reasoning("正在梳理项目结构")])}
        running
        now={1_000_000}
        lastUpdatedAt={1_000_000}
        loading={false}
        error=""
      />,
    );
    expect(markup).toContain("规划者（实时进展）");
    expect(markup).toContain("正在梳理项目结构");
    expect(markup).toContain("最近更新");
    expect(markup).not.toContain("原始输出");
  });

  it("没有活动时展示等待文案", () => {
    const markup = renderToStaticMarkup(
      <LiveActivitySummary
        activity={activity([])}
        running
        now={1_000_000}
        lastUpdatedAt={null}
        loading={false}
        error=""
      />,
    );
    expect(markup).toContain(ACTIVITY_WAITING_TEXT);
  });
});

describe("思维链摘要规则", () => {
  it("取首个有内容的行并标记为思考", () => {
    expect(activityHeadline(reasoning("## 先梳理目标\n\n然后拆解任务"))).toBe(
      "思考 · 先梳理目标",
    );
    expect(activityHeadline(reasoning(JSON.stringify({ step: 1 })))).not.toContain(
      "{",
    );
    expect(activityHeadline(reasoning("   "))).toBe("");
  });

  it("进展文案优先最新思维链", () => {
    expect(activityProgressText([reasoning("检查依赖"), tool("read", "completed", "a.ts")])).toBe(
      "思考 · 检查依赖",
    );
  });
});
