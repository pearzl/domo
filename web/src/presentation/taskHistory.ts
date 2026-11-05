/// 执行与验收历史按服务端保存的 execution_job_id 关联，不按时间或列表截断推测。
/// 每个执行作业一轮，验收重试保留在同一轮；缺少关联时明确提示而不猜测。

import type { JobSummary, VerificationHistory, VerificationRecord } from "../types";
import { jobStatusLabel, toneForStatus } from "../ui";

export type { VerificationHistory, VerificationRecord } from "../types";

export type HistoryRole = "executor" | "verifier";

/// 轮次里单个作业的展示字段。
export interface HistoryJob {
  id: string;
  role: HistoryRole;
  /// 角色文案：执行 / 验收。
  roleLabel: string;
  agentName: string | null;
  status: string;
  statusLabel: string;
  /// 状态色调，与全站状态文案同源。
  tone: "success" | "danger" | "info" | "muted" | "warn";
  durationMs: number | null;
  /// 时长是否已知；未知时不展示时长。
  durationKnown: boolean;
  /// 用量是否已知；未知时界面标注“用量未知”。
  usageKnown: boolean;
  /// 单次验收尝试的结论；执行作业为 none。
  verdict: HistoryVerdict;
  startedAt: string | null;
  finishedAt: string | null;
}

/// 验收结论及等待、决策处置；等待条件和请求决策不属于失败尝试。
export type HistoryVerdict = "passed" | "rejected" | "waiting" | "review" | "error" | "pending" | "none";

/// 同一执行轮内多次验收尝试的汇总；明细仍保留在 verifiers。
export interface VerifierAttempts {
  /// 本轮验收尝试总次数。
  total: number;
  /// 未通过的尝试次数：验收拒绝或验收作业失败。
  failed: number;
  /// 最近一次尝试；没有验收作业时为 null。
  latest: HistoryJob | null;
}

export interface HistoryRound {
  /// 执行轮次序号，从 1 开始，按执行作业创建顺序编号。
  round: number;
  /// 本轮执行作业，最近的排在最后。
  executors: HistoryJob[];
  /// 本轮全部验收尝试，按创建顺序排列。
  verifiers: HistoryJob[];
  /// 本轮最近一次执行作业。
  executor: HistoryJob | null;
  /// 本轮最近一次验收尝试；没有验收作业时为 null。
  verifier: HistoryJob | null;
  verdict: HistoryVerdict;
  /// 本轮是否出现问题：执行失败、验收拒绝或验收尝试失败。
  failed: boolean;
  /// 本轮是否有作业仍在排队或运行。
  running: boolean;
  /// 本轮多次验收尝试的汇总。
  verifierAttempts: VerifierAttempts;
}

export type TaskHistorySource = "aligned" | "jobs-only" | "empty";

export interface TaskHistory {
  rounds: HistoryRound[];
  /// aligned：已读取验收结论；jobs-only：验收结论尚未读出。两者均使用显式执行引用分组。
  source: TaskHistorySource;
  /// 来源说明，直接展示给用户；没有轮次时为空。
  sourceLabel: string;
  failedRounds: number;
  /// 没有可关联执行轮的验收作业数；这些作业不占用轮次。
  unlinkedVerifiers: number;
  /// 没有可关联执行轮的验收作业，按创建顺序排列；用于提示行与跳转。
  unlinkedJobs: HistoryJob[];
}

/// 一个执行轮及其验收尝试；unlinked 中的验收作业没有可关联的执行轮。
export interface ExecutionRoundGroup {
  round: number;
  executors: JobSummary[];
  verifiers: JobSummary[];
}

export interface RoundGrouping {
  rounds: ExecutionRoundGroup[];
  unlinked: JobSummary[];
}

export const TASK_HISTORY_TITLE = "执行与验收历史";
/// 筛选按钮：保留存在执行失败、验收拒绝或验收尝试失败的问题轮次。
export const TASK_HISTORY_FILTER_LABEL = "只看问题轮";
export const TASK_HISTORY_EMPTY = "还没有执行或验收作业。";
export const TASK_HISTORY_FILTER_EMPTY = "没有失败的轮次。";
export const TASK_HISTORY_ALIGNED_LABEL = "来源：作业与验收记录（按执行作业关联）";
export const TASK_HISTORY_JOBS_LABEL = "来源：作业记录（验收记录未读出）";
export const TASK_HISTORY_USAGE_UNKNOWN = "用量未知";
/// 未关联执行轮的验收作业提示模板；{count} 由 unlinkedVerifiersLabel 填充。
export const TASK_HISTORY_UNLINKED_VERIFIERS = "有 {count} 次验收作业没有可关联的执行轮";
export const TASK_HISTORY_NO_AGENT = "无 Agent";
export const TASK_HISTORY_JUMP_LABEL = "查看作业";

/// 未关联执行轮的验收作业提示文案。
export function unlinkedVerifiersLabel(count: number): string {
  return TASK_HISTORY_UNLINKED_VERIFIERS.replace("{count}", String(count));
}

export const HISTORY_VERDICT_LABELS: Record<HistoryVerdict, string> = {
  passed: "通过",
  rejected: "拒绝",
  waiting: "等待验证条件",
  review: "请求上级决策",
  error: "验收异常",
  pending: "待验收",
  none: "未验收",
};

export const HISTORY_VERDICT_TONES: Record<
  HistoryVerdict,
  "success" | "danger" | "info" | "muted"
> = {
  passed: "success",
  rejected: "danger",
  waiting: "info",
  review: "info",
  error: "danger",
  pending: "info",
  none: "muted",
};

/// 仍在进行中的作业状态。
const RUNNING_STATUSES = new Set(["queued", "running", "pausing"]);
/// 失败的作业状态：执行失败与已取消。
const FAILED_STATUSES = new Set(["failed", "canceled"]);

/// 进入历史的作业角色；项目分析、规划与总结作业不属于任务历史。
function isTaskRole(role: string): role is HistoryRole {
  return role === "executor" || role === "verifier";
}

function sortKey(job: JobSummary): string {
  return job.created_at || job.started_at || "";
}

function compareJobs(left: JobSummary, right: JobSummary): number {
  const leftKey = sortKey(left);
  const rightKey = sortKey(right);
  if (leftKey !== rightKey) return leftKey < rightKey ? -1 : 1;
  return left.id < right.id ? -1 : left.id > right.id ? 1 : 0;
}

function toHistoryJob(
  job: JobSummary,
  verdict: HistoryVerdict = "none",
): HistoryJob {
  const role: HistoryRole = job.role === "verifier" ? "verifier" : "executor";
  return {
    id: job.id,
    role,
    roleLabel: role === "verifier" ? "验收" : "执行",
    agentName: job.agent_name,
    status: job.status,
    statusLabel: jobStatusLabel(job.status),
    tone: toneForStatus(job.status) as HistoryJob["tone"],
    durationMs: job.duration_ms,
    durationKnown: job.duration_ms !== null && job.duration_ms > 0,
    usageKnown: job.usage_known,
    verdict,
    startedAt: job.started_at,
    finishedAt: job.finished_at,
  };
}

/// 单个验收尝试的结论；只有验收结束且解析出通过或拒绝时才有结论。
function conclusiveVerdict(
  record: VerificationRecord | undefined,
): HistoryVerdict | null {
  if (!record) return null;
  if (RUNNING_STATUSES.has(record.status)) return null;
  if (record.passed === true) return "passed";
  if (record.next_action === "wait") return "waiting";
  if (record.next_action === "review") return "review";
  if (record.passed === false) return "rejected";
  return null;
}

interface VerifierAttempt {
  job: JobSummary;
  verdict: HistoryVerdict | null;
  failed: boolean;
}

function toVerifierAttempt(
  job: JobSummary,
  record: VerificationRecord | undefined,
): VerifierAttempt {
  const verdict = conclusiveVerdict(record);
  const failed =
    verdict === "rejected" ||
    (verdict === null &&
      (FAILED_STATUSES.has(job.status) ||
        (record !== undefined && FAILED_STATUSES.has(record.status))));
  return { job, verdict, failed };
}

/// 单次验收尝试的结论：有结论取通过或拒绝，失败且无结论判为验收异常，
/// 其余仍在进行或结论待定判为待验收。
function attemptVerdict(attempt: VerifierAttempt): HistoryVerdict {
  if (attempt.verdict !== null) return attempt.verdict;
  if (attempt.failed) return "error";
  return "pending";
}

/// 本轮最近一次有结论的验收尝试。
function lastConclusion(attempts: VerifierAttempt[]): VerifierAttempt | null {
  for (let index = attempts.length - 1; index >= 0; index -= 1) {
    if (attempts[index].verdict !== null) return attempts[index];
  }
  return null;
}

function roundVerdict(attempts: VerifierAttempt[]): HistoryVerdict {
  if (attempts.length === 0) return "none";
  const conclusion = lastConclusion(attempts);
  if (conclusion !== null && conclusion.verdict !== null) return conclusion.verdict;
  if (attempts.every((attempt) => attempt.failed)) return "error";
  return "pending";
}

/// 每个验收只归入其引用的执行作业，不受接口分页、时间精度或输入顺序影响。
export function groupExecutionRounds(
  executors: JobSummary[],
  verifiers: JobSummary[],
): RoundGrouping {
  const orderedExecutors = [...executors].sort(compareJobs);
  const orderedVerifiers = [...verifiers].sort(compareJobs);
  const rounds: ExecutionRoundGroup[] = orderedExecutors.map((job, index) => ({
    round: index + 1,
    executors: [job],
    verifiers: [],
  }));
  const unlinked: JobSummary[] = [];
  const byId = new Map(orderedExecutors.map((job, index) => [job.id, rounds[index]]));
  for (const verifier of orderedVerifiers) {
    const target = verifier.execution_job_id ? byId.get(verifier.execution_job_id) : undefined;
    if (target) {
      target.verifiers.push(verifier);
    } else {
      unlinked.push(verifier);
    }
  }
  return { rounds, unlinked };
}

function buildRound(
  entry: ExecutionRoundGroup,
  records: Map<string, VerificationRecord>,
): HistoryRound {
  const executorJob = entry.executors[entry.executors.length - 1] ?? null;
  const attempts = entry.verifiers.map((job) =>
    toVerifierAttempt(job, records.get(job.id)),
  );
  const attemptsById = new Map(attempts.map((attempt) => [attempt.job.id, attempt]));
  const latestAttempt = attempts[attempts.length - 1] ?? null;
  const verdict = roundVerdict(attempts);
  const executorFailed =
    executorJob !== null && FAILED_STATUSES.has(executorJob.status);
  const latestVerifier = latestAttempt
    ? toHistoryJob(latestAttempt.job, attemptVerdict(latestAttempt))
    : null;
  return {
    round: entry.round,
    executors: entry.executors.map((job) => toHistoryJob(job)),
    verifiers: entry.verifiers.map((job) => {
      const attempt = attemptsById.get(job.id);
      return toHistoryJob(job, attempt ? attemptVerdict(attempt) : "pending");
    }),
    executor: executorJob ? toHistoryJob(executorJob) : null,
    verifier: latestVerifier,
    verdict,
    failed: executorFailed || attempts.some((attempt) => attempt.failed),
    running:
      entry.executors.some((job) => RUNNING_STATUSES.has(job.status)) ||
      entry.verifiers.some((job) => RUNNING_STATUSES.has(job.status)),
    verifierAttempts: {
      total: attempts.length,
      failed: attempts.filter((attempt) => attempt.failed).length,
      latest: latestVerifier,
    },
  };
}

interface AlignedHistory {
  rounds: HistoryRound[];
  unlinkedVerifiers: number;
  unlinkedJobs: HistoryJob[];
}

/// 对齐全部轮次，只保留轮次与作业元信息。
function alignHistoryDetail(
  jobs: JobSummary[],
  history: VerificationHistory | null = null,
): AlignedHistory {
  const taskJobs = jobs.filter((job) => isTaskRole(job.role)).sort(compareJobs);
  const executors = taskJobs.filter((job) => job.role === "executor");
  const verifiers = taskJobs.filter((job) => job.role === "verifier");
  const records = new Map<string, VerificationRecord>();
  for (const record of history?.verifications ?? []) records.set(record.id, record);
  const grouping = groupExecutionRounds(executors, verifiers);
  return {
    rounds: grouping.rounds.map((entry) => buildRound(entry, records)),
    unlinkedVerifiers: grouping.unlinked.length,
    unlinkedJobs: grouping.unlinked.map((job) => {
      const record = records.get(job.id);
      const attempt = toVerifierAttempt(job, record);
      return toHistoryJob(job, attemptVerdict(attempt));
    }),
  };
}

/// 对齐全部轮次，只返回轮次。
export function alignHistory(
  jobs: JobSummary[],
  history: VerificationHistory | null = null,
): HistoryRound[] {
  return alignHistoryDetail(jobs, history).rounds;
}

/// 执行与验收历史：把该任务全部执行者与验收者作业按执行轮对齐。
///
/// - state 为 ready 时用接口轮次校正；加载中或读取失败时只按作业时间对齐。
/// - jobs 只应包含该任务的作业，输入顺序不影响结果。
export function taskHistory(
  jobs: JobSummary[],
  history: VerificationHistory | null,
  state: "loading" | "ready" | "error",
): TaskHistory {
  const aligned = alignHistoryDetail(
    jobs,
    state === "ready" ? history : null,
  );
  const rounds = aligned.rounds;
  const source: TaskHistorySource =
    rounds.length === 0 ? "empty" : state === "ready" ? "aligned" : "jobs-only";
  const sourceLabel =
    source === "aligned"
      ? TASK_HISTORY_ALIGNED_LABEL
      : source === "jobs-only"
        ? TASK_HISTORY_JOBS_LABEL
        : "";
  return {
    rounds,
    source,
    sourceLabel,
    failedRounds: rounds.filter((round) => round.failed).length,
    unlinkedVerifiers: aligned.unlinkedVerifiers,
    unlinkedJobs: aligned.unlinkedJobs,
  };
}

/// “只看问题轮”筛选：问题轮包含执行失败、验收拒绝与验收尝试失败。
export function filterFailedRounds(
  rounds: HistoryRound[],
  failedOnly: boolean,
): HistoryRound[] {
  return failedOnly ? rounds.filter((round) => round.failed) : rounds;
}

/// 时间线上每一轮的锚点 id，供“跳转到作业详情”定位。
export function roundAnchorId(round: number): string {
  return `task-round-${round}`;
}
