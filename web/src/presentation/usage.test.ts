import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type {
  Agent,
  ProjectSummary,
  UsageGroup,
  UsageReport,
  UsageTotals,
} from "../types";
import {
  EMPTY_USAGE_FILTERS,
  currentUsageReport,
  restrictUsageToProject,
  usageGroupName,
  usageProjectOptions,
  usageRequestParams,
} from "./usage";
import { ROLE_LABELS } from "../ui";
import { UsageTable } from "../components/UsageGroupsTable";
import { readUsageProjectId, usagePath } from "../router";

function totals(overrides: Partial<UsageTotals> = {}): UsageTotals {
  return {
    jobs: 2,
    input_tokens: 100,
    output_tokens: 20,
    cache_read_tokens: 10,
    cache_write_tokens: 5,
    reasoning_tokens: 3,
    cost: 0.5,
    duration_ms: 1000,
    known_jobs: 2,
    unknown_jobs: 0,
    ...overrides,
  };
}

function group(overrides: Partial<UsageGroup> = {}): UsageGroup {
  return {
    key: "g",
    project_id: "p1",
    agent_id: null,
    agent_name: null,
    role: null,
    ...totals(),
    ...overrides,
  };
}

function report(groupBy: string): UsageReport {
  return {
    group_by: groupBy,
    filters: { project_id: "p1", agent_id: null, role: null, from: null, to: null },
    totals: totals(),
    groups: [group()],
  };
}

describe("usageRequestParams", () => {
  it("无筛选时只带维度", () => {
    expect(usageRequestParams(EMPTY_USAGE_FILTERS, "project")).toEqual({
      project_id: undefined,
      agent_id: undefined,
      role: undefined,
      from: undefined,
      to: undefined,
      group_by: "project",
    });
  });

  it("切换维度保留项目与其余筛选条件", () => {
    const filters = {
      ...EMPTY_USAGE_FILTERS,
      project_id: "p1",
      agent_id: "a1",
      role: "executor",
    };
    for (const dimension of ["project", "agent", "role"] as const) {
      expect(usageRequestParams(filters, dimension)).toMatchObject({
        project_id: "p1",
        agent_id: "a1",
        role: "executor",
        group_by: dimension,
      });
    }
  });

  it("时间范围转换成 ISO，无法解析时忽略", () => {
    const params = usageRequestParams(
      { ...EMPTY_USAGE_FILTERS, from: "2026-01-01T08:00", to: "bad" },
      "project",
    );
    expect(params.from).toBe(new Date("2026-01-01T08:00").toISOString());
    expect(params.to).toBeUndefined();
  });
});

describe("usageGroupName", () => {
  const projects = [{ id: "p1", name: "项目一" }] as unknown as ProjectSummary[];
  const agents = [{ id: "a1", name: "Agent 一" }] as unknown as Agent[];

  it("按维度给出可读名称", () => {
    expect(usageGroupName("project", group({ project_id: "p1" }), projects, agents)).toBe(
      "项目一",
    );
    expect(
      usageGroupName("agent", group({ agent_id: "a1" }), projects, agents),
    ).toBe("Agent 一");
    expect(
      usageGroupName("agent", group({ agent_id: null }), projects, agents),
    ).toBe("未记录 Agent");
    expect(
      usageGroupName("role", group({ role: "executor" }), projects, agents),
    ).toBe(ROLE_LABELS.executor);
  });
});

describe("currentUsageReport", () => {
  it("只返回与当前维度匹配的报告", () => {
    const project = report("project");
    const agent = report("agent");
    expect(currentUsageReport("project", project)).toBe(project);
    expect(currentUsageReport("agent", agent)).toBe(agent);
    expect(currentUsageReport("agent", project)).toBeNull();
    expect(currentUsageReport("role", project)).toBeNull();
    expect(currentUsageReport("project", null)).toBeNull();
  });
});

describe("usageProjectOptions", () => {
  it("项目列表项全部作为选项，选中项不在列表时追加一次", () => {
    const projects = [
      { id: "p1", name: "项目一" },
      { id: "p2", name: "项目二" },
    ] as ProjectSummary[];
    expect(usageProjectOptions("", projects)).toEqual([
      { value: "p1", label: "项目一" },
      { value: "p2", label: "项目二" },
    ]);
    expect(usageProjectOptions("p1", projects)).toHaveLength(2);
    expect(usageProjectOptions("p9", projects)).toEqual([
      { value: "p1", label: "项目一" },
      { value: "p2", label: "项目二" },
      { value: "p9", label: "p9（未在项目列表中）" },
    ]);
  });
});

describe("restrictUsageToProject", () => {
  it("按项目分组时收窄分组行并重算汇总", () => {
    const base = totals();
    const branch = (projectId: string) => ({
      ...base,
      key: projectId,
      project_id: projectId,
      agent_id: null,
      agent_name: null,
      role: null,
    });
    const partial = totals({ jobs: 3, known_jobs: 1, unknown_jobs: 2 });
    const report: UsageReport = {
      group_by: "project",
      filters: { project_id: "p1", agent_id: null, role: null, from: null, to: null },
      totals: base,
      groups: [branch("p1"), branch("p2"), { ...branch("p1"), ...partial, key: "p1b" }],
    };
    const scoped = restrictUsageToProject(report, "p1")!;
    expect(scoped.groups.map((group) => group.key)).toEqual(["p1", "p1b"]);
    expect(scoped.totals.jobs).toBe(base.jobs + 3);
    expect(scoped.totals.known_jobs).toBe(base.known_jobs + 1);
    expect(scoped.totals.unknown_jobs).toBe(2);
    expect(scoped.totals.input_tokens).toBe(base.input_tokens * 2);
    expect(scoped.totals.cost).toBeCloseTo(base.cost * 2);
  });

  it("按 Agent 或角色分组沿用报告汇总", () => {
    const base = totals();
    const report: UsageReport = {
      group_by: "agent",
      filters: { project_id: "p1", agent_id: null, role: null, from: null, to: null },
      totals: base,
      groups: [
        { ...base, key: "a1", project_id: null, agent_id: "a1", agent_name: "A", role: null },
      ],
    };
    expect(restrictUsageToProject(report, "p1")).toBe(report);
  });

  it("没有项目过滤时原样返回报告", () => {
    const base = totals();
    const report = {
      group_by: "agent",
      filters: { project_id: null, agent_id: null, role: null, from: null, to: null },
      totals: base,
      groups: [
        { ...base, key: "agent:a1", project_id: "p1", agent_id: "a1", agent_name: "A", role: null },
      ],
    } satisfies UsageReport;
    expect(restrictUsageToProject(report, "")).toBe(report);
    expect(restrictUsageToProject(null, "p1")).toBeNull();
  });
});

/// 项目页“查看用量明细”带走项目过滤、用量页读回并预选，按维度请求都带该项目。
describe("项目过滤在项目页与用量页之间传递", () => {
  it("地址栏中的项目过滤被解析为筛选值，并按维度请求带该项目", () => {
    const entered = new URL(usagePath("p1"), "http://localhost").search;
    const projectId = readUsageProjectId(entered);
    expect(projectId).toBe("p1");
    for (const dimension of ["project", "agent", "role"] as const) {
      const params = usageRequestParams(
        { ...EMPTY_USAGE_FILTERS, project_id: projectId ?? "" },
        dimension,
      );
      expect(params.project_id).toBe("p1");
      expect(params.group_by).toBe(dimension);
    }
  });

  it("项目维度报告收窄到该项目后只剩一张表与该项目的行", () => {
    const base = totals();
    const branch = (projectId: string, name: string) => ({
      ...base,
      key: projectId,
      project_id: projectId,
      agent_id: null,
      agent_name: name,
      role: null,
    });
    const full: UsageReport = {
      group_by: "project",
      filters: { project_id: null, agent_id: null, role: null, from: null, to: null },
      totals: { ...base, jobs: 4 },
      groups: [branch("p1", "P1"), branch("p2", "P2")],
    };
    const scoped = restrictUsageToProject(full, "p1");
    const markup = renderToStaticMarkup(
      createElement(UsageTable, {
        dimension: "project",
        report: scoped,
        projects: [
          { id: "p1", name: "项目一" },
          { id: "p2", name: "项目二" },
        ] as unknown as ProjectSummary[],
        agents: [],
        loading: false,
      }),
    );
    expect(markup.match(/<table/g) ?? []).toHaveLength(1);
    expect(markup).toContain("项目一");
    expect(markup).not.toContain("项目二");
  });
});
