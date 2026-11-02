import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { JobSummary, VerificationHistory, VerificationRecord } from "./types";
import { ResultSummary, VerificationRecapBlock } from "./components/TaskVerification";
import {
  RESULT_SUMMARY_EMPTY,
  VERIFICATION_FALLBACK_LABEL,
  VERIFICATION_HISTORY_LABEL,
  verificationRecap,
} from "./presentation/taskSummary";

function record(overrides: Partial<VerificationRecord> = {}): VerificationRecord {
  return {
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
    agent_name: "验收者甲",
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

const taskBase = {
  stage: "running",
  status: null,
  human_gate: null,
  attempts: 0,
  rejections: 0,
  verifier_issues: [],
  last_error: null,
};

function job(overrides: Partial<JobSummary> = {}): JobSummary {
  return {
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

describe("结果摘要 Markdown 排版", () => {
  it("标题、列表与代码块按 Markdown 渲染，不输出原始 Markdown 记号", () => {
    const markup = renderToStaticMarkup(
      <ResultSummary
        taskId="t1"
        summary={"## 完成内容\n\n- 重构选项卡\n- 补充动效\n\n```ts\nconst a = 1;\n```"}
        jobs={[job()]}
      />,
    );
    expect(markup).toContain('class="rich-content rich-content-markdown"');
    expect(markup).toContain("rich-markdown");
    expect(markup).toMatch(/<h2[^>]*>完成内容<\/h2>/);
    expect(markup).toContain("<ul");
    expect(markup).toContain("<li>重构选项卡</li>");
    expect(markup).toContain("<li>补充动效</li>");
    expect(markup).toMatch(/<pre[^>]*class="rich-code/);
    expect(markup).not.toContain("## 完成内容");
    expect(markup).not.toContain("- 重构选项卡");
  });

  it("标注结果来自哪一次执行作业", () => {
    const markup = renderToStaticMarkup(
      <ResultSummary
        taskId="t1"
        summary={"完成"}
        jobs={[job({ id: "e2", attempt: 2, agent_name: "执行者乙" })]}
      />,
    );
    const source = markup.slice(markup.indexOf("task-result-source"));
    expect(source).toContain("来自第 2 次执行作业");
    expect(source).toContain("执行者乙");
  });

  it("没有结果时给出空状态与提示，而不是空白区块", () => {
    const markup = renderToStaticMarkup(
      <ResultSummary taskId="t1" summary={null} jobs={[]} />,
    );
    expect(markup).toContain("结果摘要");
    expect(markup).toContain(RESULT_SUMMARY_EMPTY);
    expect(markup).toContain("rich-content-empty");
    expect(markup).not.toContain("task-result-source");
  });
});

describe("验收摘要区块", () => {
  it("存在验收作业数据时渲染“验收摘要”与通过结论", () => {
    const recap = verificationRecap(
      { ...taskBase, stage: "completed", attempts: 1 },
      history([record({ passed: true })]),
      "ready",
    );
    const markup = renderToStaticMarkup(<VerificationRecapBlock recap={recap} />);
    expect(markup).toContain("验收摘要");
    expect(markup).toContain(">通过<");
    expect(markup).toContain(VERIFICATION_HISTORY_LABEL);
    expect(markup).toContain("验收 1 次 · 拒绝 0 次");
    expect(markup).toContain("符合验收标准");
  });

  it("最近一轮被拒绝时渲染拒绝结论、原因与问题列表", () => {
    const recap = verificationRecap(
      { ...taskBase, stage: "executing", attempts: 2, rejections: 1 },
      history([
        record({ id: "v1", round: 1, passed: true }),
        record({
          id: "v2",
          round: 2,
          passed: false,
          reason: "缺少边界处理",
          issues: ["未覆盖空输入", "缺少回归测试"],
        }),
      ]),
      "ready",
    );
    const markup = renderToStaticMarkup(<VerificationRecapBlock recap={recap} />);
    expect(markup).toContain("验收摘要");
    expect(markup).toContain(">拒绝<");
    expect(markup).toContain("缺少边界处理");
    expect(markup).toContain("未覆盖空输入");
    expect(markup).toContain("缺少回归测试");
    expect(markup).toContain("<ol");
    expect(markup).toContain("验收 2 次 · 拒绝 1 次");
    expect(markup).toContain("最近一轮：第 2 轮");
  });

  it("没有验收作业时显示未验收，而不是空区块", () => {
    const recap = verificationRecap({ ...taskBase, stage: "completed" }, history([]), "ready");
    const markup = renderToStaticMarkup(<VerificationRecapBlock recap={recap} />);
    expect(markup).toContain("验收摘要");
    expect(markup).toContain(">未验收<");
    expect(markup).toContain("还没有验收作业产出结论");
    expect(markup).toContain("验收 0 次 · 拒绝 0 次");
  });

  it("接口不可用时回退任务字段并标注回退来源", () => {
    const recap = verificationRecap(
      {
        ...taskBase,
        stage: "executing",
        attempts: 1,
        rejections: 1,
        verifier_issues: ["验收未通过：缺少说明"],
      },
      null,
      "error",
    );
    const markup = renderToStaticMarkup(<VerificationRecapBlock recap={recap} />);
    expect(markup).toContain(VERIFICATION_FALLBACK_LABEL);
    expect(markup).toContain(">拒绝<");
    expect(markup).toContain("验收未通过：缺少说明");
    expect(markup).not.toContain(VERIFICATION_HISTORY_LABEL);
  });
});
