/// 执行与验收历史的呈现模型：纯数据推导，渲染层只负责排版。
///
/// 数据来自项目详情 jobs（该任务全部执行者与验收者作业）与验收记录接口的逐轮记录，
/// 两者按轮次对齐成纵向时间线。轮次对齐规则：
///
/// - 一轮由一次执行作业与其后的验收作业组成；同一轮内的续跑与重试验收都保留，
///   最近一次作为该轮代表。
/// - 验收轮次优先取验收记录接口的序号：接口按创建顺序逐条编号，序号相对最早一条
///   记录；记录被截断时更早的轮次没有接口记录，序号无从得知。
/// - 缺少序号（接口未就绪、记录截断或作业缺失）时按时间顺序对齐：执行作业按创建
///   时间依次进入新的一轮，验收作业归属最近一次尚未验收的执行轮。
/// - 没有执行作业的验收作业单独成轮；只有执行的轮次验收结论为“未验收”。
/// - 输入顺序无关：函数内部按创建时间与 id 排序后再对齐。

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

/// 验收结论：通过、拒绝、待验收（验收作业未结束或结论待定）、未验收（没有验收作业）。
export type HistoryVerdict = "passed" | "rejected" | "pending" | "none";

export interface HistoryRound {
  /// 轮次序号，从 1 开始；只有验收的轮次也占用一个序号。
  round: number;
  /// 本轮全部执行作业，最近的排在最后。
  executors: HistoryJob[];
  /// 本轮全部验收作业，最近的排在最后。
  verifiers: HistoryJob[];
  /// 最近一次执行为本轮代表；只有验收的轮次为 null。
  executor: HistoryJob | null;
  /// 最近一次验收为本轮代表；没有验收作业时为 null。
  verifier: HistoryJob | null;
  /// 执行结果摘要首行；作业结果尚未读出或没有结果时为 null。
  resultLine: string | null;
  verdict: HistoryVerdict;
  /// 验收原因；没有可读原因时为 null。
  reason: string | null;
  /// 本轮是否失败：执行失败、验收拒绝，或验收作业失败且没有结论。
  failed: boolean;
  /// 本轮是否有作业仍在排队或运行。
  running: boolean;
}

export type TaskHistorySource = "aligned" | "jobs-only" | "empty";

export interface TaskHistory {
  rounds: HistoryRound[];
  /// aligned：接口记录与作业按轮次对齐；jobs-only：接口不可用或尚未读出，只按作业时间对齐。
  source: TaskHistorySource;
  /// 来源说明，直接展示给用户；没有轮次时为空。
  sourceLabel: string;
  failedRounds: number;
}

/// 作业 id -> 作业结果原文；用于各轮的“结果摘要首行”。
export type JobResultMap = Record<string, string | null | undefined>;

export const TASK_HISTORY_TITLE = "执行与验收历史";
export const TASK_HISTORY_FILTER_LABEL = "只看失败";
export const TASK_HISTORY_EMPTY = "还没有执行或验收作业。";
export const TASK_HISTORY_FILTER_EMPTY = "没有失败的轮次。";
export const TASK_HISTORY_ALIGNED_LABEL = "来源：作业与验收记录（按轮次对齐）";
export const TASK_HISTORY_JOBS_LABEL = "来源：作业记录（验收记录未读出）";
export const TASK_HISTORY_USAGE_UNKNOWN = "用量未知";
export const TASK_HISTORY_RESULT_EMPTY = "尚未产出结果";
export const TASK_HISTORY_NO_AGENT = "无 Agent";
export const TASK_HISTORY_JUMP_LABEL = "查看作业";

export const HISTORY_VERDICT_LABELS: Record<HistoryVerdict, string> = {
  passed: "通过",
  rejected: "拒绝",
  pending: "待验收",
  none: "未验收",
};

export const HISTORY_VERDICT_TONES: Record<
  HistoryVerdict,
  "success" | "danger" | "info" | "muted"
> = {
  passed: "success",
  rejected: "danger",
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

function recordVerdict(record: VerificationRecord): HistoryVerdict {
  if (!RUNNING_STATUSES.has(record.status)) {
    if (record.passed === true) return "passed";
    if (record.passed === false) return "rejected";
  }
  return "pending";
}

function jobVerdict(job: JobSummary): HistoryVerdict {
  if (RUNNING_STATUSES.has(job.status)) return "pending";
  if (FAILED_STATUSES.has(job.status)) return "rejected";
  return "pending";
}

function cleanReason(value: string | null | undefined): string | null {
  const trimmed = (value ?? "").trim();
  return trimmed === "" ? null : trimmed;
}

/// 验收记录接口按创建顺序逐条编号，序号相对最早一条记录；记录被截断时更早的
/// 轮次没有接口记录，它们的序号无从得知。返回作业 id -> 轮次序号。
export function recordRounds(history: VerificationHistory | null): Map<string, number> {
  const rounds = new Map<string, number>();
  if (!history) return rounds;
  const ordered = [...history.verifications].sort((left, right) => {
    if (left.round !== right.round) return left.round - right.round;
    return left.id < right.id ? -1 : left.id > right.id ? 1 : 0;
  });
  if (ordered.length === 0) return rounds;
  const first = ordered[0].round;
  // 序号逐条递增说明接口只截掉了前面的轮次，序号按偏移平移回任务轮次。
  const offset = first - 1;
  ordered.forEach((record, index) => {
    const sequential = record.round === first + index;
    rounds.set(record.id, sequential ? offset + index + 1 : record.round);
  });
  return rounds;
}

interface AlignedRound {
  round: number;
  executors: JobSummary[];
  verifiers: JobSummary[];
}

function newRound(round: number): AlignedRound {
  return { round, executors: [], verifiers: [] };
}

/// 执行作业成轮：已有序号的并入同号轮次，其余按时间顺序进入新轮次。
function executorRounds(
  executors: JobSummary[],
  knownRounds: Map<string, number>,
): AlignedRound[] {
  const ordered: AlignedRound[] = [];
  for (const executor of executors) {
    const known = knownRounds.get(executor.id);
    const target =
      known === undefined ? undefined : ordered.find((entry) => entry.round === known);
    if (target) {
      target.executors.push(executor);
    } else {
      ordered.push({ round: known ?? 0, executors: [executor], verifiers: [] });
    }
  }
  const taken = new Set<string | number>(
    ordered.filter((entry) => entry.round > 0).map((entry) => entry.round),
  );
  for (const entry of ordered) {
    if (entry.round > 0) continue;
    let next = 1;
    while (taken.has(next)) next += 1;
    entry.round = next;
    taken.add(next);
  }
  ordered.sort((left, right) => left.round - right.round);
  return ordered;
}

/// 验收作业归属：已有序号按序号；否则归属最近一次开始且尚未验收的执行轮，
/// 早于所有执行作业时归入第 1 轮。
function attachVerifiers(
  rounds: AlignedRound[],
  verifiers: JobSummary[],
  knownRounds: Map<string, number>,
): AlignedRound[] {
  const byRound = new Map<number, AlignedRound>();
  for (const entry of rounds) byRound.set(entry.round, entry);
  const ordered = [...rounds];
  for (const verifier of verifiers) {
    const known = knownRounds.get(verifier.id);
    if (known !== undefined) {
      const target = byRound.get(known);
      if (target) {
        target.verifiers.push(verifier);
      } else {
        const created = newRound(known);
        created.verifiers.push(verifier);
        byRound.set(known, created);
        ordered.push(created);
      }
      continue;
    }
    let candidate: AlignedRound | null = null;
    for (const entry of ordered) {
      if (entry.executors.length === 0) continue;
      if (sortKey(entry.executors[0]) > sortKey(verifier)) break;
      candidate = entry;
    }
    if (candidate === null) {
      const first = byRound.get(1) ?? ordered[0];
      if (first) {
        first.verifiers.push(verifier);
      } else {
        const created = newRound(1);
        created.verifiers.push(verifier);
        byRound.set(1, created);
        ordered.push(created);
      }
      continue;
    }
    // 同一执行轮已验收过时仍归入该轮，保留重试验收的历史。
    candidate.verifiers.push(verifier);
  }
  return [...ordered].sort((left, right) => left.round - right.round);
}

function buildRound(
  entry: AlignedRound,
  records: Map<string, VerificationRecord>,
  results: JobResultMap,
): HistoryRound {
  const executorJob = entry.executors[entry.executors.length - 1] ?? null;
  const verifierJob = entry.verifiers[entry.verifiers.length - 1] ?? null;
  const record = verifierJob ? records.get(verifierJob.id) : undefined;
  const verdict: HistoryVerdict = verifierJob
    ? record
      ? recordVerdict(record)
      : jobVerdict(verifierJob)
    : "none";
  const reason = verifierJob ? verifierReason(verifierJob, record, results) : null;
  const executorFailed =
    executorJob !== null && FAILED_STATUSES.has(executorJob.status);
  const verifierFailed =
    verifierJob !== null &&
    FAILED_STATUSES.has(verifierJob.status) &&
    verdict !== "rejected";
  return {
    round: entry.round,
    executors: entry.executors.map((job) =>
      toHistoryJob(job, summaryLine(results[job.id])),
    ),
    verifiers: entry.verifiers.map((job) =>
      toHistoryJob(job, verifierReason(job, records.get(job.id), results)),
    ),
    executor: executorJob
      ? toHistoryJob(executorJob, summaryLine(results[executorJob.id]))
      : null,
    verifier: verifierJob
      ? toHistoryJob(verifierJob, verifierReason(verifierJob, record, results))
      : null,
    resultLine: executorJob ? summaryLine(results[executorJob.id]) : null,
    verdict,
    reason,
    failed: verdict === "rejected" || executorFailed || verifierFailed,
    running:
      entry.executors.some((job) => RUNNING_STATUSES.has(job.status)) ||
      entry.verifiers.some((job) => RUNNING_STATUSES.has(job.status)),
  };
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

/// 对齐全部轮次。results 提供各作业的结果原文，只影响展示文本。
export function alignHistory(
  jobs: JobSummary[],
  history: VerificationHistory | null = null,
  results: JobResultMap = {},
): HistoryRound[] {
  const taskJobs = jobs.filter((job) => isTaskRole(job.role)).sort(compareJobs);
  const executors = taskJobs.filter((job) => job.role === "executor");
  const verifiers = taskJobs.filter((job) => job.role === "verifier");
  const records = new Map<string, VerificationRecord>();
  for (const record of history?.verifications ?? []) records.set(record.id, record);
  const knownRounds = recordRounds(history);
  const rounds = attachVerifiers(
    executorRounds(executors, knownRounds),
    verifiers,
    knownRounds,
  );
  return rounds.map((entry) => buildRound(entry, records, results));
}

/// 执行与验收历史：把该任务全部执行者与验收者作业按轮次对齐。
///
/// - state 为 ready 时用接口序号校正轮次；加载中或读取失败时只按作业时间对齐。
/// - jobs 只应包含该任务的作业，输入顺序不影响结果。
/// - results 是作业 id 到结果原文的映射，用于各轮“结果摘要首行”。
export function taskHistory(
  jobs: JobSummary[],
  history: VerificationHistory | null,
  state: "loading" | "ready" | "error",
  results: JobResultMap = {},
): TaskHistory {
  const rounds = alignHistory(jobs, state === "ready" ? history : null, results);
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
  };
}

/// “只看失败”筛选：失败轮次包含执行失败、验收拒绝与验收作业失败。
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
