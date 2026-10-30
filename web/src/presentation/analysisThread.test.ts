import { describe, expect, it } from "vitest";
import type { Message } from "../types";
import { analysisThread } from "./analysisThread";
import projectPageSource from "../pages/ProjectPage.tsx?raw";

function message(id: string, role: string, content = id): Message {
  return {
    id,
    project_id: "p1",
    role,
    content,
    created_at: "2026-01-10T10:00:00Z",
  };
}

describe("项目分析对话折叠", () => {
  it("用户反馈后收起规划者先前的分析结果", () => {
    const thread = analysisThread([
      message("m1", "user"),
      message("m2", "planner"),
      message("m3", "user"),
    ]);
    expect(thread.messages.map((item) => item.message.id)).toEqual([
      "m1",
      "m2",
      "m3",
    ]);
    expect(thread.messages.map((item) => item.folded)).toEqual([
      false,
      true,
      false,
    ]);
    expect(thread.waiting).toBe("next");
  });

  it("规划者给出新的分析结果后保持展开并结束等待", () => {
    const thread = analysisThread([
      message("m1", "user"),
      message("m2", "planner"),
      message("m3", "user"),
      message("m4", "planner"),
    ]);
    expect(thread.messages.map((item) => item.folded)).toEqual([
      false,
      true,
      false,
      false,
    ]);
    expect(thread.waiting).toBeNull();
  });

  it("多轮反馈只展开最新一次分析结果", () => {
    const thread = analysisThread([
      message("m1", "user"),
      message("m2", "planner"),
      message("m3", "user"),
      message("m4", "planner"),
      message("m5", "user"),
    ]);
    expect(thread.messages.map((item) => item.folded)).toEqual([
      false,
      true,
      false,
      true,
      false,
    ]);
    expect(thread.waiting).toBe("next");
  });

  it("首次提交目标后等待开始分析", () => {
    const thread = analysisThread([message("m1", "user")]);
    expect(thread.messages[0]?.folded).toBe(false);
    expect(thread.waiting).toBe("start");
  });

  it("没有消息时不展示等待", () => {
    const thread = analysisThread([]);
    expect(thread.messages).toEqual([]);
    expect(thread.waiting).toBeNull();
  });

  it("忽略项目报告验收消息", () => {
    const thread = analysisThread([
      message("m1", "user"),
      message("m2", "planner"),
      message("m3", "review"),
    ]);
    expect(thread.messages.map((item) => item.message.id)).toEqual(["m1", "m2"]);
    expect(thread.waiting).toBeNull();
  });
});

describe("项目分析区块", () => {
  it("折叠先前的分析结果并给出等待提示", () => {
    expect(projectPageSource).toContain(
      "analysisConversation.messages.map((item) =>",
    );
    expect(projectPageSource).toContain("上一条分析结果 · 已折叠");
    expect(projectPageSource).toContain(
      "已收到你的反馈，等待规划者给出下一次分析结果…",
    );
  });
});
