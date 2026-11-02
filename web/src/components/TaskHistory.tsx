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
  TASK_HISTORY_RESULT_EMPTY,
  TASK_HISTORY_TITLE,
  TASK_HISTORY_USAGE_UNKNOWN,
  filterFailedRounds,
  roundAnchorId,
} from "../presentation/taskHistory";
import type { HistoryJob, HistoryRound, TaskHistory } from "../presentation/taskHistory";
import { Badge, formatMillis } from "../ui";

/// 一次最多读取多少条作业详情，避免轮次很多时发起过多请求。
const DETAIL_REQUEST_LIMIT = 24;
const DETAIL_REQUEST_CONCURRENCY = 4;

/// 作业结果原文缓存：作业 id -> 结果原文。
export type JobResults = Record<string, string | null>;

/**
 * 读取各轮执行作业的结果原文。结果只在作业详情接口里，按作业 id 缓存，
 * 缓存由抽屉持有以便重新渲染时复用；已知缓存不重复请求，读取失败的作业不写入，
 * 界面按“尚未产出结果”呈现。
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
              // 读取失败按“尚未产出结果”呈现，不阻断历史。
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

/// 每轮摘要行：Agent、状态、时长与用量是否已知。
function roundMeta(job: HistoryJob): string {
  return [
    `${job.roleLabel} · ${job.agentName ?? TASK_HISTORY_NO_AGENT}`,
    job.statusLabel,
    job.durationKnown ? formatMillis(job.durationMs) : "时长未知",
    job.usageKnown ? "用量已知" : TASK_HISTORY_USAGE_UNKNOWN,
  ].join(" · ");
}

/// 跳转目标：优先最近一次验收作业，其次执行作业。
function jumpTarget(round: HistoryRound): string | null {
  return round.verifier?.id ?? round.executor?.id ?? null;
}

/**
 * 执行与验收历史：按轮次纵向排列成时间线。每轮展示 Agent、状态、时长、用量是否已知、
 * 结果摘要首行与验收结论；点击某一轮跳转到作业详情的展开状态。“只看失败”只在存在
 * 失败轮次时可用。
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
  return (
    <section className="task-history" data-drawer-block="history">
      <div className="task-result-head">
        <h3>{TASK_HISTORY_TITLE}</h3>
        <span className="muted task-history-source">{history.sourceLabel}</span>
      </div>
      {history.rounds.length === 0 ? (
        <p className="muted task-history-note">{TASK_HISTORY_EMPTY}</p>
      ) : (
        <>
          <div className="task-history-tools">
            <button
              type="button"
              className={active ? "secondary task-history-filter" : "ghost task-history-filter"}
              aria-pressed={active}
              disabled={!failedAvailable}
              title={failedAvailable ? "只保留失败轮次" : "没有失败的轮次"}
              onClick={() => setFailedOnly((current) => !current)}
            >
              {TASK_HISTORY_FILTER_LABEL}
            </button>
            <span className="muted task-history-count">
              {active
                ? `${rounds.length}/${history.rounds.length} 轮`
                : `${history.rounds.length} 轮 · 失败 ${history.failedRounds} 轮`}
            </span>
          </div>
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
  const verdictLabel = HISTORY_VERDICT_LABELS[round.verdict];
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
        <span className="task-history-round-no">第 {round.round} 轮</span>
        <Badge tone={HISTORY_VERDICT_TONES[round.verdict]}>{verdictLabel}</Badge>
        {round.running ? <Badge tone="info">进行中</Badge> : null}
        <span className="muted task-history-round-meta">
          {round.executor ? roundMeta(round.executor) : "没有执行作业"}
        </span>
        <span className="task-history-result">
          {round.resultLine ?? TASK_HISTORY_RESULT_EMPTY}
        </span>
        <span className="muted task-history-verifier">
          {round.verifier
            ? `验收 · ${round.verifier.agentName ?? TASK_HISTORY_NO_AGENT} · ${round.verifier.statusLabel}`
            : "未验收"}
        </span>
      </button>
      <div className="task-history-body">
        {[...round.executors, ...round.verifiers].map((job) => (
          <div className="task-history-job" key={job.id} data-role={job.role}>
            <span className="task-history-job-role">{job.roleLabel}</span>
            <Badge tone={job.tone}>{job.statusLabel}</Badge>
            <span className="muted">{job.agentName ?? TASK_HISTORY_NO_AGENT}</span>
            <span className="muted">
              {job.durationKnown ? formatMillis(job.durationMs) : "时长未知"}
            </span>
            <span className="muted">
              {job.usageKnown ? "用量已知" : TASK_HISTORY_USAGE_UNKNOWN}
            </span>
            <span className="task-history-job-line">
              {job.resultLine ?? TASK_HISTORY_RESULT_EMPTY}
            </span>
            <button
              type="button"
              className="ghost task-history-jump"
              onClick={() => onJumpToJob(job.id)}
            >
              {TASK_HISTORY_JUMP_LABEL}
            </button>
          </div>
        ))}
        {round.reason ? <p className="task-history-reason">验收原因：{round.reason}</p> : null}
      </div>
    </li>
  );
}
