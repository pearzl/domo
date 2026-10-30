import { describe, expect, it } from "vitest";
import type { JobActivity, JobActivityItem } from "../types";
import {
  ACTIVITY_DIGEST_LIMIT,
  ACTIVITY_HEADLINE_MAX,
  ACTIVITY_WAITING_TEXT,
  activityDigest,
  activityHeadline,
  filterActivityItems,
  isKeyActivity,
  isToolFailure,
} from "./activity";

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

function message(text: string): JobActivityItem {
  return item({ kind: "message", text });
}

function tool(
  name: string,
  status: string | null = "completed",
  target: string | null = null,
  error: string | null = null,
): JobActivityItem {
  return item({ kind: "tool", tool: name, status, target, error });
}

function activity(items: JobActivityItem[]): JobActivity {
  return {
    format: "opencode",
    bytes: 1024,
    items,
    summary: {
      messages: items.filter((entry) => entry.kind === "message").length,
      tool_calls: items.filter((entry) => entry.kind === "tool").length,
      tool_failures: items.filter((entry) => isToolFailure(entry)).length,
      steps: items.filter((entry) => entry.kind === "step").length,
      truncated: false,
    },
  };
}

describe("activityHeadline", () => {
  it("有消息时取最新的消息文本", () => {
    const items = [
      message("先梳理目标"),
      tool("read", "completed", "README.md"),
      message("## 下一步\n\n开始实现实时区"),
    ];
    const digest = activityDigest(activity(items));
    const messages = digest.items.filter((entry) => entry.kind === "message");
    expect(messages.at(-1)?.text).toBe("下一步");
    expect(digest.items.at(-1)?.text).toBe("下一步");
  });

  it("只有工具时给出工具摘要", () => {
    const items = [
      tool("read", "completed", "web/src/pages/ProjectPage.tsx"),
      tool("grep", "completed", "LiveOutput"),
    ];
    const digest = activityDigest(activity(items));
    expect(digest.waiting).toBeNull();
    expect(digest.items.map((entry) => entry.text)).toEqual([
      "read · web/src/pages/ProjectPage.tsx",
      "grep · LiveOutput",
    ]);
    expect(digest.items.every((entry) => entry.kind === "tool")).toBe(true);
  });

  it("失败工具在摘要中包含错误说明", () => {
    const headline = activityHeadline(
      tool("bash", "error", "npm test", "命令失败：exit 1"),
    );
    expect(headline).toContain("bash");
    expect(headline).toContain("npm test");
    expect(headline).toContain("exit 1");
  });
});

describe("activityDigest", () => {
  it("空活动返回等待文案", () => {
    expect(activityDigest(null)).toEqual({
      items: [],
      hidden: 0,
      waiting: ACTIVITY_WAITING_TEXT,
    });
    expect(activityDigest(activity([])).waiting).toBe(ACTIVITY_WAITING_TEXT);
    expect(ACTIVITY_WAITING_TEXT).toContain("等待");
  });

  it("摘要长度受控", () => {
    const long = "x".repeat(500);
    const headline = activityHeadline(message(long));
    expect(Array.from(headline).length).toBeLessThanOrEqual(ACTIVITY_HEADLINE_MAX);
    expect(headline.endsWith("…")).toBe(true);
  });

  it("不输出原始 JSON 字符串", () => {
    const payload = JSON.stringify({ command: "npm test", output: "ok" }, null, 2);
    const headline = activityHeadline(tool("bash", "completed", payload));
    expect(headline).not.toContain("{");
    expect(headline).not.toContain("npm test");
    expect(headline).toContain("结构化内容");

    const messageHeadline = activityHeadline(message(`[1,2,3]`));
    expect(messageHeadline).not.toContain("[1,2,3]");
    expect(messageHeadline).toContain("结构化内容");
  });

  it("默认只取最近若干条并报告省略数量", () => {
    const items = Array.from({ length: ACTIVITY_DIGEST_LIMIT + 3 }, (_, index) =>
      message(`第 ${index + 1} 条消息`),
    );
    const digest = activityDigest(activity(items));
    expect(digest.items).toHaveLength(ACTIVITY_DIGEST_LIMIT);
    expect(digest.hidden).toBe(3);
    expect(digest.items.at(-1)?.text).toBe(`第 ${ACTIVITY_DIGEST_LIMIT + 3} 条消息`);
    expect(digest.items.at(-1)?.index).toBe(ACTIVITY_DIGEST_LIMIT + 2);
  });

  it("limit 为 0 时不截断", () => {
    const items = Array.from({ length: 10 }, (_, index) => message(`m${index}`));
    const digest = activityDigest(activity(items), { limit: 0 });
    expect(digest.items).toHaveLength(10);
    expect(digest.hidden).toBe(0);
  });
});

describe("关键模式过滤", () => {
  const items = [
    message("开始实现"),
    item({ kind: "step", text: "准备工具" }),
    tool("read", "completed", "a.ts"),
    tool("write", "running", "b.ts"),
    tool("bash", "error", "npm test", "exit 1"),
    tool("edit", "completed", "c.ts"),
  ];

  it("过滤步骤与成功工具明细", () => {
    const kept = filterActivityItems(items, true);
    expect(kept.some((entry) => entry.kind === "step")).toBe(false);
    expect(kept.some((entry) => entry.tool === "read")).toBe(false);
    expect(kept.some((entry) => entry.tool === "edit")).toBe(false);
  });

  it("保留失败工具与进行中的工具以及消息", () => {
    const kept = filterActivityItems(items, true);
    expect(kept.filter((entry) => entry.kind === "tool").map((entry) => entry.tool)).toEqual([
      "write",
      "bash",
    ]);
    expect(kept.some((entry) => entry.kind === "message")).toBe(true);
  });

  it("关键模式的摘要标记失败与进行中", () => {
    const digest = activityDigest(activity(items), { keyOnly: true, limit: 0 });
    const tones = digest.items.map((entry) => [entry.tool ?? entry.kind, entry.tone]);
    expect(tones).toEqual([
      ["message", "normal"],
      ["write", "live"],
      ["bash", "failure"],
    ]);
  });

  it("error 字段非空即视为失败", () => {
    expect(isToolFailure(tool("bash", "completed", null, "boom"))).toBe(true);
    expect(isKeyActivity(tool("bash", "completed", null, "boom"))).toBe(true);
    expect(isKeyActivity(item({ kind: "line", text: "raw stdout" }))).toBe(false);
  });
});
