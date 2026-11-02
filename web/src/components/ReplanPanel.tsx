import { InputBox } from "./InputBox";
import type { ReplanSummary } from "../presentation/taskList";

/// 重新规划：预览将取消与保留的任务，提交重规划原因。
export function ReplanPanel({
  preview,
  reason,
  reasonDraftKey,
  busy,
  onReasonChange,
  onSubmit,
  onCancel,
}: {
  preview: ReplanSummary;
  reason: string;
  reasonDraftKey: string;
  busy: boolean;
  onReasonChange: (value: string) => void;
  onSubmit: () => void;
  onCancel: () => void;
}) {
  return (
    <section className="panel">
      <h2>重新规划</h2>
      <p className="subtitle">
        已完成任务会保留；未完成任务会退出当前 DAG 并停止派发。提交后规划者重新给出方案，确认方案后才继续执行。请说明需要重新规划的原因。
      </p>
      <div className="replan-preview">
        <div className="replan-preview-group">
          <strong>将取消 {preview.cancelCount} 个未完成任务</strong>
          {preview.cancelKeys.length ? (
            <span className="mono replan-keys">
              {preview.cancelKeys.join("、")}
            </span>
          ) : (
            <span className="muted">没有未完成任务</span>
          )}
        </div>
        <div className="replan-preview-group">
          <strong>保留 {preview.keepCount} 个已完成任务</strong>
          {preview.keepKeys.length ? (
            <span className="mono replan-keys">
              {preview.keepKeys.join("、")}
            </span>
          ) : (
            <span className="muted">没有已完成任务</span>
          )}
        </div>
      </div>
      <div className="confirm-row">
        <InputBox
          value={reason}
          onChange={onReasonChange}
          draftKey={reasonDraftKey}
          rows={3}
          maxAutoGrowChars={520}
          placeholder="例如：原计划遗漏了必要的数据迁移步骤，需要调整为……"
          actions={
            <div className="button-row">
              <button
                className="primary"
                disabled={busy || !reason.trim()}
                onClick={onSubmit}
              >
                提交重规划
              </button>
              <button
                className="secondary"
                disabled={busy}
                onClick={onCancel}
              >
                取消
              </button>
            </div>
          }
        />
      </div>
    </section>
  );
}
