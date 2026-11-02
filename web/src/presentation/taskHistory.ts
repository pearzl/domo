/// 执行与验收历史的呈现模型：纯数据推导，渲染层只负责排版。
///
/// 数据来自项目详情 jobs（该任务全部执行者与验收者作业）与验收记录接口的逐轮记录。
/// 轮次对齐规则：
///
/// - 每一轮对应一个执行作业：执行作业按创建时间与 id 升序编号为第 N 轮，
///   rounds.length 恒等于执行作业数。
/// - 验收作业按创建时间归入创建时间不晚于它的最近一个执行轮；同一执行轮后的
///   多次验收（异常重试、拒绝后重新验收）都归入该轮并保留全部尝试明细。
/// - 验收记录接口的 round 是所属执行轮：>=1 时用于校正归属，0 表示没有可关联的
///   执行作业，计入 unlinkedVerifiers，不占用轮次。
/// - 早于全部执行作业的验收作业没有可关联的执行轮，同样计入 unlinkedVerifiers。
/// - 作业列表缺少最早执行作业时，按接口轮次与本地位置之差推断偏移，剩余执行作业
///   从第 1 轮继续编号，验收仍归入对应执行轮。
/// - 输入顺序无关：函数内部按创建时间与 id 排序后再对齐。
///
/// 本轮结论取最近一次有结论的验收；验收作业全部失败且没有结论时判为验收异常。
/// 本轮执行作业存在但没有结果时使用与验收文案区分的“本次执行未产出结果”。

import type { JobSummary, VerificationHistory, VerificationRecord } from "../types";

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
  /// 该作业的结果摘要首行：执行作业取结果首行，验收作业优先取验收原因。
  resultLine: string | null;
  startedAt: string | null;
  finishedAt: string | null;
}

/// 验收结论：通过、拒绝、验收异常（全部尝试失败且无结论）、待验收（尚未结束或结论待定）、
/// 未验收（没有验收作业）。
export type HistoryVerdict = "passed" | "rejected" | "error" | "pending" | "none";

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
  /// 执行结果摘要首行；作业结果尚未读出或没有结果时为 null。
  resultLine: string | null;
  /// 本轮结果展示文案：有结果取首行，没有结果时为“本次执行未产出结果”。
  resultText: string;
  verdict: HistoryVerdict;
  /// 验收原因；没有可读原因时为 null。
  reason: string | null;
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
  /// aligned：接口记录与作业按轮次对齐；jobs-only：接口不可用或尚未读出，只按作业时间对齐。
  source: TaskHistorySource;
  /// 来源说明，直接展示给用户；没有轮次时为空。
  sourceLabel: string;
  failedRounds: number;
  /// 没有可关联执行轮的验收作业数；这些作业不占用轮次。
  unlinkedVerifiers: number;
}

/// 作业 id -> 作业结果原文；用于各轮的“结果摘要首行”。
export type JobResultMap = Record<string, string | null | undefined>;

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
export const TASK_HISTORY_ALIGNED_LABEL = "来源：作业与验收记录（按轮次对齐）";
export const TASK_HISTORY_JOBS_LABEL = "来源：作业记录（验收记录未读出）";
export const TASK_HISTORY_USAGE_UNKNOWN = "用量未知";
export const TASK_HISTORY_RESULT_EMPTY = "尚未产出结果";
export const TASK_HISTORY_EXECUTION_EMPTY = "本次执行未产出结果";
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
  error: "danger",
  pending: "info",
  none: "muted",
};

const JOB_STATUS_LABELS: Record<string, string> = {
  queued: "排队中",
  running: "运行中",
  succeeded: "成功",
  failed: "失败",
  canceled: "已取消",
  completed: "已完成",
  pausing: "暂停中",
  paused: "已暂停",
};

const JOB_STATUS_TONES: Record<string, HistoryJob["tone"]> = {
  queued: "warn",
  running: "info",
  succeeded: "success",
  completed: "success",
  failed: "danger",
  canceled: "muted",
  pausing: "warn",
  paused: "warn",
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

function toHistoryJob(job: JobSummary, resultLine: string | null): HistoryJob {
  const role: HistoryRole = job.role === "verifier" ? "verifier" : "executor";
  return {
    id: job.id,
    role,
    roleLabel: role === "verifier" ? "验收" : "执行",
    agentName: job.agent_name,
    status: job.status,
    statusLabel: JOB_STATUS_LABELS[job.status] ?? job.status,
    tone: JOB_STATUS_TONES[job.status] ?? "muted",
    durationMs: job.duration_ms,
    durationKnown: job.duration_ms !== null && job.duration_ms > 0,
    usageKnown: job.usage_known,
    resultLine,
    startedAt: job.started_at,
    finishedAt: job.finished_at,
  };
}

/// 结果摘要首行：取第一个非空行、折叠空白、去掉 Markdown 标题与列表标记，
/// 并截断到 max 个字符。
export function summaryLine(
  value: string | null | undefined,
  max = 120,
): string | null {
  for (const line of (value ?? "").split(/\r?\n/)) {
    const collapsed = line
      .replace(/^\s*(?:#{1,6}|>|[-*+]|\d+\.)\s+/, "")
      .split(/\s+/)
      .filter(Boolean)
      .join(" ");
    if (collapsed === "") continue;
    const chars = Array.from(collapsed);
    return chars.length <= max ? collapsed : `${chars.slice(0, max - 1).join("")}…`;
  }
  return null;
}

/// 单个验收尝试的结论；只有验收结束且解析出通过或拒绝时才有结论。
function conclusiveVerdict(
  record: VerificationRecord | undefined,
): "passed" | "rejected" | null {
  if (!record) return null;
  if (RUNNING_STATUSES.has(record.status)) return null;
  if (record.passed === true) return "passed";
  if (record.passed === false) return "rejected";
  return null;
}

function cleanReason(value: string | null | undefined): string | null {
  const trimmed = (value ?? "").trim();
  return trimmed === "" ? null : trimmed;
}

/// 验收记录接口的 round 是所属执行轮：>=1 直接作为轮次，0 表示没有可关联的执行作业。
/// 返回作业 id -> 接口轮次。
export function recordRounds(history: VerificationHistory | null): Map<string, number> {
  const rounds = new Map<string, number>();
  for (const record of history?.verifications ?? []) rounds.set(record.id, record.round);
  return rounds;
}

interface VerifierAttempt {
  job: JobSummary;
  record: VerificationRecord | undefined;
  verdict: "passed" | "rejected" | null;
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
  return { job, record, verdict, failed };
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

/// 验收原因：优先取验收记录解析出的原因与原文摘要，其次取验收作业结果的首行，
/// 最后回退到作业失败错误的首行。
function verifierReason(
  verifier: JobSummary,
  record: VerificationRecord | undefined,
  results: JobResultMap,
): string | null {
  return (
    cleanReason(record?.reason) ??
    summaryLine(record?.raw_excerpt) ??
    cleanReason(record?.parse_error) ??
    summaryLine(results[verifier.id]) ??
    summaryLine(verifier.error)
  );
}

/// 本地执行作业中，创建时间不晚于该验收作业的作业数；0 表示验收早于全部执行作业。
function verifierPosition(executors: JobSummary[], verifier: JobSummary): number {
  const key = sortKey(verifier);
  let position = 0;
  for (const executor of executors) {
    if (sortKey(executor) > key) break;
    position += 1;
  }
  return position;
}

/// 作业列表缺少最早执行作业时，接口轮次与本地位置之差即为偏移；取出现最多的差值。
function truncationOffset(
  executors: JobSummary[],
  verifiers: JobSummary[],
  knownRounds: Map<string, number>,
): number {
  const counts = new Map<number, number>();
  for (const verifier of verifiers) {
    const known = knownRounds.get(verifier.id);
    if (known === undefined || known < 1) continue;
    const position = verifierPosition(executors, verifier);
    if (position < 1) continue;
    const offset = known - position;
    counts.set(offset, (counts.get(offset) ?? 0) + 1);
  }
  let selected = 0;
  let selectedCount = 0;
  for (const [offset, count] of counts) {
    const closer = Math.abs(offset) < Math.abs(selected);
    if (count > selectedCount || (count === selectedCount && closer)) {
      selected = offset;
      selectedCount = count;
    }
  }
  return selectedCount === 0 ? 0 : selected;
}

/// 按执行作业分组：执行作业按创建时间与 id 升序编号，验收作业以接口轮次校正、
/// 缺少接口轮次时按创建时间归入最近一个执行轮；无法归属的验收作业进入 unlinked。
export function groupExecutionRounds(
  executors: JobSummary[],
  verifiers: JobSummary[],
  knownRounds: Map<string, number> = new Map(),
): RoundGrouping {
  const orderedExecutors = [...executors].sort(compareJobs);
  const orderedVerifiers = [...verifiers].sort(compareJobs);
  const offset = truncationOffset(orderedExecutors, orderedVerifiers, knownRounds);
  const rounds: ExecutionRoundGroup[] = orderedExecutors.map((job, index) => ({
    round: index + 1,
    executors: [job],
    verifiers: [],
  }));
  const unlinked: JobSummary[] = [];
  for (const verifier of orderedVerifiers) {
    const known = knownRounds.get(verifier.id);
    const position = verifierPosition(orderedExecutors, verifier);
    const target = known === undefined ? position : known >= 1 ? known - offset : 0;
    if (target >= 1 && target <= rounds.length) {
      rounds[target - 1].verifiers.push(verifier);
    } else {
      unlinked.push(verifier);
    }
  }
  return { rounds, unlinked };
}

function buildRound(
  entry: ExecutionRoundGroup,
  records: Map<string, VerificationRecord>,
  results: JobResultMap,
): HistoryRound {
  const executorJob = entry.executors[entry.executors.length - 1] ?? null;
  const attempts = entry.verifiers.map((job) =>
    toVerifierAttempt(job, records.get(job.id)),
  );
  const latestAttempt = attempts[attempts.length - 1] ?? null;
  const conclusion = lastConclusion(attempts);
  const latestFailed = [...attempts].reverse().find((attempt) => attempt.failed) ?? null;
  const verdict = roundVerdict(attempts);
  const reasonAttempt =
    verdict === "error" ? latestFailed : conclusion ?? latestAttempt;
  const reason =
    reasonAttempt === null
      ? null
      : verdict === "error"
        ? cleanReason(reasonAttempt.job.error) ??
          verifierReason(reasonAttempt.job, reasonAttempt.record, results)
        : verifierReason(reasonAttempt.job, reasonAttempt.record, results);
  const resultLine = executorJob ? summaryLine(results[executorJob.id]) : null;
  const executorFailed =
    executorJob !== null && FAILED_STATUSES.has(executorJob.status);
  const latestVerifier = latestAttempt
    ? toHistoryJob(
        latestAttempt.job,
        verifierReason(latestAttempt.job, latestAttempt.record, results),
      )
    : null;
  return {
    round: entry.round,
    executors: entry.executors.map((job) =>
      toHistoryJob(job, summaryLine(results[job.id])),
    ),
    verifiers: entry.verifiers.map((job) =>
      toHistoryJob(job, verifierReason(job, records.get(job.id), results)),
    ),
    executor: executorJob ? toHistoryJob(executorJob, resultLine) : null,
    verifier: latestVerifier,
    resultLine,
    resultText: resultLine ?? TASK_HISTORY_EXECUTION_EMPTY,
    verdict,
    reason,
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
}

/// 对齐全部轮次。results 提供各作业的结果原文，只影响展示文本。
function alignHistoryDetail(
  jobs: JobSummary[],
  history: VerificationHistory | null = null,
  results: JobResultMap = {},
): AlignedHistory {
  const taskJobs = jobs.filter((job) => isTaskRole(job.role)).sort(compareJobs);
  const executors = taskJobs.filter((job) => job.role === "executor");
  const verifiers = taskJobs.filter((job) => job.role === "verifier");
  const records = new Map<string, VerificationRecord>();
  for (const record of history?.verifications ?? []) records.set(record.id, record);
  const grouping = groupExecutionRounds(executors, verifiers, recordRounds(history));
  return {
    rounds: grouping.rounds.map((entry) => buildRound(entry, records, results)),
    unlinkedVerifiers: grouping.unlinked.length,
  };
}

/// 对齐全部轮次，只返回轮次。
export function alignHistory(
  jobs: JobSummary[],
  history: VerificationHistory | null = null,
  results: JobResultMap = {},
): HistoryRound[] {
  return alignHistoryDetail(jobs, history, results).rounds;
}

/// 执行与验收历史：把该任务全部执行者与验收者作业按执行轮对齐。
///
/// - state 为 ready 时用接口轮次校正；加载中或读取失败时只按作业时间对齐。
/// - jobs 只应包含该任务的作业，输入顺序不影响结果。
/// - results 是作业 id 到结果原文的映射，用于各轮“结果摘要首行”。
export function taskHistory(
  jobs: JobSummary[],
  history: VerificationHistory | null,
  state: "loading" | "ready" | "error",
  results: JobResultMap = {},
): TaskHistory {
  const aligned = alignHistoryDetail(
    jobs,
    state === "ready" ? history : null,
    results,
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
