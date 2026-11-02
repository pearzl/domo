/// 执行与验收历史：按执行轮展示，轮内展开本轮的全部验收尝试。
///
/// 人工核对清单：
/// - 用“一次执行多次验收异常”的任务记录打开任务抽屉：只有一个轮次，轮内列出 3 次验收尝试。
/// - 轮次标题行显示“第 N 轮”、执行结论徽标与“验收 X 次 · 失败 Y 次 · 最近…”汇总。
/// - 执行结果首行缺失时显示“本次执行未产出结果”，不出现与验收混淆的“尚未产出结果”。
/// - 验收异常重试的尝试徽标为危险色调，原因首行逐条可读。
/// - 存在未关联执行轮的验收作业时，轮次列表上方出现数量与最近一次状态的提示，点击可跳转。
/// - 720px 及以下窗口宽度下轮次头与尝试行不横向溢出。

import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import {
  HISTORY_VERDICT_LABELS,
  HISTORY_VERDICT_TONES,
  TASK_HISTORY_EMPTY,
  TASK_HISTORY_FILTER_EMPTY,
  TASK_HISTORY_FILTER_LABEL,
  TASK_HISTORY_JUMP_LABEL,
  TASK_HISTORY_NO_AGENT,
  TASK_HISTORY_TITLE,
  filterFailedRounds,
  roundAnchorId,
  unlinkedVerifiersLabel,
} from "../presentation/taskHistory";
import type { HistoryJob, HistoryRound, TaskHistory } from "../presentation/taskHistory";
import { Badge } from "../ui";

/// 一次最多读取多少条作业详情，避免轮次很多时发起过多请求。
const DETAIL_REQUEST_LIMIT = 24;
const DETAIL_REQUEST_CONCURRENCY = 4;

/// 作业结果原文缓存：作业 id -> 结果原文。
export type JobResults = Record<string, string | null>;

/**
 * 读取各轮执行作业的结果原文。结果只在作业详情接口里，按作业 id 缓存，
 * 缓存由抽屉持有以便重新渲染时复用；已知缓存不重复请求，读取失败的作业不写入，
 * 界面按“本次执行未产出结果”呈现。
 */
export function useJobResults(
  jobIds: string[],
  known: JobResults,
  onLoaded: (results: JobResults) => void,
  version = 0,
): JobResults {
  const requestRef = useRef(0);
  const signature = jobIds.join(",");

  useEffect(() => {
    requestRef.current += 1;
    const token = requestRef.current;
    // 没有结果原文的作业在快照刷新后重试，运行中作业结束时能补上结果。
    const pending = jobIds
      .filter((id) => id !== "" && known[id] == null)
      .slice(0, DETAIL_REQUEST_LIMIT);
    if (pending.length === 0) return;
    let cancelled = false;
    async function load(queue: string[]) {
      const loaded: JobResults = {};
      while (queue.length > 0 && !cancelled && token === requestRef.current) {
        const batch = queue.splice(0, DETAIL_REQUEST_CONCURRENCY);
        await Promise.all(
          batch.map(async (id) => {
            try {
              const detail = await api.getJob(id);
              loaded[id] = detail.result;
            } catch {
              // 读取失败按“本次执行未产出结果”呈现，不阻断历史。
            }
          }),
        );
      }
      if (!cancelled && token === requestRef.current && Object.keys(loaded).length > 0) {
        onLoaded(loaded);
      }
    }
    void load([...pending]);
    return () => {
      cancelled = true;
    };
    // 缓存只作为“是否已读取”的判据，作业集合或项目快照变化才重新读取。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [signature, version]);

  return known;
}

/// 本轮验收汇总：尝试次数、失败次数与最近结论。
function verifierSummary(round: HistoryRound): string {
  const { total, failed } = round.verifierAttempts;
  if (total === 0) return HISTORY_VERDICT_LABELS.none;
  const parts = [`验收 ${total} 次`];
  if (failed > 0) parts.push(`失败 ${failed} 次`);
  parts.push(`最近${HISTORY_VERDICT_LABELS[round.verdict]}`);
  return parts.join(" · ");
}

/// 单次验收尝试是否失败：拒绝或验收异常。
function attemptFailed(job: HistoryJob): boolean {
  return job.verdict === "rejected" || job.verdict === "error";
}

/// 跳转目标：优先最近一次验收作业，其次执行作业。
function jumpTarget(round: HistoryRound): string | null {
  return round.verifier?.id ?? round.executor?.id ?? null;
}

/**
 * 执行与验收历史：按执行轮纵向排列成时间线。每轮首行展示轮次编号、执行结论与验收汇总，
 * 次行展示本轮执行结果首行；展开后列出本轮全部验收尝试。“只看问题轮”只在存在问题时可用。
 */
export function TaskHistoryBlock({
  history,
  onJumpToJob,
}: {
  history: TaskHistory;
  onJumpToJob: (jobId: string) => void;
}) {
  const [failedOnly, setFailedOnly] = useState(false);
  const failedAvailable = history.failedRounds > 0;
  const active = failedOnly && failedAvailable;
  const rounds = filterFailedRounds(history.rounds, active);
  const unlinkedJobs = history.unlinkedJobs ?? [];
  const unlinkedLatest = unlinkedJobs[unlinkedJobs.length - 1] ?? null;
  const countLabel = active
    ? `显示 ${rounds.length} / ${history.rounds.length} 个执行轮次 · 失败 ${history.failedRounds} 轮`
    : `${history.rounds.length} 个执行轮次 · 失败 ${history.failedRounds} 轮`;
  const unlinkedNotice =
    history.unlinkedVerifiers > 0 ? (
      <button
        type="button"
        className="ghost task-history-unlinked"
        title="跳转到最近一次未关联执行轮的验收作业"
        onClick={() => {
          if (unlinkedLatest) onJumpToJob(unlinkedLatest.id);
        }}
      >
        <span className="task-history-unlinked-text">
          {unlinkedVerifiersLabel(history.unlinkedVerifiers)}
        </span>
        {unlinkedLatest ? (
          <span className="task-history-unlinked-state">
            <span className="muted">最近一次</span>
            <Badge tone={unlinkedLatest.tone}>{unlinkedLatest.statusLabel}</Badge>
            <Badge tone={HISTORY_VERDICT_TONES[unlinkedLatest.verdict]}>
              {HISTORY_VERDICT_LABELS[unlinkedLatest.verdict]}
            </Badge>
          </span>
        ) : null}
      </button>
    ) : null;
  return (
    <section className="task-history" data-drawer-block="history">
      <div className="task-result-head">
        <h3>{TASK_HISTORY_TITLE}</h3>
        <span className="muted task-history-source">{history.sourceLabel}</span>
      </div>
      {history.rounds.length === 0 ? (
        <>
          {unlinkedNotice}
          <p className="muted task-history-note">{TASK_HISTORY_EMPTY}</p>
        </>
      ) : (
        <>
          <div className="task-history-tools">
            <button
              type="button"
              className={active ? "secondary task-history-filter" : "ghost task-history-filter"}
              aria-pressed={active}
              disabled={!failedAvailable}
              title={failedAvailable ? "只保留问题轮次" : "没有失败的轮次"}
              onClick={() => setFailedOnly((current) => !current)}
            >
              {TASK_HISTORY_FILTER_LABEL}
            </button>
            <span className="muted task-history-count">{countLabel}</span>
          </div>
          {unlinkedNotice}
          {rounds.length === 0 ? (
            <p className="muted task-history-note">{TASK_HISTORY_FILTER_EMPTY}</p>
          ) : (
            <ol className="task-history-rounds">
              {rounds.map((round) => (
                <RoundItem key={round.round} round={round} onJumpToJob={onJumpToJob} />
              ))}
            </ol>
          )}
        </>
      )}
    </section>
  );
}

function RoundItem({
  round,
  onJumpToJob,
}: {
  round: HistoryRound;
  onJumpToJob: (jobId: string) => void;
}) {
  const target = jumpTarget(round);
  return (
    <li
      className={`task-history-round${round.failed ? " task-history-failed" : ""}`}
      id={roundAnchorId(round.round)}
      data-round={round.round}
      data-failed={round.failed ? "true" : "false"}
    >
      <button
        type="button"
        className="task-history-round-head"
        title="跳转到作业详情的展开状态"
        disabled={target === null}
        onClick={() => {
          if (target) onJumpToJob(target);
        }}
      >
        <span className="task-history-round-title">
          <span className="task-history-round-no">第 {round.round} 轮</span>
          {round.executor ? (
            <Badge tone={round.executor.tone}>{round.executor.statusLabel}</Badge>
          ) : (
            <Badge tone="muted">没有执行作业</Badge>
          )}
          {round.running ? <Badge tone="info">进行中</Badge> : null}
          <span className="task-history-round-summary">{verifierSummary(round)}</span>
        </span>
        <span className="task-history-round-result">{round.resultText}</span>
      </button>
      <div className="task-history-body">
        <div className="task-history-attempts-head">
          <span className="task-history-attempts-title">本轮验收</span>
          <span className="muted">{round.verifierAttempts.total} 次尝试</span>
        </div>
        {round.verifiers.length === 0 ? (
          <p className="muted task-history-attempt-empty">没有验收作业。</p>
        ) : (
          <ol className="task-history-attempts">
            {round.verifiers.map((job, index) => {
              const failed = attemptFailed(job);
              return (
                <li
                  className="task-history-attempt"
                  key={job.id}
                  data-failed={failed ? "true" : "false"}
                >
                  <span className="task-history-attempt-no">尝试 {index + 1}</span>
                  <Badge tone={HISTORY_VERDICT_TONES[job.verdict]}>
                    {HISTORY_VERDICT_LABELS[job.verdict]}
                  </Badge>
                  <span className="task-history-attempt-agent">
                    {job.agentName ?? TASK_HISTORY_NO_AGENT}
                  </span>
                  <span className="task-history-attempt-reason">
                    {job.resultLine ?? "没有验收原因"}
                  </span>
                  <button
                    type="button"
                    className="ghost task-history-jump"
                    onClick={() => onJumpToJob(job.id)}
                  >
                    {TASK_HISTORY_JUMP_LABEL}
                  </button>
                </li>
              );
            })}
          </ol>
        )}
      </div>
    </li>
  );
}
