import { useEffect, useState } from "react";
import type { SyntheticEvent } from "react";
import type { JobDetail, JobSummary } from "../types";
import { api, errorMessage } from "../api";
import { JobActivity } from "./JobActivity";
import { RichContent } from "./RichContent";
import {
  Badge,
  FAILURE_KIND_LABELS,
  ROLE_LABELS,
  formatCost,
  formatDuration,
  formatMillis,
  formatTime,
  formatTokens,
  failureSummaryLabel,
  jobStatusLabel,
  shortText,
  toneForStatus,
} from "../ui";

export function JobDetailView({
  job,
  retryable = false,
  onRetry,
}: {
  job: JobSummary;
  retryable?: boolean;
  onRetry?: () => void;
}) {
  const [detail, setDetail] = useState<JobDetail | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [open, setOpen] = useState(job.status === "running");

  async function loadDetail() {
    setLoading(true);
    setError("");
    try {
      setDetail(await api.getJob(job.id));
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (open && !detail && !loading) void loadDetail();
    // Detail loading is keyed by the open state; job details never change for
    // a finished job.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  function handleToggle(event: SyntheticEvent<HTMLDetailsElement>) {
    setOpen(event.currentTarget.open);
  }

  const duration = job.duration_ms
    ? formatMillis(job.duration_ms)
    : formatDuration(job.started_at, job.finished_at);
  const pid = detail?.pid ?? null;
  const tokenDetails = formatTokenDetails(job);
  const failure = job.failure_summary;

  return (
    <div className="job">
      <details open={open} onToggle={handleToggle}>
        <summary className="job-summary">
          <span className="job-role">{ROLE_LABELS[job.role] ?? job.role}</span>
          <Badge tone={toneForStatus(job.status)}>
            {jobStatusLabel(job.status)}
          </Badge>
          <span className="muted">{job.agent_name ?? "无 Agent"}</span>
          <span className="muted">第 {job.attempt} 次</span>
          <span className="muted job-duration">{duration}</span>
          {job.usage_known ? (
            <span className="muted" title={tokenDetails || undefined}>
              {formatTokens(job.input_tokens)} in · {formatTokens(job.output_tokens)} out ·{" "}
              {formatCost(job.cost)}
            </span>
          ) : null}
          {failure ? (
            <Badge tone="danger">
              {failureSummaryLabel(failure.kind)} · {shortText(failure.message, 60)}
            </Badge>
          ) : job.failure_kind ? (
            <Badge tone="danger">
              {FAILURE_KIND_LABELS[job.failure_kind] ?? job.failure_kind}
            </Badge>
          ) : null}
          {job.exit_code !== null ? (
            <span className="muted">exit {job.exit_code}</span>
          ) : null}
          {retryable && onRetry ? (
            <button
              className="secondary"
              title="让这个失败作业重新排队"
              onClick={(event) => {
                event.preventDefault();
                event.stopPropagation();
                onRetry();
              }}
            >
              重试
            </button>
          ) : null}
        </summary>
        {open ? (
          <div className="job-body">
            <div className="job-meta">
              创建 {formatTime(job.created_at)} · 开始 {formatTime(job.started_at)} · 结束{" "}
              {formatTime(job.finished_at)}
              {pid ? ` · PID ${pid}` : ""}
            </div>
            <div className="job-meta">
              用量：
              {job.usage_known
                ? `输入 ${formatTokens(job.input_tokens)} · 输出 ${formatTokens(
                    job.output_tokens,
                  )} · 费用 ${formatCost(job.cost)}${
                    tokenDetails ? ` · ${tokenDetails}` : ""
                  }`
                : "未记录（Agent 未提供 token/费用）"}
              {" · "}时长 {formatMillis(job.duration_ms)}
            </div>
            {failure ? (
              <div className="failure-summary">
                <div className="failure-summary-head">
                  <Badge tone="danger">{failureSummaryLabel(failure.kind)}</Badge>
                  <span>{failure.message}</span>
                </div>
                {failure.status !== null || failure.retry_after_ms !== null ? (
                  <div className="job-meta">
                    {failure.status !== null ? `HTTP ${failure.status}` : ""}
                    {failure.status !== null && failure.retry_after_ms !== null ? " · " : ""}
                    {failure.retry_after_ms !== null
                      ? `${formatMillis(failure.retry_after_ms)} 后重试`
                      : ""}
                  </div>
                ) : null}
              </div>
            ) : null}
            {job.error ? (
              <details className="raw-error">
                <summary>查看原始错误</summary>
                <div className="error-box">
                  <RichContent content={job.error} />
                </div>
              </details>
            ) : null}
            <JobActivity jobId={job.id} running={job.status === "running"} />
            {loading ? <div className="muted">加载作业详情…</div> : null}
            {error ? <div className="error-box">{error}</div> : null}
            {detail ? <JobBlock title="结果" value={detail.result} /> : null}
            {detail ? <JobBlock title="提示词" value={detail.prompt} /> : null}
          </div>
        ) : null}
      </details>
    </div>
  );
}

function JobBlock({ title, value }: { title: string; value: string | null }) {
  if (!value) return null;
  return (
    <div className="job-block">
      <div className="job-block-title">{title}</div>
      <RichContent content={value} />
    </div>
  );
}

function formatTokenDetails(job: JobSummary): string {
  const parts: string[] = [];
  if (job.cache_read_tokens != null) {
    parts.push(`缓存读 ${formatTokens(job.cache_read_tokens)}`);
  }
  if (job.cache_write_tokens != null) {
    parts.push(`缓存写 ${formatTokens(job.cache_write_tokens)}`);
  }
  if (job.reasoning_tokens != null) {
    parts.push(`推理 ${formatTokens(job.reasoning_tokens)}`);
  }
  return parts.join(" · ");
}
