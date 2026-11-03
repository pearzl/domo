import { InputBox } from "./InputBox";

/// 用户主动修改已确认目标；提交只启动重规划，方案影响仍需确认。
export function GoalEditPanel({
  goal,
  goalDraftKey,
  busy,
  onGoalChange,
  onSubmit,
  onCancel,
}: {
  goal: string;
  goalDraftKey: string;
  busy: boolean;
  onGoalChange: (value: string) => void;
  onSubmit: () => void;
  onCancel: () => void;
}) {
  return (
    <section className="panel">
      <h2>修改项目目标</h2>
      <p className="subtitle">
        新目标会记录到审计并启动一个更高代际的重规划。规划草稿只能围绕新目标生成；旧任务和报告在方案确认前保持可恢复。
      </p>
      <InputBox
        value={goal}
        onChange={onGoalChange}
        draftKey={goalDraftKey}
        rows={3}
        maxAutoGrowChars={520}
        placeholder="明确新的交付目标、范围边界和验收预期。"
        actions={
          <div className="button-row">
            <button className="primary" disabled={busy || !goal.trim()} onClick={onSubmit}>
              提交新目标并重规划
            </button>
            <button className="secondary" disabled={busy} onClick={onCancel}>
              取消
            </button>
          </div>
        }
      />
    </section>
  );
}
