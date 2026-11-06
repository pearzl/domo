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

  it("验证条件缺项时展示解除条件和重新验收入口，不声称报告生成失败", () => {
    const markup = renderToStaticMarkup(<ProjectReportPanel {...props} onRetryAcceptance={() => {}}
      project={{ stage: "summarizing", status: "blocked", report: null, acceptance: '{"next_action":"wait"}', acceptance_passed: 0,
        blocked_reason: "等待用户连接目标设备" } as Project} />);
    expect(markup).toContain("等待用户连接目标设备");
    expect(markup).toContain("条件已补齐，重新验收");
    expect(markup).not.toContain("报告生成失败");
  });

  it("直接完成项目重写报告不表示重新实施", () => {
    const markup = renderToStaticMarkup(<ProjectReportPanel {...props}
      project={{ stage: "summarizing", status: "blocked", report: "研究报告", completion_mode: "direct" } as Project} />);
    expect(markup).toContain("重新撰写只修改报告");
    expect(markup).not.toContain("重做并重新总结");
  });

  it("项目验收用用户说明展示操作，执行指导与验收快照分别折叠", () => {
    const user_action = { summary: "请确认采用哪个统计口径", why: "两种口径会得到不同的结果。", owner: "报表负责人",
      steps: ["选择含税或不含税口径，并记录决定。"], after: "记录意见后重新验收，AI 将核对最终报表。" };
    const markup = renderToStaticMarkup(<ProjectReportPanel {...props} onRetryAcceptance={() => {}}
      project={{ stage: "summarizing", status: "blocked", report: null, acceptance_passed: 0,
        user_action, blocked_reason: user_action.summary,
        acceptance: JSON.stringify({ next_action: "arbitrate", user_action, guidance: "核对所选口径与报表公式", criteria: [] }) } as Project} />);
    expect(markup).toContain("选择含税或不含税口径");
    expect(markup).toContain("Agent 执行指导");
    expect(markup.indexOf("选择含税或不含税口径")).toBeLessThan(markup.indexOf("核对所选口径与报表公式"));
    expect(markup).not.toMatch(/<details[^>]*\bopen/);
    expect(markup).toContain("条件已补齐，重新验收");
  });
});
