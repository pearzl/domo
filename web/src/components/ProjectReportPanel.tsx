import type { Message, Project } from "../types";
import { InputBox } from "./InputBox";
import { RichContent } from "./RichContent";

/// 项目报告与评议：报告内容、意见记录与四类提交动作。
export function ProjectReportPanel({
  project,
  reviewMessages,
  feedback,
  feedbackDraftKey,
  onFeedbackChange,
  busy,
  archived,
  onSubmitReport,
  onRequestReplan,
  onAddTask,
}: {
  project: Project;
  reviewMessages: Message[];
  feedback: string;
  feedbackDraftKey: string;
  onFeedbackChange: (value: string) => void;
  busy: boolean;
  archived: boolean;
  onSubmitReport: (action: "accept" | "comment" | "reject") => void;
  onRequestReplan: () => void;
  onAddTask: () => void;
}) {
  if (project.stage !== "summarizing" && !project.report) return null;
  return (
    <section className="panel">
      <h2>项目报告{reviewMessages.length > 0 ? `（${reviewMessages.length} 条评议）` : ""}</h2>
      {reviewMessages.length > 0 ? (
        <details className="report-review-log" open={!!project.report}>
          <summary>评议记录（{reviewMessages.length}）</summary>
          <div className="messages compact">
            {reviewMessages.map((item) => (
              <div key={item.id} className="message message-user">
                <div className="message-role">你的意见</div>
                <RichContent content={item.content} className="message-content" />
              </div>
            ))}
          </div>
        </details>
      ) : null}
      {project.report ? (
        <RichContent content={project.report} className="report" />
      ) : project.stage === "summarizing" ? (
        project.blocked_reason ? (
          <p className="muted">报告生成失败，可在上方失败作业上重试。</p>
        ) : (
          <p className="muted">规划者正在依据评议意见重写报告…</p>
        )
      ) : null}
      {project.stage === "summarizing" && !archived ? (
        <div className="report-actions">
          <InputBox
            value={feedback}
            onChange={onFeedbackChange}
            draftKey={feedbackDraftKey}
            rows={3}
            maxAutoGrowChars={520}
            placeholder="对报告的意见（提交意见可供多轮讨论；要求重写或重新规划时必须填写）。"
            actions={
              <div className="button-row">
                <button
                  className="primary"
                  disabled={busy || !project.report}
                  onClick={() => onSubmitReport("accept")}
                >
                  通过并完成
                </button>
                <button
                  className="secondary"
                  disabled={busy || !feedback.trim()}
                  onClick={() => onSubmitReport("comment")}
                >
                  提交意见
                </button>
                <button
                  className="secondary"
                  disabled={busy || !project.report || !feedback.trim()}
                  onClick={() => onSubmitReport("reject")}
                  title={
                    project.completion_mode === "direct"
                      ? "记录意见并重新执行直接完成"
                      : "记录意见并要求规划者重新撰写报告"
                  }
                >
                  {project.completion_mode === "direct" ? "重做并重新总结" : "重新撰写"}
                </button>
                <button
                  className="secondary"
                  disabled={busy || !project.report || !feedback.trim()}
                  onClick={onRequestReplan}
                  title="先预览影响；提交后生成方案草稿，确认草稿时才应用"
                >
                  要求重新规划
                </button>
                {project.completion_mode !== "direct" ? (
                  <button
                    className="secondary"
                    disabled={busy || archived}
                    onClick={onAddTask}
                    title="通过添加任务表单增加新任务；添加后项目回到执行阶段，原报告作废"
                  >
                    增加新任务
                  </button>
                ) : null}
              </div>
            }
          />
          {project.completion_mode !== "direct" ? (
            <p className="muted">
              “增加新任务”会打开任务面板的添加任务表单（复用人工加任务端点）；提交后项目回到执行阶段，原报告作废待重写。
            </p>
          ) : (
            <p className="muted">
              直接完成模式下，“重做并重新总结”会重新执行并生成新报告。
            </p>
          )}
        </div>
      ) : null}
    </section>
  );
}
