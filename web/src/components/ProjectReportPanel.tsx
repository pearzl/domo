import type { Message, Project } from "../types";
import { InputBox } from "./InputBox";
import { RichContent } from "./RichContent";
import { Empty } from "../ui";


function acceptanceIssueCount(project: Project) {
  try {
    const snapshot = JSON.parse(project.acceptance ?? "null");
    return Array.isArray(snapshot?.issues) ? snapshot.issues.length : 0;
  } catch {
    return 0;
  }
}
/// 项目报告与评议：报告内容、意见记录与三类提交动作。
export function ProjectReportPanel({
  project,
  reviewMessages,
  feedback,
  feedbackDraftKey,
  onFeedbackChange,
  busy,
  archived,
  onSubmitReport,
  onOpenTasks,
  onRetryAcceptance,
}: {
  project: Project;
  reviewMessages: Message[];
  feedback: string;
  feedbackDraftKey: string;
  onFeedbackChange: (value: string) => void;
  busy: boolean;
  archived: boolean;
  onSubmitReport: (action: "accept" | "comment" | "reject") => void;
  onOpenTasks?: () => void;
  onRetryAcceptance?: () => void;
}) {
  const unclosedCount = acceptanceIssueCount(project);
  if (project.stage !== "summarizing" && !project.report) return (
    <section className="panel" aria-label="项目报告">
      <Empty title="项目报告尚未生成" hint={archived ? "项目已归档，未保存项目报告。任务与作业记录仍可查阅。" : project.stage === "analysis" ? "确认项目方案并完成交付后，规划者将生成报告。" : "任务仍在推进，任务完成后规划者将进行项目验收并生成报告。"} />
      {onOpenTasks ? <div className="empty-actions"><button className="secondary" onClick={onOpenTasks}>查看任务进展</button></div> : null}
    </section>
  );
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
          <div className="blocked-banner">
            <p><strong>等待处理</strong></p>
            <RichContent content={project.blocked_reason} />
            {project.acceptance && !project.acceptance_passed && project.status === "blocked" && onRetryAcceptance ? <button className="primary" disabled={busy} onClick={onRetryAcceptance}>条件已补齐，重新验收</button> : null}
          </div>
        ) : (
          <p className="muted">规划者正在依据评议意见判断后续处理…</p>
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
            placeholder="对报告的意见（提交意见可多轮讨论；驳回时必须填写）。"
            actions={
              <div className="button-row">
                <button
                  className="primary"
                  disabled={busy || !project.report || (unclosedCount > 0 && !feedback.trim())}
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
                  title="只表达不接受；规划者判断补工作、调整契约还是重写报告"
                >
                  驳回
                </button>
              </div>
            }
          />
          <p className="muted">
            {unclosedCount > 0
              ? "报告有 " + unclosedCount + " 项未闭合项，接受时必须填写理由。"
              : "驳回只表达不接受；是否补工作、调整契约或重写报告由规划者判断。"}
          </p>
        </div>
      ) : null}
    </section>

  );
}
