import { useEffect, useState } from "react";
import type { SyntheticEvent } from "react";
import type { JobDetail, JobSummary } from "../types";
import { api, errorMessage } from "../api";
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
  toneForStatus,
} from "../ui";

export function JobDetailView({ job }: { job: JobSummary }) {
  const [detail, setDetail] = useState<JobDetail | null>(null);
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [live, setLive] = useState<{
    stdout: string;
    stderr: string;
    pid: number | null;
    running: boolean;
  } | null>(null);

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

  async function loadOutput() {
    try {
      setLive(await api.getJobOutput(job.id));
    } catch {
      // The job may have just been removed or the output file may not exist yet.
    }
  }

  function handleToggle(event: SyntheticEvent<HTMLDetailsElement>) {
    const next = event.currentTarget.open;
    setOpen(next);
    if (next && !detail && !loading) {
      void loadDetail();
    }
    if (next) void loadOutput();
  }

  useEffect(() => {
    if (!open) return;
    void loadOutput();
    if (job.status !== "running") return;
    const timer = window.setInterval(() => void loadOutput(), 1500);
    return () => window.clearInterval(timer);
  }, [open, job.id, job.status]);

  const duration = job.duration_ms
    ? formatMillis(job.duration_ms)
    : formatDuration(job.started_at, job.finished_at);
  const stdout = live?.stdout || detail?.stdout || "";
  const stderr = live?.stderr || detail?.stderr || "";
  const pid = live?.pid ?? detail?.pid ?? null;

  return (
    <details className="job" onToggle={handleToggle}>
      <summary className="job-summary">
        <span className="job-role">{ROLE_LABELS[job.role] ?? job.role}</span>
        <Badge tone={toneForStatus(job.status)}>
          {JOB_STATUS_LABELS[job.status] ?? job.status}
        </Badge>
        <span className="muted">{job.agent_name ?? "无 Agent"}</span>
        <span className="muted">第 {job.attempt} 次</span>
        <span className="muted job-duration">{duration}</span>
        {job.usage_known ? (
          <span className="muted">
            {formatTokens(job.input_tokens)} in · {formatTokens(job.output_tokens)} out ·{" "}
            {formatCost(job.cost)}
          </span>
        ) : (
          <Badge tone="warn" title="Agent 未在结果中提供 token 或费用">
            用量未知
          </Badge>
        )}
        {job.failure_kind ? (
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
              )} · 费用 ${formatCost(job.cost)}`
            : "未知（Agent 未提供 token 或费用）"}
          {" · "}时长 {formatMillis(job.duration_ms)}
        </div>
        {job.error ? <div className="error-box pre-wrap">{job.error}</div> : null}
        {loading ? <div className="muted">加载作业详情…</div> : null}
        {error ? <div className="error-box">{error}</div> : null}
        {detail ? <JobBlock title="结果" value={detail.result} /> : null}
        <JobBlock
          title={live?.running ? "运行输出（实时）" : "标准输出"}
          value={stdout}
        />
        <JobBlock
          title={live?.running ? "运行错误（实时）" : "标准错误"}
          value={stderr}
        />
        {detail ? <JobBlock title="提示词" value={detail.prompt} /> : null}
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
