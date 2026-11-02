import { describe, expect, it } from "vitest";
import type { JobActivity, JobActivityItem } from "../types";
import {
  ACTIVITY_CLOCK_MS,
  ACTIVITY_DIGEST_LIMIT,
  ACTIVITY_HEADLINE_MAX,
  ACTIVITY_POLL_MS,
  ACTIVITY_WAITING_TEXT,
  activityBlocks,
  activityDigest,
  activityHeadline,
  activityProgressText,
  activitySignature,
  filterActivityItems,
  formatClockTime,
  isKeyActivity,
  isNearBottom,
  isToolFailure,
  nextBeatDelay,
  relativeTimeLabel,
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

function reasoning(text: string): JobActivityItem {
  return item({ kind: "reasoning", text });
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
      reasoning: items.filter((entry) => entry.kind === "reasoning").length,
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

describe("思维链摘要与关键模式", () => {
  it("思维链取首个有内容的行并标记为思考", () => {
    const headline = activityHeadline(reasoning("## 先梳理目标\n\n然后拆解任务"));
    expect(headline).toBe("思考 · 先梳理目标");
  });

  it("思维链条目在关键模式下保留", () => {
    const entries = [reasoning("检查依赖"), tool("read", "completed", "a.ts")];
    expect(isKeyActivity(entries[0])).toBe(true);
    expect(isKeyActivity(entries[1])).toBe(false);
    const digest = activityDigest(activity(entries), { keyOnly: true, limit: 0 });
    expect(digest.items.map((entry) => entry.kind)).toEqual(["reasoning"]);
    expect(digest.items[0]?.text).toBe("思考 · 检查依赖");
    expect(digest.items[0]?.tone).toBe("normal");
  });

  it("思维链中的结构化内容同样不直接输出", () => {
    const headline = activityHeadline(reasoning(JSON.stringify({ step: 1 })));
    expect(headline).not.toContain("{");
    expect(headline).toContain("结构化内容");
  });

  it("空思维链不进入摘要", () => {
    expect(activityHeadline(reasoning("   "))).toBe("");
    expect(activityDigest(activity([reasoning("")])).waiting).toBe(
      ACTIVITY_WAITING_TEXT,
    );
  });

  it("进展文案优先最新思维链，其次消息", () => {
    expect(
      activityProgressText([
        message("旧消息"),
        reasoning("检查依赖"),
        tool("read", "completed", "a.ts"),
      ]),
    ).toBe("思考 · 检查依赖");
    expect(activityProgressText([message("开始执行")])).toBe("开始执行");
    expect(activityProgressText([])).toBe("正在思考…");
  });
});

describe("活动分块", () => {
  it("连续思维链独立成块，工具与步骤合并为一组", () => {
    const blocks = activityBlocks([
      reasoning("一"),
      reasoning("二"),
      tool("read", "completed", "a.ts"),
      item({ kind: "step", text: "准备" }),
      tool("bash", "error", "npm test", "exit 1"),
      message("继续"),
    ]);
    expect(blocks.map((block) => block.type)).toEqual([
      "reasoning",
      "compact",
      "message",
    ]);
    expect(blocks[0]?.type === "reasoning" && blocks[0].items).toHaveLength(2);
    expect(blocks[1]?.type === "compact" && blocks[1].items).toHaveLength(3);
    expect(blocks[2] && blocks[2].start).toBe(5);
  });

  it("分块保留原列表偏移", () => {
    const blocks = activityBlocks([message("只有消息")], 12);
    expect(blocks[0]?.start).toBe(12);
  });
});

describe("跟随最新内容", () => {
  it("贴底时继续跟随，上滚后停止，重新贴底恢复", () => {
    expect(isNearBottom(1_000, 580, 400)).toBe(true);
    expect(isNearBottom(1_000, 400, 400)).toBe(false);
    expect(isNearBottom(1_000, 576, 400)).toBe(true);
  });
});

describe("稳定时间显示", () => {
  it("相对时间按整秒向下取整，不会回跳", () => {
    expect(relativeTimeLabel(1_000, 1_000)).toBe("刚刚");
    expect(relativeTimeLabel(1_000, 1_999)).toBe("刚刚");
    expect(relativeTimeLabel(1_000, 4_400)).toBe("3 秒前");
    expect(relativeTimeLabel(1_000, 62_000)).toBe("1 分钟前");
    expect(relativeTimeLabel(1_000, 3_600_000)).toBe("59 分钟前");
  });

  it("节拍延迟总是落在下一个边界", () => {
    for (const now of [0, 1, 999, 1_000, 1_499, 1_500, 1_501]) {
      const delay = nextBeatDelay(now, ACTIVITY_POLL_MS);
      expect(delay).toBeGreaterThan(0);
      expect((now + delay) % ACTIVITY_POLL_MS).toBe(0);
    }
    expect(nextBeatDelay(500, ACTIVITY_CLOCK_MS)).toBe(500);
    expect(nextBeatDelay(1_000, ACTIVITY_CLOCK_MS)).toBe(ACTIVITY_CLOCK_MS);
  });

  it("时钟格式包含时分秒且非法值有占位", () => {
    expect(formatClockTime(Date.parse("2026-01-10T10:20:30"))).toMatch(
      /\d{1,2}:\d{2}:\d{2}/,
    );
    expect(formatClockTime(Number.NaN)).toBe("—");
  });

  it("活动指纹只在可见内容变化时改变", () => {
    const base = activity([reasoning("思考")]);
    const grownBytes = { ...base, bytes: base.bytes + 100 };
    expect(activitySignature(base)).toBe(activitySignature(grownBytes));
    const grownText = activity([reasoning("思考更多")]);
    expect(activitySignature(grownText)).not.toBe(activitySignature(base));
    expect(activitySignature(null)).toBe("");
  });

  it("新增条目与工具目标补齐都会改变活动指纹", () => {
    const base = activity([tool("read", "running", null)]);
    const appended = activity([tool("read", "running", null), message("新输出")]);
    const filledTarget = activity([tool("read", "running", "src/a.ts")]);
    expect(activitySignature(appended)).not.toBe(activitySignature(base));
    expect(activitySignature(filledTarget)).not.toBe(activitySignature(base));
    const replacedTarget = activity([tool("read", "running", "src/b.ts")]);
    expect(activitySignature(replacedTarget)).not.toBe(
      activitySignature(filledTarget),
    );
  });
});
