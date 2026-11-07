import { renderToStaticMarkup } from "react-dom/server";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import type { Project } from "../types";
import { ProjectReportPanel } from "./ProjectReportPanel";
import { ReplanPanel } from "./ReplanPanel";
import { GoalEditPanel } from "./GoalEditPanel";

const project: Project = {
  id: "p1",
  name: "安全重规划",
  goal: "验证报告入口",
  repo_path: "/repo",
  base_branch: "main",
  branch: "domo/p1",
  workspace: "/workspace",
  stage: "summarizing",
  status: "blocked",
  blocked_reason: null,
  report: "# 当前报告",
  report_feedback: null,
  replan_reason: null,
  replan_source: null,
  replan_requested_at: null,
  confirm_mode: "auto",
  confirmed_at: "2026-01-01T00:00:00Z",
  merged_at: null,
  merged_commit: null,
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

describe("安全重规划入口", () => {
  it("报告动作进入影响预览而不是承诺立即回到分析", () => {
    const markup = renderToStaticMarkup(
      <ProjectReportPanel
        project={project}
        reviewMessages={[]}
        feedback="需要调整范围"
        feedbackDraftKey="report"
        onFeedbackChange={() => {}}
        busy={false}
        archived={false}
        onSubmitReport={() => {}}
      />,
    );
    expect(markup).toContain("驳回");
    expect(markup).toContain("由规划者判断");
  });

  it("主动修改目标与方案影响确认是两个独立步骤", () => {
    const markup = renderToStaticMarkup(
      <GoalEditPanel
        goal="用户明确提交的新目标"
        goalDraftKey="goal"
        busy={false}
        onGoalChange={() => {}}
        onSubmit={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(markup).toContain("修改项目目标");
    expect(markup).toContain("提交新目标并重规划");
    expect(markup).toContain("旧任务和报告在方案确认前保持可恢复");
  });

  it("分析页提供放弃重规划并恢复旧计划的入口", () => {
    const page = readFileSync("src/pages/ProjectPage.tsx", "utf8");
    const api = readFileSync("src/api.ts", "utf8");
    expect(page).toContain("放弃重规划并恢复旧目标与旧计划");
    expect(page).toContain("abandonReplan");
    expect(api).toContain("/replan/abandon");
  });

  it("共享预览明确旧计划保留到方案确认", () => {
    const markup = renderToStaticMarkup(
      <ReplanPanel
        preview={{
          cancelCount: 1,
          cancelKeys: ["T2"],
          keepCount: 1,
          keepKeys: ["T1"],
        }}
        reason="需要调整范围"
        reasonDraftKey="replan"
        busy={false}
        onReasonChange={() => {}}
        onSubmit={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(markup).toContain("旧任务、报告和验收口径在分析期间保持可恢复");
    expect(markup).toContain("应用新方案后将取消 1 个未完成任务");
    expect(markup).toContain("原子切换计划");
  });
});
