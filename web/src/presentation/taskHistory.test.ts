import { describe, expect, it } from "vitest";
import type { JobSummary, VerificationHistory, VerificationRecord } from "../types";
import {
  HISTORY_VERDICT_LABELS,
  TASK_HISTORY_ALIGNED_LABEL,
  TASK_HISTORY_FILTER_LABEL,
  TASK_HISTORY_JOBS_LABEL,
  TASK_HISTORY_UNLINKED_VERIFIERS,
  alignHistory,
  filterFailedRounds,
  groupExecutionRounds,
  recordRounds,
  roundAnchorId,
  taskHistory,
  unlinkedVerifiersLabel,
} from "./taskHistory";

function at(minute: number): string {
  return `2026-01-10T10:${String(minute).padStart(2, "0")}:00Z`;
}

function beforeFirst(minute: number): string {
  return `2026-01-10T09:${String(minute).padStart(2, "0")}:00Z`;
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

function record(
  overrides: Partial<VerificationRecord> & { id: string; round: number },
): VerificationRecord {
  return {
    attempt: 1,
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
  attempt: 1,
  created_at: at(15),
  agent_name: "验收者二",
});

describe("按执行轮聚合验收尝试", () => {
  it("1 次执行 + 3 次验收（2 次失败）只产生 1 轮并汇总为 3 次尝试、2 次失败", () => {
    const rejected = job({ id: "v1", role: "verifier", created_at: at(5) });
    const crashed = job({
      id: "v2",
      role: "verifier",
      created_at: at(6),
      status: "failed",
      error: "验收进程异常退出",
    });
    const passed = job({ id: "v3", role: "verifier", created_at: at(7) });
    const result = taskHistory(
      [executor1, rejected, crashed, passed],
      history([
        record({ id: "v1", round: 1, passed: false, reason: "缺少测试" }),
        record({ id: "v3", round: 1, passed: true, reason: "符合验收标准" }),
      ]),
      "ready",
    );
    expect(result.rounds).toHaveLength(1);
    expect(result.rounds[0].round).toBe(1);
    expect(result.rounds[0].executor?.id).toBe("e1");
    expect(result.rounds[0].verifiers.map((entry) => entry.id)).toEqual([
      "v1",
      "v2",
      "v3",
    ]);
    expect(result.rounds[0].verifierAttempts.total).toBe(3);
    expect(result.rounds[0].verifierAttempts.failed).toBe(2);
    expect(result.rounds[0].verifierAttempts.latest?.id).toBe("v3");
    expect(result.rounds[0].verdict).toBe("passed");
    expect(result.rounds[0].failed).toBe(true);
  });

  it("拒绝后重新验收仍为同一轮，汇总 2 次尝试", () => {
    const rejected = job({ id: "v1", role: "verifier", created_at: at(5) });
    const retry = job({ id: "v2", role: "verifier", created_at: at(6) });
    const rounds = alignHistory(
      [executor1, rejected, retry],
      history([
        record({ id: "v1", round: 1, passed: false, reason: "缺少测试" }),
        record({ id: "v2", round: 1, passed: true, reason: "补充测试后通过" }),
      ]),
    );
    expect(rounds).toHaveLength(1);
    expect(rounds[0].verifiers.map((entry) => entry.id)).toEqual(["v1", "v2"]);
    expect(rounds[0].verifierAttempts).toMatchObject({ total: 2, failed: 1 });
    expect(rounds[0].verifierAttempts.latest?.id).toBe("v2");
    expect(rounds[0].verdict).toBe("passed");
  });

  it("2 次执行 + 2 次验收为 2 轮，验收各归其轮", () => {
    const result = taskHistory(
      [executor1, verifier1, executor2, verifier2],
      history([
        record({ id: "v1", round: 1 }),
        record({ id: "v2", round: 2, passed: false, reason: "缺少测试" }),
      ]),
      "ready",
    );
    expect(result.rounds.map((round) => round.round)).toEqual([1, 2]);
    expect(result.rounds[0].executor?.id).toBe("e1");
    expect(result.rounds[0].verifiers.map((entry) => entry.id)).toEqual(["v1"]);
    expect(result.rounds[0].verifierAttempts.total).toBe(1);
    expect(result.rounds[1].executor?.id).toBe("e2");
    expect(result.rounds[1].verifiers.map((entry) => entry.id)).toEqual(["v2"]);
    expect(result.rounds[1].verdict).toBe("rejected");
    expect(result.rounds[1].verifierAttempts).toMatchObject({ total: 1, failed: 1 });
  });

  it("rounds 长度恒等于执行作业数", () => {
    const result = taskHistory(
      [executor1, verifier1, executor2, verifier2, executor3],
      history([
        record({ id: "v1", round: 1 }),
        record({ id: "v2", round: 2, passed: false }),
      ]),
      "ready",
    );
    expect(result.rounds.map((round) => round.round)).toEqual([1, 2, 3]);
    expect(result.rounds[2].verifier).toBeNull();
    expect(result.rounds[2].verdict).toBe("none");
    expect(result.rounds[2].verifierAttempts).toEqual({
      total: 0,
      failed: 0,
      latest: null,
    });
  });

  it("历史只保留轮次与作业元信息，不携带结果或原因长文本", () => {
    const rounds = alignHistory([executor1]);
    expect("resultLine" in rounds[0]).toBe(false);
    expect("resultText" in rounds[0]).toBe(false);
    expect("reason" in rounds[0]).toBe(false);
    expect(rounds[0].executor?.id).toBe("e1");
    expect(rounds[0].executor?.statusLabel).toBe("成功");
    expect(rounds[0].executor?.durationKnown).toBe(true);
    expect(rounds[0].executor?.usageKnown).toBe(true);

    const withRecord = alignHistory(
      [executor1, verifier1],
      history([record({ id: "v1", round: 1, passed: false, reason: "缺少测试" })]),
    );
    expect(withRecord[0].verifiers[0].verdict).toBe("rejected");
    expect("resultLine" in withRecord[0].verifiers[0]).toBe(false);
    expect("reason" in withRecord[0]).toBe(false);
  });
});

describe("未关联执行的验收作业", () => {
  it("早于全部执行作业且接口 round=0 的验收作业计入 unlinked，不占用轮次", () => {
    const early = job({
      id: "v0",
      role: "verifier",
      created_at: beforeFirst(59),
    });
    const unanchored = job({ id: "v1", role: "verifier", created_at: at(5) });
    const result = taskHistory(
      [executor1, early, unanchored],
      history([record({ id: "v1", round: 0 })]),
      "ready",
    );
    expect(result.rounds).toHaveLength(1);
    expect(result.rounds[0].verifiers).toEqual([]);
    expect(result.unlinkedVerifiers).toBe(2);
  });

  it("没有验收作业时 unlinked 为零，筛选后为空", () => {
    const result = taskHistory([executor1], null, "loading");
    expect(result.rounds).toHaveLength(1);
    expect(result.unlinkedVerifiers).toBe(0);
    expect(filterFailedRounds(result.rounds, true)).toEqual([]);
  });

  it("未关联提示文案固定", () => {
    expect(TASK_HISTORY_UNLINKED_VERIFIERS).toBe(
      "有 {count} 次验收作业没有可关联的执行轮",
    );
    expect(unlinkedVerifiersLabel(2)).toBe("有 2 次验收作业没有可关联的执行轮");
  });
});

describe("验收结论", () => {
  it("验收作业全部失败且无结论时判为验收异常", () => {
    const rounds = alignHistory([
      executor1,
      job({ id: "v1", role: "verifier", created_at: at(5), status: "failed", error: "第一次异常" }),
      job({ id: "v2", role: "verifier", created_at: at(6), status: "failed", error: "第二次异常" }),
    ]);
    expect(rounds).toHaveLength(1);
    expect(rounds[0].verdict).toBe("error");
    expect(HISTORY_VERDICT_LABELS.error).toBe("验收异常");
    expect(rounds[0].failed).toBe(true);
    expect(rounds[0].verifierAttempts).toMatchObject({ total: 2, failed: 2 });
  });

  it("验收尚未结束时待验收，不计为失败", () => {
    const rounds = alignHistory([
      executor1,
      job({ id: "v1", role: "verifier", created_at: at(5), status: "running" }),
    ]);
    expect(rounds[0].verdict).toBe("pending");
    expect(rounds[0].failed).toBe(false);
    expect(rounds[0].running).toBe(true);
    expect(rounds[0].verifierAttempts).toMatchObject({ total: 1, failed: 0 });
  });

  it("最近一次有结论的验收决定本轮结论", () => {
    const rounds = alignHistory(
      [
        executor1,
        job({ id: "v1", role: "verifier", created_at: at(5) }),
        job({ id: "v2", role: "verifier", created_at: at(6), status: "running" }),
      ],
      history([record({ id: "v1", round: 1 })]),
    );
    expect(rounds[0].verdict).toBe("passed");
    expect(rounds[0].running).toBe(true);
  });

  it("执行失败的轮次计入问题轮", () => {
    const rounds = alignHistory([
      job({ id: "e1", status: "failed", error: "boom" }),
      job({ id: "v1", role: "verifier", created_at: at(5) }),
    ]);
    expect(rounds[0].verdict).toBe("pending");
    expect(rounds[0].failed).toBe(true);
  });
});

describe("乱序、截断与筛选", () => {
  const ordered = [executor1, verifier1, executor2, verifier2, executor3];
  const historyInput = history([
    record({ id: "v1", round: 1 }),
    record({ id: "v2", round: 2, passed: false }),
  ]);

  it("同一组作业任意顺序得到同一组轮次与尝试汇总", () => {
    const expected = alignHistory(ordered, historyInput);
    const permutations = [
      [...ordered].reverse(),
      [verifier2, executor3, verifier1, executor1, executor2],
      [executor2, executor1, executor3, verifier2, verifier1],
    ];
    for (const input of permutations) {
      const rounds = alignHistory(input, historyInput);
      expect(
        rounds.map((round) => [
          round.round,
          round.executor?.id,
          round.verifiers.map((entry) => entry.id),
          round.verifierAttempts.total,
        ]),
      ).toEqual(
        expected.map((round) => [
          round.round,
          round.executor?.id,
          round.verifiers.map((entry) => entry.id),
          round.verifierAttempts.total,
        ]),
      );
    }
  });

  it("验收记录截断缺少最早轮次的记录时编号不重排", () => {
    const lateVerifier = job({ id: "v3", role: "verifier", created_at: at(25) });
    const rounds = alignHistory(
      [executor1, executor2, executor3, lateVerifier],
      history([record({ id: "v3", round: 3 })], true),
    );
    expect(rounds.map((round) => round.round)).toEqual([1, 2, 3]);
    expect(rounds[0].verifiers).toEqual([]);
    expect(rounds[2].verifiers.map((entry) => entry.id)).toEqual(["v3"]);
    expect(recordRounds(history([record({ id: "v3", round: 3 })], true)).get("v3")).toBe(3);
  });

  it("执行作业被截断缺少最早轮次时编号顺延，验收仍归入对应轮", () => {
    const lateExecutor1 = job({ id: "e3", created_at: at(20) });
    const lateExecutor2 = job({ id: "e4", created_at: at(30) });
    const lateVerifier1 = job({
      id: "v3",
      role: "verifier",
      created_at: at(25),
    });
    const lateVerifier2 = job({
      id: "v4",
      role: "verifier",
      created_at: at(35),
    });
    const records = history(
      [
        record({ id: "v3", round: 3 }),
        record({ id: "v4", round: 4, passed: false }),
      ],
      true,
    );
    const rounds = alignHistory(
      [lateExecutor1, lateVerifier1, lateExecutor2, lateVerifier2],
      records,
    );
    expect(rounds.map((round) => round.round)).toEqual([1, 2]);
    expect(rounds[0].executor?.id).toBe("e3");
    expect(rounds[0].verifiers.map((entry) => entry.id)).toEqual(["v3"]);
    expect(rounds[1].executor?.id).toBe("e4");
    expect(rounds[1].verifiers.map((entry) => entry.id)).toEqual(["v4"]);
    // 接口轮次保持执行轮编号，本地轮次按现存执行作业顺延。
    expect(recordRounds(records).get("v3")).toBe(3);
    expect(recordRounds(records).get("v4")).toBe(4);
  });

  it("分组纯函数只按执行作业成轮", () => {
    const grouping = groupExecutionRounds(
      [executor1, executor2],
      [verifier1],
      new Map(),
    );
    expect(grouping.rounds.map((entry) => entry.round)).toEqual([1, 2]);
    expect(grouping.rounds[0].verifiers.map((entry) => entry.id)).toEqual(["v1"]);
    expect(grouping.unlinked).toEqual([]);

    const onlyVerifier = groupExecutionRounds([], [verifier1], new Map());
    expect(onlyVerifier.rounds).toEqual([]);
    expect(onlyVerifier.unlinked.map((entry) => entry.id)).toEqual(["v1"]);
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

  it("没有作业时来源为空", () => {
    const result = taskHistory([], null, "loading");
    expect(result.rounds).toEqual([]);
    expect(result.unlinkedVerifiers).toBe(0);
    expect(result.source).toBe("empty");
    expect(result.sourceLabel).toBe("");
  });

  it("来源标注区分接口对齐与仅作业记录", () => {
    const aligned = taskHistory(
      [executor1, verifier1],
      history([record({ id: "v1", round: 1 })]),
      "ready",
    );
    expect(aligned.source).toBe("aligned");
    expect(aligned.sourceLabel).toBe(TASK_HISTORY_ALIGNED_LABEL);

    const jobsOnly = taskHistory([executor1, verifier1], null, "loading");
    expect(jobsOnly.source).toBe("jobs-only");
    expect(jobsOnly.sourceLabel).toBe(TASK_HISTORY_JOBS_LABEL);
  });

  it("只看问题轮筛选保留执行失败、验收拒绝与验收尝试失败的轮次", () => {
    const rounds = alignHistory(ordered, historyInput);
    expect(filterFailedRounds(rounds, false)).toBe(rounds);
    expect(filterFailedRounds(rounds, true).map((round) => round.round)).toEqual([2]);
    expect(TASK_HISTORY_FILTER_LABEL).toBe("只看问题轮");
  });

  it("轮次锚点固定可定位", () => {
    expect(roundAnchorId(3)).toBe("task-round-3");
  });
});
