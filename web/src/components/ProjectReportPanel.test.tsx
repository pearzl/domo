import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Project } from "../types";
import { ProjectReportPanel } from "./ProjectReportPanel";

describe("报告未生成", () => {
  const props = { reviewMessages: [], feedback: "", feedbackDraftKey: "test-report-feedback", onFeedbackChange: () => {}, busy: false, archived: false, onSubmitReport: () => {}, onRequestReplan: () => {}, onAddTask: () => {}, onOpenTasks: () => {} };
  it("执行阶段给出明确说明和任务入口，不返回空白面板", () => {
    const markup = renderToStaticMarkup(<ProjectReportPanel {...props} project={{ stage: "executing", report: null } as Project} />);
    expect(markup).toContain("项目报告尚未生成");
    expect(markup).toContain("任务仍在推进");
    expect(markup).toContain("查看任务进展");
    expect(markup).not.toContain("通过并完成");
  });

  it("归档项目不暗示后续会继续生成报告", () => {
    const markup = renderToStaticMarkup(<ProjectReportPanel {...props} archived project={{ stage: "analysis", report: null } as Project} />);
    expect(markup).toContain("项目已归档");
    expect(markup).not.toContain("规划者将生成报告");
  });
});
