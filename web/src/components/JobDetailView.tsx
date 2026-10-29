import { useState } from "react";
import type { SyntheticEvent } from "react";
import type { JobDetail, JobSummary } from "../types";
import { api, errorMessage } from "../api";
import { LiveOutput } from "./LiveOutput";
import { RichContent } from "./RichContent";
import {
  Badge,
  FAILURE_KIND_LABELS,
  JOB_STATUS_LABELS,
  ROLE_LABELS,
  formatCost,
  formatDuration,
  formatMillis,
  formatTime,
  formatTokens,
  failureSummaryLabel,
  shortText,
  toneForStatus,
} from "../ui";

export function JobDetailView({ job }: { job: JobSummary }) {
  const [detail, setDetail] = useState<JobDetail | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

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

  function handleToggle(event: SyntheticEvent<HTMLDetailsElement>) {
    if (event.currentTarget.open && !detail && !loading) {
      void loadDetail();
    }
  }

  const duration = job.duration_ms
    ? formatMillis(job.duration_ms)
    : formatDuration(job.started_at, job.finished_at);
  const pid = detail?.pid ?? null;
  const tokenDetails = formatTokenDetails(job);
  const failure = job.failure_summary;

  return (
    <div className="job">
      <details onToggle={handleToggle}>
        <summary className="job-summary">
          <span className="job-role">{ROLE_LABELS[job.role] ?? job.role}</span>
          <Badge tone={toneForStatus(job.status)}>
            {JOB_STATUS_LABELS[job.status] ?? job.status}
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
        </summary>
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
          {loading ? <div className="muted">加载作业详情…</div> : null}
          {error ? <div className="error-box">{error}</div> : null}
          {detail ? <JobBlock title="结果" value={detail.result} /> : null}
          {detail ? <JobBlock title="提示词" value={detail.prompt} /> : null}
        </div>
      </details>
      <div className="job-live">
        <LiveOutput jobId={job.id} running={job.status === "running"} />
      </div>
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
