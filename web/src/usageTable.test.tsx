import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type {
  ProjectSummary,
  UsageGroup,
  UsageReport,
  UsageTotals,
} from "./types";
import { UsageTable } from "./components/UsageGroupsTable";

function totals(overrides: Partial<UsageTotals> = {}): UsageTotals {
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
  it("一次只渲染一张表，换维度后表头与行数据随之变化", () => {
    const project = render("project", PROJECT_REPORT);
    const agent = render("agent", AGENT_REPORT);
    const role = render("role", ROLE_REPORT);
    for (const tree of [project, agent, role]) {
      expect(tree.match(/<table/g) ?? []).toHaveLength(1);
    }
    expect(project).toContain("<th>项目</th>");
    expect(project).toContain("项目一");
    expect(agent).toContain("<th>Agent</th>");
    expect(agent).toContain("Agent 一");
    expect(role).toContain("<th>角色</th>");
    expect(role).toContain("执行");
  });

  it("报告维度与当前维度不一致时不渲染旧维度的表格", () => {
    expect(render("project", PROJECT_REPORT)).toContain("<table");
    expect(render("project", AGENT_REPORT)).not.toContain("<table");
    expect(render("project", null)).not.toContain("<table");
  });
});
