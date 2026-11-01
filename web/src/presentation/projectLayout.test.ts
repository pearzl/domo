import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { ProjectDetail } from "../types";
import { ProjectOverview } from "../components/ProjectOverview";
import {
  PROJECT_BLOCK_KEYS,
  projectBlockOrder,
  type ProjectBlockKey,
} from "./projectLayout";
import projectPageSource from "../pages/ProjectPage.tsx?raw";
import projectHeaderSource from "../components/ProjectHeader.tsx?raw";

const STAGES = ["analysis", "executing", "summarizing", "completed"] as const;

describe("projectBlockOrder 阶段顺序", () => {
  it("analysis 阶段分析先于任务", () => {
    expect(projectBlockOrder("analysis").slice(0, 2)).toEqual([
      "analysis",
      "tasks",
    ]);
  });

  it("executing 阶段任务先于分析记录", () => {
    expect(projectBlockOrder("executing").slice(0, 2)).toEqual([
      "tasks",
      "analysis",
    ]);
  });

  it("summarizing 与 completed 阶段报告先于任务", () => {
    for (const stage of ["summarizing", "completed"] as const) {
      expect(projectBlockOrder(stage).slice(0, 2)).toEqual([
        "report",
        "tasks",
      ]);
    }
  });

  it("阶段未知时回退到任务优先且不抛错", () => {
    for (const stage of [null, undefined, "", "archived", "running"]) {
      expect(() => projectBlockOrder(stage)).not.toThrow();
      expect(projectBlockOrder(stage)[0]).toBe("tasks");
    }
  });

  it("始终保留全部顶层区块，且每个区块只出现一次", () => {
    for (const stage of STAGES) {
      const order = projectBlockOrder(stage);
      expect([...order].sort()).toEqual([...PROJECT_BLOCK_KEYS].sort());
    }
    for (const stage of [null, undefined, "unknown"]) {
      expect(projectBlockOrder(stage)).toHaveLength(PROJECT_BLOCK_KEYS.length);
      expect(new Set(projectBlockOrder(stage)).size).toBe(
        PROJECT_BLOCK_KEYS.length,
      );
    }
  });

  it("返回的数组可安全修改，不影响下一次调用", () => {
    const first = projectBlockOrder("executing");
    first.reverse();
    expect(projectBlockOrder("executing")[0]).toBe("tasks");
  });
});

describe("项目详情页区块布局", () => {
  const blockMap = 'projectBlockOrder(project.stage).map';

  it("按 projectLayout 输出顶层区块", () => {
    expect(projectPageSource).toContain(blockMap);
    for (const key of ["analysis", "tasks", "report"] as ProjectBlockKey[]) {
      expect(projectPageSource).toContain(`block === "${key}"`);
    }
    expect(projectPageSource).toContain('<Fragment key={block}>');
  });

  it("项目 Agent 池为顶层面板，且排在分析、任务与报告之后", () => {
    const mapIndex = projectPageSource.indexOf(blockMap);
    const poolIndex = projectPageSource.indexOf("<AgentPoolPanel", mapIndex);
    expect(mapIndex).toBeGreaterThan(0);
    expect(poolIndex).toBeGreaterThan(mapIndex);
    expect(projectPageSource).not.toContain("项目设置");
    expect(projectPageSource).not.toContain("project-settings");
  });

  it("页头交给 ProjectHeader，并展示阶段、状态、进度与运行作业数", () => {
    expect(projectPageSource).toContain("<ProjectHeader");
    const start = projectHeaderSource.indexOf(
      '<header className="project-header panel">',
    );
    expect(start).toBeGreaterThan(0);
    expect(projectHeaderSource).toContain("<ProjectStatusBar");
    expect(projectHeaderSource).toContain("<StageBadge");
    expect(projectHeaderSource).toContain("<StatusBadge");
    expect(projectHeaderSource).toContain("任务进度");
    expect(projectHeaderSource).toContain("运行作业");
  });

  it("项目源信息默认收起，并提供逐项复制入口", () => {
    const start = projectHeaderSource.indexOf(
      '<details className="project-source">',
    );
    expect(start).toBeGreaterThan(0);
    const openTag = projectHeaderSource.slice(
      start,
      projectHeaderSource.indexOf(">", start),
    );
    expect(openTag).not.toContain("open");
    expect(projectHeaderSource).toContain("<CopyButton");
    expect(projectHeaderSource).toContain("项目源信息");
  });

  it("项目作业区使用默认最近记录与展开入口", () => {
    const start = projectPageSource.indexOf("{projectJobs.length > 0 ? (");
    const section = projectPageSource.slice(
      start,
      projectPageSource.indexOf(") : null}", start),
    );
    expect(start).toBeGreaterThan(0);
    expect(section).toContain("<JobRecordList");
    expect(section).not.toContain("limit={0}");
  });
});

function overviewDetail(): ProjectDetail {
  return {
    project: {
      id: "p1",
      name: "示例项目",
      goal: "交付示例",
      repo_path: "/repo",
      base_branch: "main",
      branch: "domo/p1",
      workspace: "/ws",
      stage: "executing",
      status: null,
      blocked_reason: null,
      report: null,
      replan_reason: null,
      replan_source: null,
      replan_requested_at: null,
      confirm_mode: "manual",
      confirmed_at: null,
      completion_mode: "tasks",
      merged_at: null,
      merged_commit: null,
      created_at: "2026-01-10T10:00:00Z",
      updated_at: "2026-01-10T10:00:00Z",
    },
    tasks: [],
    jobs: [],
    messages: [],
    events: [],
    agents: [],
    proposal: null,
    usage: {
      totals: {
        jobs: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        cost: 0,
        duration_ms: 0,
        known_jobs: 0,
        unknown_jobs: 0,
      },
      by_agent: [],
      by_role: [],
    },
  } as unknown as ProjectDetail;
}

describe("ProjectOverview 信息密度", () => {
  it("不渲染术语说明卡片，保留状态、任务与用量统计", () => {
    const markup = renderToStaticMarkup(
      createElement(ProjectOverview, { detail: overviewDetail() }),
    );
    expect(markup).not.toContain("术语说明");
    expect(markup).not.toContain("任务 = 项目内可验收单元");
    expect(markup).toContain("项目概览");
    expect(markup).toContain("任务完成");
    expect(markup).toContain("作业数");
    expect(markup).toContain("项目用时");
  });
});
