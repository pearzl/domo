import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Project } from "../types";
import { ProjectReportPanel } from "./ProjectReportPanel";

const props = {
  reviewMessages: [],
  feedback: "",
  feedbackDraftKey: "test-report-feedback",
  onFeedbackChange: () => {},
  busy: false,
  archived: false,
  onSubmitReport: () => {},
  onOpenTasks: () => {},
};

describe("报告未生成", () => {
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

  it("验证条件缺项时展示解除条件和重新验收入口，不声称报告生成失败", () => {
    const markup = renderToStaticMarkup(<ProjectReportPanel {...props} onRetryAcceptance={() => {}}
      project={{ stage: "summarizing", status: "blocked", report: null, acceptance: '{"issues":[]}', acceptance_passed: 0,
        blocked_reason: "等待用户连接目标设备" } as Project} />);
    expect(markup).toContain("等待用户连接目标设备");
    expect(markup).toContain("条件已补齐，重新验收");
    expect(markup).not.toContain("报告生成失败");
  });

  it("报告带未闭合项时提示接受必须填写理由", () => {
    const markup = renderToStaticMarkup(<ProjectReportPanel {...props} project={{ stage: "summarizing", status: "blocked",
      report: "研究报告", acceptance: '{"issues":["需要确认口径"]}', acceptance_passed: 1 } as Project} />);
    expect(markup).toContain("报告有 1 项未闭合项");
    expect(markup).toContain("接受时必须填写理由");
  });

  it("人工只表达接受或驳回，不直接选择重写或重规划", () => {
    const markup = renderToStaticMarkup(<ProjectReportPanel {...props} project={{ stage: "summarizing", status: "blocked",
      report: "研究报告", acceptance: '{"issues":[]}', acceptance_passed: 1 } as Project} />);
    expect(markup).toContain("通过并完成");
    expect(markup).toContain("驳回");
    expect(markup).not.toContain("重新撰写");
    expect(markup).not.toContain("要求重新规划");
  });
});
