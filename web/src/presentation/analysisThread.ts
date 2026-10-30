import type { Message } from "../types";

/// 分析对话中规划者消息的展示状态；用户反馈之后，先前的分析结果收起。
export interface AnalysisMessageItem {
  message: Message;
  /// 已被后续用户反馈取代的规划者分析结果。
  folded: boolean;
}

/// 等待规划者回复的场景：首次提交目标后等待开始分析，
/// 发送反馈后等待下一次分析结果。
export type AnalysisWaiting = "start" | "next" | null;

export interface AnalysisThread {
  messages: AnalysisMessageItem[];
  waiting: AnalysisWaiting;
}

/// 规划者的分析结果在收到用户反馈后收起，新的分析结果保持展开。
/// 最后一条消息来自用户时进入等待，直到规划者给出新的分析结果。
export function analysisThread(messages: Message[]): AnalysisThread {
  const thread = messages.filter((message) => message.role !== "review");
  let lastUserIndex = -1;
  let lastPlannerIndex = -1;
  thread.forEach((message, index) => {
    if (message.role === "user") lastUserIndex = index;
    if (message.role === "planner") lastPlannerIndex = index;
  });

  const waiting: AnalysisWaiting =
    lastUserIndex >= 0 && lastUserIndex > lastPlannerIndex
      ? lastPlannerIndex >= 0
        ? "next"
        : "start"
      : null;

  return {
    messages: thread.map((message, index) => ({
      message,
      folded:
        message.role === "planner" &&
        lastUserIndex >= 0 &&
        index < lastUserIndex,
    })),
    waiting,
  };
}
