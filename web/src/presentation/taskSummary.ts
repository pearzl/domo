/// 结果摘要与验收摘要的呈现模型：只做纯数据推导，渲染层负责排版。
///
/// 验收摘要的数据来源优先取验收记录接口（逐轮验收作业），接口不可用时回退到
/// 任务快照里的 verifier_issues 与 last_error，并在界面标明回退来源。

import type {
  JobSummary,
  VerificationHistory,
  VerificationRecord,
} from "../types";

export type { VerificationHistory, VerificationRecord } from "../types";

export type VerificationConclusion = "passed" | "rejected" | "waiting" | "review" | "pending" | "none";

export const VERIFICATION_CONCLUSIONS: Record<
  VerificationConclusion,
  { label: string; tone: "success" | "danger" | "info" | "muted" }
> = {
  passed: { label: "通过", tone: "success" },
  rejected: { label: "拒绝", tone: "danger" },
  waiting: { label: "等待验证条件", tone: "info" },
  review: { label: "请求上级决策", tone: "info" },
  pending: { label: "待验收", tone: "info" },
  none: { label: "未验收", tone: "muted" },
};

/// 验收摘要的数据来源：接口记录，或回退到任务快照字段。
export type VerificationSource = "history" | "task";

export interface VerificationRecap {
  conclusion: VerificationConclusion;
  source: VerificationSource;
  /// 来源说明，直接展示给用户。
  sourceLabel: string;
  /// 最近一轮验收的原因与问题；没有验收作业时为空。
  reason: string | null;
  issues: string[];
  /// 无法解析的验收结果原文摘要，仅在解析失败时出现。
  unparsedExcerpt: string | null;
  /// 验收次数与拒绝次数。
  total: number;
  rejected: number;
  /// 接口记录被截断时，更早的验收作业未参与统计。
  truncated: boolean;
  /// 最近一轮验收的轮次；回退来源没有轮次时为空。
  round: number | null;
  /// 回退来源只记录最近一次验收的反馈，没有逐轮作业明细。
  detailNote: string | null;
}

/// 验收记录读取状态；ready 用接口记录，其余状态回退任务字段。
export type VerificationLoadState = "loading" | "ready" | "error";

/// 结果摘要来源说明：取最近一次产出结果的执行作业。
export interface ResultSource {
  label: string;
  jobId: string;
  finishedAt: string | null;
  agent: string | null;
}

export const RESULT_SUMMARY_EMPTY = "还没有记录执行结果。";
export const RESULT_SUMMARY_HINT =
  "执行作业产出结果后，这里按 Markdown 呈现，并标出来自哪一次作业。";
export const VERIFICATION_RECAP_TITLE = "验收摘要";
export const VERIFICATION_RECAP_EMPTY_HINT = "还没有验收作业产出结论。";
export const VERIFICATION_HISTORY_LABEL = "来源：验收记录（逐轮验收作业）";
export const VERIFICATION_FALLBACK_LABEL = "来源：任务字段（验收记录不可用）";
export const VERIFICATION_LOADING_LABEL = "来源：验收记录（加载中）";

/// 当前需要验收者的作业仍然占用时，验收尚未完成。
const RUNNING_STATUSES = new Set(["queued", "running", "pausing"]);

function successStatus(status: string): boolean {
  return status === "succeeded" || status === "completed";
}

/// 最近一次产出结果的执行作业：只取成功的执行作业，按完成时间取最新一条。
export function resultSource(taskId: string, jobs: JobSummary[]): ResultSource | null {
  const completed = jobs.filter(
    (job) =>
      job.task_id === taskId &&
      job.role === "executor" &&
      successStatus(job.status) &&
      (job.finished_at ?? job.started_at ?? job.created_at),
  );
  if (completed.length === 0) return null;
  const latest = [...completed].sort((left, right) => {
    const leftAt = left.finished_at ?? left.started_at ?? left.created_at;
    const rightAt = right.finished_at ?? right.started_at ?? right.created_at;
    if (leftAt !== rightAt) return leftAt < rightAt ? -1 : 1;
    return left.id < right.id ? -1 : 1;
  })[completed.length - 1];
  return {
    label: "来自最近成功执行作业",
    jobId: latest.id,
    finishedAt: latest.finished_at ?? latest.started_at ?? latest.created_at,
    agent: latest.agent_name,
  };
}

function latestRecord(history: VerificationHistory): VerificationRecord | null {
  if (history.verifications.length === 0) return null;
  return [...history.verifications].sort((left, right) => {
    if (left.round !== right.round) return left.round - right.round;
    return left.id < right.id ? -1 : 1;
  })[history.verifications.length - 1];
}

function recordConclusion(record: VerificationRecord): VerificationConclusion {
  // 验收作业已完成且给出结论时，以解析出的 pass 为准；作业未结束或结果无法解析时结论待定。
  if (!RUNNING_STATUSES.has(record.status)) {
    if (record.passed === true) return "passed";
    if (record.next_action === "wait") return "waiting";
    if (record.next_action === "review") return "review";
    if (record.passed === false) return "rejected";
  }
  return "pending";
}

function recordIssues(record: VerificationRecord): string[] {
  return record.issues.map((issue) => issue.trim()).filter((issue) => issue !== "");
}

function fromHistory(history: VerificationHistory): VerificationRecap {
  const rejected = history.verifications.filter(
    (record) => record.passed === false && record.next_action !== "wait" && record.next_action !== "review",
  ).length;
  const latest = latestRecord(history);
  const base: VerificationRecap = {
    conclusion: "none",
    source: "history",
    sourceLabel: VERIFICATION_HISTORY_LABEL,
    reason: null,
    issues: [],
    unparsedExcerpt: null,
    total: history.verifications.length,
    rejected,
    truncated: history.truncated,
    round: null,
    detailNote: history.truncated
      ? `仅统计最近 ${history.verifications.length}/${history.total} 次验收。`
      : null,
  };
  if (!latest) return base;
  return {
    ...base,
    conclusion: recordConclusion(latest),
    reason: latest.reason && latest.reason.trim() !== "" ? latest.reason : null,
    issues: latest.guidance && !latest.issues.includes(latest.guidance)
      ? [...recordIssues(latest), latest.guidance] : recordIssues(latest),
    unparsedExcerpt: latest.parse_error
      ? latest.parse_error
      : latest.parsed
        ? null
        : latest.raw_excerpt,
    round: latest.round,
  };
}

interface TaskVerificationInput {
  stage: string;
  status: string | null;
  human_gate: string | null;
  attempts?: number;
  rejections?: number;
  verifier_issues?: string[];
  last_error?: string | null;
}

/// 回退来源：接口不可用时只能用任务快照推断结论与最近反馈。
function fromTask(task: TaskVerificationInput): VerificationRecap {
  const attempts = task.attempts ?? 0;
  const rejections = task.rejections ?? 0;
  const issues = (task.verifier_issues ?? [])
    .map((issue) => issue.trim())
    .filter((issue) => issue !== "");
  const rejected = rejections > 0;
  let conclusion: VerificationConclusion;
  if (task.human_gate === "milestone" || task.stage === "verifying") {
    conclusion = "pending";
  } else if (rejected) {
    conclusion = "rejected";
  } else if (task.stage === "completed" && attempts > 0) {
    conclusion = "passed";
  } else if (task.stage === "completed") {
    conclusion = "none";
  } else if (attempts > 0) {
    // 执行过但尚未完成验收：验收结论还没出来。
    conclusion = "pending";
  } else {
    conclusion = "none";
  }
  const lastError = (task.last_error ?? "").trim();
  return {
    conclusion,
    source: "task",
    sourceLabel: VERIFICATION_FALLBACK_LABEL,
    reason: lastError === "" ? null : lastError,
    issues,
    unparsedExcerpt: null,
    total: attempts,
    // 反馈可能是验证缺项，不据此推测拒绝次数。
    rejected: rejections,
    truncated: false,
    round: null,
    detailNote:
      "验收记录不可用，这里按任务的验收次数、拒绝次数与最近反馈汇总，没有逐轮验收原因。",
  };
}

/// 验收摘要：优先取接口记录里最近一条验收作业；接口不可用或尚未加载时回退到任务字段。
///
/// - ready：使用接口记录，空记录说明确无验收作业，结论为“未验收”。
/// - loading：接口结果未知，先按任务字段渲染，标注加载中。
/// - error：接口不可用，回退任务字段并标注回退来源。
export function verificationRecap(
  task: TaskVerificationInput,
  history: VerificationHistory | null,
  state: VerificationLoadState,
): VerificationRecap {
  if (state === "ready" && history) {
    const recap = fromHistory(history);
    return recap.total === 0 ? { ...recap, conclusion: "none" } : recap;
  }
  const fallback = fromTask(task);
  if (state === "loading") {
    return { ...fallback, sourceLabel: VERIFICATION_LOADING_LABEL };
  }
  return fallback;
}

/// 结果摘要是否有可展示的正文：空白内容按空状态处理。
export function hasSummaryText(value: string | null | undefined): boolean {
  return (value ?? "").trim() !== "";
}

/// 验收相关数据签名：任务阶段、验收计数、反馈与失败原因，以及作业快照。
/// 用于判断抽屉是否需要重新读取验收记录，避免每次轮询都请求接口。
export function verificationSignature(
  tasks: Array<{
    id: string;
    stage: string;
    status: string | null;
    human_gate: string | null;
    attempts: number;
    rejections: number;
    verifier_issues: string[];
    last_error: string | null;
  }>,
  jobs: Array<{
    id: string;
    task_id: string | null;
    role: string;
    status: string;
    attempt: number;
    finished_at: string | null;
  }>,
): string {
  const taskPart = tasks
    .map((task) =>
      [
        task.id,
        task.stage,
        task.status ?? "",
        task.human_gate ?? "",
        task.attempts,
        task.rejections,
        task.verifier_issues.join("\u0001"),
        task.last_error ?? "",
      ].join("\u0002"),
    )
    .join("\u0003");
  const jobPart = jobs
    .map((job) =>
      [job.id, job.task_id ?? "", job.role, job.status, job.attempt, job.finished_at ?? ""].join(
        "\u0002",
      ),
    )
    .join("\u0003");
  return `${taskPart}\u0004${jobPart}`;
}
