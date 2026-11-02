import { describe, expect, it } from "vitest";
import type { JobSummary, VerificationHistory, VerificationRecord } from "../types";
import {
  HISTORY_VERDICT_LABELS,
  TASK_HISTORY_ALIGNED_LABEL,
  TASK_HISTORY_JOBS_LABEL,
  alignHistory,
  filterFailedRounds,
  recordRounds,
  roundAnchorId,
  summaryLine,
  taskHistory,
} from "./taskHistory";

function at(minute: number): string {
  return `2026-01-10T10:${String(minute).padStart(2, "0")}:00Z`;
}

function job(overrides: Partial<JobSummary> & { id: string }): JobSummary {
  return {
    project_id: "p1",
    task_id: "t1",
    role: "executor",
    agent_id: null,
    agent_name: "执行者",
    status: "succeeded",
    attempt: 1,
    error: null,
    exit_code: 0,
    failure_kind: null,
    failure_summary: null,
    input_tokens: 10,
    output_tokens: 20,
    cache_read_tokens: null,
    cache_write_tokens: null,
    reasoning_tokens: null,
    cost: 0.01,
    duration_ms: 60_000,
    usage_known: true,
    started_at: at(0),
    finished_at: at(1),
    created_at: at(0),
    ...overrides,
  };
}

function record(overrides: Partial<VerificationRecord> & { id: string; round: number }): VerificationRecord {
  return {
    attempt: overrides.round,
    status: "succeeded",
    passed: true,
    reason: "符合验收标准",
    issues: [],
    parsed: true,
    parse_error: null,
    raw_excerpt: null,
    agent_id: null,
    agent_name: "验收者",
    started_at: null,
    finished_at: null,
    usage_known: true,
    ...overrides,
  };
}

function history(verifications: VerificationRecord[], truncated = false): VerificationHistory {
  return {
    project_id: "p1",
    task_id: "t1",
    total: verifications.length,
    limit: 50,
    truncated,
    verifications,
  };
}

/// 三轮执行、两轮验收：第 1 轮验收通过，第 2 轮验收拒绝，第 3 轮待验收。
const executor1 = job({ id: "e1", attempt: 1, created_at: at(0) });
const executor2 = job({ id: "e2", attempt: 2, created_at: at(10) });
const executor3 = job({ id: "e3", attempt: 3, created_at: at(20) });
const verifier1 = job({
  id: "v1",
  role: "verifier",
  attempt: 1,
  created_at: at(5),
  agent_name: "验收者一",
});
const verifier2 = job({
  id: "v2",
  role: "verifier",
  attempt: 2,
  created_at: at(15),
  agent_name: "验收者二",
});

describe("summaryLine 结果摘要首行", () => {
  it("取第一个非空行并折叠空白", () => {
    expect(summaryLine("\n\n  第一行   有空格 \n第二行")).toBe("第一行 有空格");
    expect(summaryLine("单行")).toBe("单行");
    expect(summaryLine("   ")).toBeNull();
    expect(summaryLine(null)).toBeNull();
  });

  it("去掉 Markdown 标题与列表标记", () => {
    expect(summaryLine("## 完成内容\n\n细节")).toBe("完成内容");
    expect(summaryLine("- 第一项")).toBe("第一项");
    expect(summaryLine("1. 步骤")).toBe("步骤");
  });

  it("超长首行截断并保留省略号", () => {
    const line = summaryLine("长".repeat(50), 10);
    expect(Array.from(line ?? "")).toHaveLength(10);
    expect(line?.endsWith("…")).toBe(true);
  });
});

describe("同一轮次执行与验收配对", () => {
  it("三轮执行两轮验收对齐成三轮，验收归属各自执行轮", () => {
    const result = taskHistory(
      [executor1, verifier1, executor2, verifier2, executor3],
      history([record({ id: "v1", round: 1 }), record({ id: "v2", round: 2, passed: false })]),
      "ready",
    );
    expect(result.rounds.map((round) => round.round)).toEqual([1, 2, 3]);
    expect(result.rounds[0].executor?.id).toBe("e1");
    expect(result.rounds[0].verifier?.id).toBe("v1");
    expect(result.rounds[1].executor?.id).toBe("e2");
    expect(result.rounds[1].verifier?.id).toBe("v2");
    expect(result.rounds[2].executor?.id).toBe("e3");
    expect(result.rounds[2].verifier).toBeNull();
    expect(result.rounds.map((round) => round.verdict)).toEqual([
      "passed",
      "rejected",
      "none",
    ]);
    expect(result.failedRounds).toBe(1);
    expect(result.source).toBe("aligned");
    expect(result.sourceLabel).toBe(TASK_HISTORY_ALIGNED_LABEL);
  });

  it("每轮展示 Agent、状态、时长、用量已知与原因", () => {
    const rounds = alignHistory(
      [executor1, verifier1],
      history([record({ id: "v1", round: 1 })]),
      { e1: "完成第一轮\n更多说明" },
    );
    const [first] = rounds;
    expect(first.executor?.agentName).toBe("执行者");
    expect(first.executor?.statusLabel).toBe("成功");
    expect(first.executor?.durationKnown).toBe(true);
    expect(first.executor?.usageKnown).toBe(true);
    expect(first.resultLine).toBe("完成第一轮");
    expect(first.verifier?.agentName).toBe("验收者一");
    expect(first.reason).toBe("符合验收标准");
  });

  it("用量未知与时长缺失在轮次里可行", () => {
    const rounds = alignHistory([
      job({ id: "e1", usage_known: false, duration_ms: null }),
    ]);
    expect(rounds[0].executor?.usageKnown).toBe(false);
    expect(rounds[0].executor?.durationKnown).toBe(false);
    expect(rounds[0].resultLine).toBeNull();
  });
});

describe("缺失验收", () => {
  it("没有验收作业的轮次结论为未验收，不计入失败", () => {
    const rounds = alignHistory([executor1, executor2]);
    expect(rounds).toHaveLength(2);
    expect(rounds.every((round) => round.verdict === "none")).toBe(true);
    expect(rounds.every((round) => round.failed === false)).toBe(true);
    expect(HISTORY_VERDICT_LABELS.none).toBe("未验收");
  });

  it("验收记录未读出时只按作业时间对齐，来源标注作业记录", () => {
    const result = taskHistory(
      [executor1, verifier1, executor2, executor3],
      null,
      "loading",
    );
    expect(result.source).toBe("jobs-only");
    expect(result.sourceLabel).toBe(TASK_HISTORY_JOBS_LABEL);
    expect(result.rounds.map((round) => round.round)).toEqual([1, 2, 3]);
    expect(result.rounds[0].verifier?.id).toBe("v1");
    expect(result.rounds[1].verifier).toBeNull();
  });

  it("验收作业没有结论时按状态判定：未结束待验收，失败计入失败轮次", () => {
    const running = alignHistory([
      executor1,
      job({ id: "v1", role: "verifier", created_at: at(5), status: "running" }),
    ]);
    expect(running[0].verdict).toBe("pending");
    expect(running[0].running).toBe(true);
    expect(running[0].failed).toBe(false);

    const failed = alignHistory([
      executor1,
      job({ id: "v1", role: "verifier", created_at: at(5), status: "failed", error: "验收进程退出" }),
    ]);
    expect(failed[0].verdict).toBe("rejected");
    expect(failed[0].failed).toBe(true);
    expect(failed[0].reason).toBe("验收进程退出");
  });

  it("执行失败的轮次计入失败，验收通过也保留执行状态", () => {
    const rounds = alignHistory([
      job({ id: "e1", status: "failed", error: "boom" }),
      job({ id: "v1", role: "verifier", created_at: at(5) }),
    ]);
    expect(rounds[0].verdict).toBe("pending");
    expect(rounds[0].failed).toBe(true);
  });
});

describe("乱序输入", () => {
  const ordered = [executor1, executor2, executor3, verifier1, verifier2];
  const historyInput = history([
    record({ id: "v1", round: 1 }),
    record({ id: "v2", round: 2, passed: false }),
  ]);

  it("同一组作业任意顺序得到同一组轮次", () => {
    const expected = alignHistory(ordered, historyInput);
    const permutations = [
      [...ordered].reverse(),
      [verifier2, executor3, verifier1, executor1, executor2],
      [executor2, executor1, executor3, verifier2, verifier1],
    ];
    for (const input of permutations) {
      const rounds = alignHistory(input, historyInput);
      expect(rounds.map((round) => [round.round, round.executor?.id, round.verifier?.id])).toEqual(
        expected.map((round) => [round.round, round.executor?.id, round.verifier?.id]),
      );
    }
  });

  it("验收记录乱序与截断时按序号平移对齐", () => {
    expect(
      [...recordRounds(history([record({ id: "v2", round: 2 }), record({ id: "v1", round: 1 })]))],
    ).toEqual([
      ["v1", 1],
      ["v2", 2],
    ]);
    // 接口只保留最近 3 条，序号从 3 开始：平移回任务轮次 3、4、5。
    const truncated = recordRounds(
      history(
        [
          record({ id: "v5", round: 5 }),
          record({ id: "v3", round: 3 }),
          record({ id: "v4", round: 4 }),
        ],
        true,
      ),
    );
    expect(truncated.get("v3")).toBe(3);
    expect(truncated.get("v5")).toBe(5);
  });

  it("验收作业结果原文作为原因回退", () => {
    const rounds = alignHistory(
      [executor1, job({ id: "v1", role: "verifier", created_at: at(5) })],
      null,
      { v1: "结论：不通过\n原因是缺测试" },
    );
    expect(rounds[0].reason).toBe("结论：不通过");
  });

  it("接口序号把重试验收并回同一执行轮", () => {
    const retry = job({ id: "v1b", role: "verifier", created_at: at(6) });
    const rounds = alignHistory(
      [executor1, verifier1, retry, executor2],
      history([record({ id: "v1", round: 1 })]),
    );
    expect(rounds.map((round) => round.round)).toEqual([1, 2]);
    expect(rounds[0].verifiers.map((entry) => entry.id)).toEqual(["v1", "v1b"]);
  });

  it("只有验收作业时也成轮", () => {
    const rounds = alignHistory([verifier1]);
    expect(rounds).toHaveLength(1);
    expect(rounds[0].executor).toBeNull();
    expect(rounds[0].verifiers.map((entry) => entry.id)).toEqual(["v1"]);
  });

  it("非任务作业（规划、总结）不进入历史", () => {
    const rounds = alignHistory([
      executor1,
      job({ id: "p1", role: "planner", created_at: at(0) }),
      job({ id: "s1", role: "summary", created_at: at(9) }),
    ]);
    expect(rounds).toHaveLength(1);
    expect(rounds[0].executors.map((entry) => entry.id)).toEqual(["e1"]);
  });

  it("没有作业时来源为空，筛选后为空", () => {
    const result = taskHistory([], null, "loading");
    expect(result.rounds).toEqual([]);
    expect(result.source).toBe("empty");
    expect(result.sourceLabel).toBe("");
    expect(filterFailedRounds(result.rounds, true)).toEqual([]);
  });
});

describe("只看失败筛选", () => {
  const rounds = alignHistory(
    [executor1, verifier1, executor2, verifier2, executor3],
    history([record({ id: "v1", round: 1 }), record({ id: "v2", round: 2, passed: false })]),
  );

  it("开启筛选时只保留失败轮次，关闭时原样返回", () => {
    expect(filterFailedRounds(rounds, false)).toBe(rounds);
    expect(filterFailedRounds(rounds, true).map((round) => round.round)).toEqual([2]);
  });

  it("轮次锚点固定可定位", () => {
    expect(roundAnchorId(3)).toBe("task-round-3");
  });
});
