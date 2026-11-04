import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { ProjectDetail } from "../types";
import { DeliveryPanel } from "./DeliveryPanel";

const detail = {
  project: { id: "p", status: "running", report: "报告结果与引用" },
  delivery: {
    current_contract_id: 2, contracts: [
      { id: 1, goal: "目标", proposal: null, source: "goal", created_at: "2026-01-01" },
      { id: 2, goal: "目标", proposal: { acceptance: ["引用可核对"], verification: [] }, source: "plan", created_at: "2026-01-02" },
    ], limits: { cost_budget: 5, task_attempt_limit: 3, replan_limit: 2 },
    known_cost: 1.25, unknown_cost_jobs: 2,
  },
} as unknown as ProjectDetail;

describe("DeliveryPanel", () => {
  it("交付概览显示契约、标准、结果与费用未知边界", () => {
    const markup = renderToStaticMarkup(<DeliveryPanel detail={detail} onSaved={() => {}} />);
    for (const text of ["契约 #", "引用可核对", "报告结果与引用", "1.2500", "费用未知", "预算不能作为账单硬上限", "授权继续"]) {
      expect(markup).toContain(text);
    }
    expect(markup).not.toContain("<details open");
  });
  it("归档视图只读", () => {
    const archived = { ...detail, project: { ...detail.project, status: "archived" } };
    const markup = renderToStaticMarkup(<DeliveryPanel detail={archived} onSaved={() => {}} />);
    expect(markup).not.toContain("授权继续");
    expect(markup).toContain("契约版本与方案差异");
  });
});
