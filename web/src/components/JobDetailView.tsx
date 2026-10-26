import { useState } from "react";
import type { SyntheticEvent } from "react";
import type { JobDetail, JobSummary } from "../types";
import { api, errorMessage } from "../api";
import { Badge, JOB_STATUS_LABELS, ROLE_LABELS, formatDuration, formatTime, toneForStatus } from "../ui";

export function JobDetailView({ job }: { job: JobSummary }) {
  const [detail, setDetail] = useState<JobDetail | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  async function handleToggle(event: SyntheticEvent<HTMLDetailsElement>) {
    if (!event.currentTarget.open || detail || loading) return;
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

  return (
    <details className="job" onToggle={handleToggle}>
      <summary className="job-summary">
        <span className="job-role">{ROLE_LABELS[job.role] ?? job.role}</span>
        <Badge tone={toneForStatus(job.status)}>
          {JOB_STATUS_LABELS[job.status] ?? job.status}
        </Badge>
        <span className="muted">{job.agent_name ?? "无 Agent"}</span>
        <span className="muted">第 {job.attempt} 次</span>
        <span className="muted job-duration">
          {formatDuration(job.started_at, job.finished_at)}
        </span>
        {job.exit_code !== null ? (
          <span className="muted">exit {job.exit_code}</span>
        ) : null}
      </summary>
      <div className="job-body">
        <div className="job-meta">
          创建 {formatTime(job.created_at)} · 开始 {formatTime(job.started_at)} · 结束{" "}
          {formatTime(job.finished_at)}
        </div>
        {job.error ? <div className="error-box pre-wrap">{job.error}</div> : null}
        {loading ? <div className="muted">加载作业详情…</div> : null}
        {error ? <div className="error-box">{error}</div> : null}
        {detail ? (
          <>
            <JobBlock title="结果" value={detail.result} />
            <JobBlock title="提示词" value={detail.prompt} />
            <JobBlock title="标准输出" value={detail.stdout} />
            <JobBlock title="标准错误" value={detail.stderr} />
          </>
        ) : null}
      </div>
    </details>
  );
}

function JobBlock({ title, value }: { title: string; value: string | null }) {
  if (!value) return null;
  return (
    <div className="job-block">
      <div className="job-block-title">{title}</div>
      <pre>{value}</pre>
    </div>
  );
}
