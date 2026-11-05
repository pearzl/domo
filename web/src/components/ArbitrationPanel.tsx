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

/// 人工裁决以新会话执行；原会话继续由作业记录中的资格控制。
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
  }, [projectId, task.id, task.status, task.stage, task.human_gate, task.waiting_reason]);

  async function submit(input: {
    action: "retry" | "switch_agent" | "guide";
    agent_id?: string;
    guidance?: string;
    risk_confirmed?: boolean;
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
      <h3>{context?.replan_request ? "重规划决策" : "人工裁决"}</h3>
      {context?.replan_request ? <p className="muted">
        {context.replan_request.state === "waiting"
          ? "等待输入或处理条件：补齐后可重新决策，或记录指导让原任务继续。"
          : "上级正在处理请求或等待替代方案应用，无需人工逐项审批；其他独立任务继续。"}
      </p> : null}
      {contextError ? (
        <>
          <div className="error-box">{contextError}</div>
          <button className="primary" disabled={disabled || busy} onClick={onLegacyRetry}>
            新会话执行此任务
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
              disabled={disabled || busy || !context.actions.includes("retry")}
              onClick={() => void submit({ action: "retry" })}
            >
              {context.replan_request ? "重新决策" : "新会话执行"}
            </button>
          </div>
          {context.actions.includes("switch_agent") ? <div className="arbitration-switch">
            <p className="muted">人工指定会绕过角色能力、启停、冷却、可用时段和用量限额，可能产生额外费用；并发满额时仍等待该 Agent。</p>
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
              onClick={() => {
                if (window.confirm("确认本次人工指定？可能产生额外费用、使用不具备目标角色的 Agent，并绕过正常可用性限制。")) {
                  void submit({ action: "switch_agent", agent_id: agentId, risk_confirmed: true });
                }
              }}
            >
              换 Agent 执行
            </button>
          </div> : null}
          <InputBox
            value={guidance}
            onChange={guidanceDraft.setValue}
            draftKey={guidanceDraft.key}
            rows={3}
            disabled={disabled || busy || !context.actions.includes("guide")}
            placeholder="补充指导：说明下一步应如何处理；提交后任务重新排队，等待派发新作业。"
            actions={
              <button
                className="secondary"
                disabled={disabled || busy || !guidance.trim() || !context.actions.includes("guide")}
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
