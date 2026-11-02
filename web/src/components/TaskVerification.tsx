import { useEffect, useRef, useState } from "react";
import { api, errorMessage } from "../api";
import type { JobSummary, VerificationHistory } from "../types";
import {
  RESULT_SUMMARY_EMPTY,
  RESULT_SUMMARY_HINT,
  VERIFICATION_CONCLUSIONS,
  VERIFICATION_RECAP_EMPTY_HINT,
  VERIFICATION_RECAP_TITLE,
  hasSummaryText,
  resultSource,
} from "../presentation/taskSummary";
import type {
  ResultSource,
  VerificationLoadState,
  VerificationRecap,
} from "../presentation/taskSummary";
import { Badge, formatTime } from "../ui";
import { RichContent } from "./RichContent";

/// 验收记录读取：结果未知时加载中，失败时保留错误供呈现层回退。
export interface VerificationHistoryState {
  history: VerificationHistory | null;
  state: VerificationLoadState;
  error: string;
}

/**
 * 读取任务的逐轮验收记录。验收作业结束后结论与计数会变化，调用方用 version
 * 标记项目快照的刷新版本，快照变化时重新读取。
 */
export function useVerificationHistory(
  projectId: string,
  taskId: string,
  version: number,
): VerificationHistoryState {
  const [state, setState] = useState<VerificationHistoryState>({
    history: null,
    state: "loading",
    error: "",
  });
  // 请求序号：过期响应直接丢弃，避免旧数据覆盖新快照。
  const requestRef = useRef(0);

  useEffect(() => {
    let cancelled = false;
    requestRef.current += 1;
    const token = requestRef.current;
    setState((current) => ({ ...current, state: "loading" }));
    api
      .getTaskVerifications(projectId, taskId)
      .then((history) => {
        if (cancelled || token !== requestRef.current) return;
        setState({ history, state: "ready", error: "" });
      })
      .catch((err: unknown) => {
        if (cancelled || token !== requestRef.current) return;
        setState({
          history: null,
          state: "error",
          error: errorMessage(err),
        });
      });
    return () => {
      cancelled = true;
    };
  }, [projectId, taskId, version]);

  return state;
}

/**
 * 结果摘要：Markdown 排版、来源说明与空状态。
 * 来源取最近一次成功产出结果的执行作业。
 */
export function ResultSummary({
  taskId,
  summary,
  jobs,
}: {
  taskId: string;
  summary: string | null;
  jobs: JobSummary[];
}) {
  const source: ResultSource | null = resultSource(taskId, jobs);
  const filled = hasSummaryText(summary);
  return (
    <section className="task-result" data-drawer-block="result">
      <div className="task-result-head">
        <h3>结果摘要</h3>
        {source ? (
          <span className="task-result-source muted" title={source.jobId}>
            {source.label}
            {source.finishedAt ? ` · ${formatTime(source.finishedAt)}` : ""}
            {source.agent ? ` · ${source.agent}` : ""}
          </span>
        ) : null}
      </div>
      <RichContent
        content={summary}
        format="markdown"
        emptyText={RESULT_SUMMARY_EMPTY}
      />
      {filled ? null : (
        <p className="muted task-result-hint">{RESULT_SUMMARY_HINT}</p>
      )}
    </section>
  );
}

/**
 * 验收摘要：结论（通过/拒绝/待验收/未验收）、最近一轮原因与问题、
 * 验收次数与拒绝次数，并标明数据来源。
 */
export function VerificationRecapBlock({ recap }: { recap: VerificationRecap }) {
  const conclusion = VERIFICATION_CONCLUSIONS[recap.conclusion];
  return (
    <section className="task-verification" data-drawer-block="verification">
      <div className="task-result-head">
        <h3>{VERIFICATION_RECAP_TITLE}</h3>
        <span className="muted">{recap.sourceLabel}</span>
      </div>
      <div className="verification-verdict" data-source={recap.source}>
        <Badge tone={conclusion.tone}>{conclusion.label}</Badge>
        <span className="muted">
          验收 {recap.total} 次 · 拒绝 {recap.rejected} 次
        </span>
        {recap.round !== null ? (
          <span className="muted">最近一轮：第 {recap.round} 轮</span>
        ) : null}
      </div>
      {recap.reason ? (
        <div className="verification-reason">
          <span className="verification-label">最近一轮原因</span>
          <RichContent content={recap.reason} format="markdown" />
        </div>
      ) : null}
      {recap.issues.length > 0 ? (
        <div className="verification-issues">
          <span className="verification-label">问题</span>
          <ol>
            {recap.issues.map((issue, index) => (
              <li key={`${index}-${issue}`}>
                <RichContent content={issue} format="markdown" />
              </li>
            ))}
          </ol>
        </div>
      ) : null}
      {recap.unparsedExcerpt ? (
        <div className="verification-unparsed">
          <span className="verification-label">无法解析的验收结果</span>
          <pre className="rich-content-raw">{recap.unparsedExcerpt}</pre>
        </div>
      ) : null}
      {recap.conclusion === "none" ? (
        <p className="muted verification-note">{VERIFICATION_RECAP_EMPTY_HINT}</p>
      ) : null}
      {recap.detailNote ? (
        <p className="muted verification-note">{recap.detailNote}</p>
      ) : null}
    </section>
  );
}
