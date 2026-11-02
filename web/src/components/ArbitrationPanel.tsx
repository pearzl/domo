import { useEffect, useState } from "react";
import type { ArbitrationContext, Task } from "../types";
import { api, errorMessage } from "../api";
import { InputBox } from "./InputBox";
import { useDraftValue } from "./useDraft";
import { RichContent } from "./RichContent";
import {
  Badge,
  ROLE_LABELS,
  failureSummaryLabel,
  jobStatusLabel,
  shortText,
  taskStateLabel,
} from "../ui";

/// 阻塞任务的人工裁决：查看失败上下文，重试、换 Agent 或记录指导后重新排队。
export function ArbitrationPanel({
  projectId,
  task,
  onRefresh,
  disabled,
  onLegacyRetry,
}: {
  projectId: string;
  task: Task;
  onRefresh: () => Promise<void>;
  disabled: boolean;
  onLegacyRetry: () => void;
}) {
  const [context, setContext] = useState<ArbitrationContext | null>(null);
  const [contextError, setContextError] = useState("");
  const [agentId, setAgentId] = useState("");
  const guidanceDraft = useDraftValue(
    { projectId, taskId: task.id, field: "arbitration-guidance" },
    "",
  );
  const guidance = guidanceDraft.value;
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let active = true;
    setContextError("");
    api
      .getTaskArbitration(projectId, task.id)
      .then((next) => {
        if (!active) return;
        setContext(next);
        setAgentId((current) => current || next.agents[0]?.id || "");
      })
      .catch((err) => {
        if (active) setContextError(errorMessage(err));
      });
    return () => {
      active = false;
    };
  }, [projectId, task.id, task.status, task.stage, task.human_gate]);

  async function submit(input: {
    action: "retry" | "switch_agent" | "guide";
    agent_id?: string;
    guidance?: string;
  }) {
    setBusy(true);
    setError("");
    try {
      await api.submitTaskArbitration(projectId, task.id, input);
      guidanceDraft.clear();
      await onRefresh();
      const next = await api.getTaskArbitration(projectId, task.id);
      setContext(next);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  const roleLabel = context?.role === "verifier" ? "验收" : "执行";

  return (
    <section className="arbitration-panel">
      <h3>人工裁决</h3>
      {contextError ? (
        <>
          <div className="error-box">{contextError}</div>
          <button className="primary" disabled={disabled || busy} onClick={onLegacyRetry}>
            重试此任务
          </button>
        </>
      ) : null}
      {context ? (
        <>
          <div className="kv">
            <span>阶段</span>
            <span>
              {taskStateLabel(context.task.stage, context.task.status)}
            </span>
          </div>
          <div className="kv">
            <span>失败原因</span>
            <div>
              {context.task.failure_summary ? (
                <div className="failure-summary">
                  <div className="failure-summary-head">
                    <Badge tone="danger">
                      {failureSummaryLabel(context.task.failure_summary.kind)}
                    </Badge>
                    <span>{shortText(context.task.failure_summary.message, 160)}</span>
                  </div>
                </div>
              ) : null}
              {context.task.last_error ? (
                <details className="raw-error">
                  <summary>查看原始错误</summary>
                  <div className="error-box">
                    <RichContent content={context.task.last_error} />
                  </div>
                </details>
              ) : (
                <span className="muted">—</span>
              )}
            </div>
          </div>
          <div className="kv">
            <span>失败统计</span>
            <span>
              执行 {context.task.attempts} 次 · 验收拒绝 {context.task.rejections} 次
            </span>
          </div>
          <div className="kv">
            <span>对应角色</span>
            <span>{roleLabel}</span>
          </div>
          {context.jobs.length > 0 ? (
            <div className="arbitration-jobs">
              {context.jobs.slice(-4).map((job) => (
                <div className="arbitration-job" key={job.id}>
                  <div className="muted">
                    {ROLE_LABELS[job.role] ?? job.role} · {job.agent_name ?? "—"} ·{" "}
                    {jobStatusLabel(job.status)}
                    {job.failure_summary
                      ? ` · ${failureSummaryLabel(job.failure_summary.kind)} · ${shortText(
                          job.failure_summary.message,
                          100,
                        )}`
                      : job.error
                        ? " · 失败 · 摘要不可用"
                        : ""}
                  </div>
                  {job.error ? (
                    <details className="raw-error">
                      <summary>查看原始错误</summary>
                      <div className="error-box">
                        <RichContent content={job.error} />
                      </div>
                    </details>
                  ) : null}
                </div>
              ))}
            </div>
          ) : null}
          <div className="button-row">
            <button
              className="primary"
              disabled={disabled || busy}
              onClick={() => void submit({ action: "retry" })}
            >
              重试
            </button>
          </div>
          <div className="arbitration-switch">
            <select
              value={agentId}
              onChange={(event) => setAgentId(event.target.value)}
              disabled={disabled || busy || context.agents.length === 0}
            >
              {context.agents.map((agent) => (
                <option key={agent.id} value={agent.id}>
                  {agent.name}
                  {agent.enabled ? "" : "（已停用）"}
                  {agent.cooldown_until ? "（冷却中）" : ""}
                </option>
              ))}
            </select>
            <button
              className="secondary"
              disabled={disabled || busy || !agentId}
              onClick={() =>
                void submit({ action: "switch_agent", agent_id: agentId })
              }
            >
              换 Agent 执行
            </button>
          </div>
          <InputBox
            value={guidance}
            onChange={guidanceDraft.setValue}
            draftKey={guidanceDraft.key}
            rows={3}
            disabled={disabled || busy}
            placeholder="补充指导：说明下一步应如何处理；提交后任务重新排队，等待派发新作业。"
            actions={
              <button
                className="secondary"
                disabled={disabled || busy || !guidance.trim()}
                onClick={() =>
                  void submit({ action: "guide", guidance: guidance.trim() })
                }
              >
                记录指导并重新排队
              </button>
            }
          />
        </>
      ) : null}
      {error ? <div className="error-box">{error}</div> : null}
    </section>
  );
}
