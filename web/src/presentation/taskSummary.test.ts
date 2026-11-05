import { describe, expect, it } from "vitest";
import type { JobSummary, VerificationHistory, VerificationRecord } from "../types";
import {
  RESULT_SUMMARY_EMPTY,
  VERIFICATION_FALLBACK_LABEL,
  VERIFICATION_HISTORY_LABEL,
  VERIFICATION_LOADING_LABEL,
  hasSummaryText,
  resultSource,
  verificationRecap,
  verificationSignature,
} from "./taskSummary";

function record(overrides: Partial<VerificationRecord> = {}): VerificationRecord {
  return {
    execution_job_id: "job1",
    id: "v1",
    round: 1,
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
    started_at: "2026-01-10T10:00:00Z",
    finished_at: "2026-01-10T10:01:00Z",
    usage_known: true,
    ...overrides,
  };
}

function history(records: VerificationRecord[]): VerificationHistory {
  return {
    project_id: "p1",
    task_id: "t1",
    total: records.length,
    limit: 50,
    truncated: false,
    verifications: records,
  };
}

function job(overrides: Partial<JobSummary> = {}): JobSummary {
  return {
    execution_job_id: null,
    id: "job1",
    project_id: "p1",
    task_id: "t1",
    role: "executor",
    agent_id: null,
    agent_name: null,
    status: "succeeded",
    attempt: 1,
    error: null,
    exit_code: 0,
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
    finished_at: "2026-01-10T10:01:00Z",
    created_at: "2026-01-10T10:00:00Z",
    ...overrides,
  };
}

const taskBase = {
  stage: "running",
  status: null,
  human_gate: null,
  attempts: 0,
  rejections: 0,
  verifier_issues: [],
  last_error: null,
};

describe("verificationRecap 结论", () => {
  it("等待条件与上级决策不计为拒绝，并保留解除指导", () => {
    for (const action of ["wait", "review"] as const) {
      const recap = verificationRecap(taskBase, history([record({ passed: false, next_action: action,
        guidance: "用户连接设备", issues: ["缺少运行证据"] })]), "ready");
      expect(recap.conclusion).toBe(action === "wait" ? "waiting" : "review");
      expect(recap.rejected).toBe(0);
      expect(recap.issues).toContain("用户连接设备");
    }
  });
  it("最近一条验收作业通过时结论为通过", () => {
    const recap = verificationRecap(
      { ...taskBase, stage: "completed", attempts: 1 },
      history([record({ passed: true, round: 1 })]),
      "ready",
    );
    expect(recap.conclusion).toBe("passed");
    expect(recap.source).toBe("history");
    expect(recap.sourceLabel).toBe(VERIFICATION_HISTORY_LABEL);
    expect(recap.reason).toBe("符合验收标准");
    expect(recap.round).toBe(1);
  });

  it("最近一条验收作业拒绝时结论为拒绝，并给出原因与问题列表", () => {
    const recap = verificationRecap(
      { ...taskBase, stage: "executing", attempts: 2, rejections: 1 },
      history([
        record({ id: "v1", round: 1, passed: true }),
        record({
          id: "v2",
          round: 2,
          passed: false,
          reason: "验收未通过",
          issues: ["缺少边界", "没有测试"],
        }),
      ]),
      "ready",
    );
    expect(recap.conclusion).toBe("rejected");
    expect(recap.total).toBe(2);
    expect(recap.rejected).toBe(1);
    expect(recap.reason).toBe("验收未通过");
    expect(recap.issues).toEqual(["缺少边界", "没有测试"]);
    expect(recap.round).toBe(2);
  });

  it("最新一条为运行中的验收作业时结论为待验收", () => {
    const recap = verificationRecap(
      { ...taskBase, stage: "verifying", attempts: 1 },
      history([record({ id: "v2", round: 2, status: "running", passed: null, reason: null })]),
      "ready",
    );
    expect(recap.conclusion).toBe("pending");
  });

  it("没有验收作业时结论为未验收，而不是空区块", () => {
    const recap = verificationRecap({ ...taskBase, stage: "completed" }, history([]), "ready");
    expect(recap.conclusion).toBe("none");
    expect(recap.total).toBe(0);
    expect(recap.issues).toEqual([]);
  });

  it("验收作业结果无法解析时带上原文摘要", () => {
    const recap = verificationRecap(
      taskBase,
      history([
        record({
          passed: null,
          parsed: false,
          reason: null,
          parse_error: "无法解析：结果不是合法 JSON",
          raw_excerpt: "not json",
        }),
      ]),
      "ready",
    );
    expect(recap.conclusion).toBe("pending");
    expect(recap.unparsedExcerpt).toBe("无法解析：结果不是合法 JSON");
  });

  it("记录被截断时标注统计范围", () => {
    const recap = verificationRecap(
      taskBase,
      { ...history([record()]), total: 80, truncated: true },
      "ready",
    );
    expect(recap.truncated).toBe(true);
    expect(recap.detailNote).toContain("1/80");
  });
});

describe("verificationRecap 回退来源", () => {
  it("验证记录不可用时不把条件缺项推测为验收拒绝", () => {
    const recap = verificationRecap({ ...taskBase, stage: "verifying", status: "blocked",
      verifier_issues: ["缺少设备"], last_error: "用户连接设备后重新验收" }, null, "error");
    expect(recap.conclusion).toBe("pending");
    expect(recap.rejected).toBe(0);
  });
  it("接口不可用时回退到 verifier_issues 与 last_error，并标注来源", () => {
    const recap = verificationRecap(
      {
        ...taskBase,
        stage: "executing",
        attempts: 2,
        rejections: 1,
        verifier_issues: ["没有覆盖边界"],
        last_error: "执行超时",
      },
      null,
      "error",
    );
    expect(recap.source).toBe("task");
    expect(recap.sourceLabel).toBe(VERIFICATION_FALLBACK_LABEL);
    expect(recap.conclusion).toBe("rejected");
    expect(recap.issues).toEqual(["没有覆盖边界"]);
    expect(recap.reason).toBe("执行超时");
    expect(recap.total).toBe(2);
    expect(recap.rejected).toBe(1);
    expect(recap.detailNote).toContain("验收记录不可用");
  });

  it("回退来源里已完成且有执行次数的任务视为通过", () => {
    const recap = verificationRecap(
      { ...taskBase, stage: "completed", attempts: 1 },
      null,
      "error",
    );
    expect(recap.conclusion).toBe("passed");
  });

  it("回退来源里从未执行过的任务显示未验收", () => {
    const recap = verificationRecap(taskBase, null, "error");
    expect(recap.conclusion).toBe("none");
    expect(recap.total).toBe(0);
  });

  it("回退来源里等待人工验收的任务显示待验收", () => {
    const recap = verificationRecap(
      { ...taskBase, stage: "completed", human_gate: "milestone", attempts: 1 },
      null,
      "error",
    );
    expect(recap.conclusion).toBe("pending");
  });

  it("接口加载中时先按任务字段渲染，并标注加载中", () => {
    const recap = verificationRecap(
      { ...taskBase, attempts: 1, stage: "verifying" },
      null,
      "loading",
    );
    expect(recap.sourceLabel).toBe(VERIFICATION_LOADING_LABEL);
    expect(recap.conclusion).toBe("pending");
  });
});

describe("resultSource 结果摘要来源", () => {
  it("取最近一次成功的执行作业", () => {
    const source = resultSource("t1", [
      job({ id: "e1", attempt: 1, finished_at: "2026-01-10T10:01:00Z" }),
      job({ id: "e2", attempt: 2, finished_at: "2026-01-10T11:01:00Z" }),
      job({ id: "e3", attempt: 3, status: "failed", finished_at: "2026-01-10T12:01:00Z" }),
      job({ id: "v1", role: "verifier", finished_at: "2026-01-10T13:01:00Z" }),
    ]);
    expect(source?.jobId).toBe("e2");
    expect(source?.label).toBe("来自最近成功执行作业");
    expect(source?.finishedAt).toBe("2026-01-10T11:01:00Z");
  });

  it("没有成功的执行作业时没有来源说明", () => {
    expect(resultSource("t1", [job({ status: "failed" })])).toBeNull();
    expect(resultSource("t1", [])).toBeNull();
    expect(resultSource("t1", [job({ task_id: "t2" })])).toBeNull();
  });
});

describe("结果摘要空状态", () => {
  it("空白结果按空状态处理", () => {
    expect(hasSummaryText(null)).toBe(false);
    expect(hasSummaryText("   ")).toBe(false);
    expect(hasSummaryText("## 完成")).toBe(true);
    expect(RESULT_SUMMARY_EMPTY.length).toBeGreaterThan(0);
  });
});

describe("verificationSignature 刷新触发", () => {
  const tasks = [
    {
      id: "t1",
      stage: "verifying",
      status: null,
      human_gate: null,
      attempts: 1,
      rejections: 0,
      verifier_issues: [] as string[],
      last_error: null,
    },
  ];
  const jobs = [
    { id: "v1", task_id: "t1", role: "verifier", status: "running", attempt: 1, finished_at: null },
  ];

  it("同一快照签名稳定，验收相关数据变化时签名改变", () => {
    expect(verificationSignature(tasks, jobs)).toBe(verificationSignature(tasks, jobs));
    expect(
      verificationSignature([{ ...tasks[0], stage: "completed" }], jobs),
    ).not.toBe(verificationSignature(tasks, jobs));
    expect(
      verificationSignature(tasks, [{ ...jobs[0], status: "succeeded" }]),
    ).not.toBe(verificationSignature(tasks, jobs));
    // 空快照也有稳定签名，页面首次加载后被真实数据替换时触发一次读取。
    expect(verificationSignature([], [])).toBe(verificationSignature([], []));
    expect(verificationSignature([], [])).not.toBe(verificationSignature(tasks, jobs));
  });
});
