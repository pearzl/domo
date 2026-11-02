import { useState } from "react";
import type { Task } from "../types";
import { api, errorMessage } from "../api";
import { InputBox } from "./InputBox";
import { useDraftValue } from "./useDraft";
import { RichContent } from "./RichContent";

/// 里程碑人工验收：通过即合并；拒绝可退回执行或要求重新规划。
export function HumanAcceptancePanel({
  task,
  projectId,
  onRefresh,
  disabled,
}: {
  task: Task;
  projectId: string;
  onRefresh: () => Promise<void>;
  disabled: boolean;
}) {
  const reasonDraft = useDraftValue(
    { projectId: task.project_id, taskId: task.id, field: "acceptance-reason" },
    "",
  );
  const guidanceDraft = useDraftValue(
    { projectId: task.project_id, taskId: task.id, field: "acceptance-guidance" },
    "",
  );
  const reason = reasonDraft.value;
  const guidance = guidanceDraft.value;
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  async function submit(action: "accept" | "retry" | "replan") {
    if (action !== "accept" && !reason.trim()) {
      setError("拒绝时必须填写理由。");
      return;
    }
    setBusy(true);
    setError("");
    try {
      if (action === "accept") {
        await api.acceptMilestone(projectId, task.id, reason);
      } else {
        await api.rejectMilestone(projectId, task.id, reason, guidance, action);
      }
      reasonDraft.clear();
      guidanceDraft.clear();
      await onRefresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="arbitration-panel">
      <h3>人工验收</h3>
      <div className="blocked-banner">
        <strong>验收者已通过，等待人工确认</strong>
        <RichContent
          content={task.last_error}
          emptyText="里程碑任务必须由人工确认。"
        />
      </div>
      <InputBox
        value={reason}
        onChange={reasonDraft.setValue}
        draftKey={reasonDraft.key}
        rows={3}
        disabled={disabled || busy}
        placeholder="通过意见（可选）；拒绝时必须填写理由。"
      />
      <InputBox
        value={guidance}
        onChange={guidanceDraft.setValue}
        draftKey={guidanceDraft.key}
        rows={3}
        disabled={disabled || busy}
        placeholder="拒绝时可选：给执行者或规划者的补充指导。"
        actions={
          <div className="button-row">
            <button
              className="primary"
              disabled={disabled || busy}
              onClick={() => void submit("accept")}
            >
              人工通过并合并
            </button>
            <button
              className="secondary"
              disabled={disabled || busy}
              onClick={() => void submit("retry")}
            >
              拒绝并退回执行
            </button>
            <button
              className="secondary"
              disabled={disabled || busy}
              onClick={() => void submit("replan")}
            >
              拒绝并要求重新规划
            </button>
          </div>
        }
      />
      <small>
        退回执行会把验收意见和指导交给执行者；要求重新规划会作废未完成任务，并把意见交给规划者。
      </small>
      {error ? <div className="error-box">{error}</div> : null}
    </section>
  );
}
