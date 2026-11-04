import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type {
  ProjectSummary,
  UsageGroup,
  UsageReport,
  UsageTotals,
} from "./types";
import { UsageTable } from "./components/UsageGroupsTable";
import { UsageSummary } from "./components/UsageSummary";
import { UsageMetricValue } from "./components/UsageMetricValue";

function totals(overrides: Partial<UsageTotals> = {}): UsageTotals {
  const known = overrides.known_jobs ?? 3;
  return {
    jobs: 3,
    input_tokens: 1200,
    output_tokens: 300,
    cache_read_tokens: 100,
    cache_write_tokens: 50,
    reasoning_tokens: 30,
    cost: 0.25,
    duration_ms: 60000,
    known_jobs: 3,
    unknown_jobs: 0,
    known_metrics: { input_tokens: known, output_tokens: known, cache_read_tokens: known, cache_write_tokens: known, reasoning_tokens: known, cost: known },
    ...overrides,
  };
}

function group(overrides: Partial<UsageGroup>): UsageGroup {
  return {
    key: "g",
    project_id: null,
    agent_id: null,
    agent_name: null,
    role: null,
    ...totals(),
    ...overrides,
  };
}

const PROJECT_REPORT: UsageReport = {
  group_by: "project",
  filters: { project_id: "p1", agent_id: null, role: null, from: null, to: null },
  totals: totals(),
  groups: [group({ key: "project:p1", project_id: "p1" })],
};

const AGENT_REPORT: UsageReport = {
  group_by: "agent",
  filters: { project_id: "p1", agent_id: null, role: null, from: null, to: null },
  totals: totals(),
  groups: [group({ key: "agent:a1", agent_id: "a1", agent_name: "Agent 一" })],
};

const ROLE_REPORT: UsageReport = {
  group_by: "role",
  filters: { project_id: "p1", agent_id: null, role: null, from: null, to: null },
  totals: totals(),
  groups: [group({ key: "role:executor", role: "executor" })],
};

const PROJECTS = [{ id: "p1", name: "项目一" }] as unknown as ProjectSummary[];

function render(dimension: "project" | "agent" | "role", report: UsageReport | null) {
  return renderToStaticMarkup(
    <UsageTable
      dimension={dimension}
      report={report}
      projects={PROJECTS}
      agents={[]}
      loading={false}
    />,
  );
}

describe("UsageTable", () => {
  it("token 完整但费用缺失时表格和项目概览不显示零费用", () => {
    const tokenOnly = totals({ cost: 0, known_metrics: { input_tokens: 3, output_tokens: 2, cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0, cost: 0 } });
    const report = { ...PROJECT_REPORT, totals: tokenOnly, groups: [group({ ...tokenOnly, project_id: "p1" })] };
    const table = render("project", report);
    const summary = renderToStaticMarkup(<UsageSummary projectId="p1" usage={{ totals: tokenOnly, by_agent: [], by_role: [] }} />);
    const cost = renderToStaticMarkup(<UsageMetricValue totals={tokenOnly} metric="cost" />);
    expect(cost).toContain(">—</span>");
    expect(cost).not.toContain(">0<");
    for (const markup of [table, summary]) {
      expect(markup).not.toContain("0.0000");
      expect(markup).toContain("2/3");
    }
    expect(summary).toContain("费用覆盖 0/3");
  });
  it("一次只渲染一张表，换维度后表头与行数据随之变化", () => {
    const project = render("project", PROJECT_REPORT);
    const agent = render("agent", AGENT_REPORT);
    const role = render("role", ROLE_REPORT);
    for (const tree of [project, agent, role]) {
      expect(tree.match(/<table/g) ?? []).toHaveLength(1);
    }
    expect(project).toContain('<th scope="col">项目</th>');
    expect(project).toContain("项目一");
    expect(agent).toContain('<th scope="col">Agent</th>');
    expect(agent).toContain("Agent 一");
    expect(role).toContain('<th scope="col">角色</th>');
    expect(role).toContain("执行");
  });

  it("报告维度与当前维度不一致时不渲染旧维度的表格", () => {
    expect(render("project", PROJECT_REPORT)).toContain("<table");
    expect(render("project", AGENT_REPORT)).not.toContain("<table");
    expect(render("project", null)).not.toContain("<table");
  });

  it("明细滚动区可通过键盘聚焦，并有明确名称和表格说明", () => {
    const markup = render("project", PROJECT_REPORT);
    expect(markup).toContain('role="region"');
    expect(markup).toContain('tabindex="0"');
    expect(markup).toContain("可横向滚动");
    expect(markup).toContain('<caption class="sr-only">');
  });
});
