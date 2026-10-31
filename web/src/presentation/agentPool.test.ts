import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { ProjectAgentEntry, ProjectRolePool, Role } from "../types";
import { nextRoleTab, resolveActiveRole, roleTabs } from "./agentPool";
import { AgentPoolPanel } from "../components/AgentPoolPanel";

function entry(overrides: Partial<ProjectAgentEntry> = {}): ProjectAgentEntry {
  return {
    agent_id: "a1",
    name: "Agent 1",
    kind: "cli",
    model: "m1",
    priority: 0,
    enabled: true,
    agent_enabled: true,
    project_enabled: true,
    available: true,
    unavailable_reason: null,
    max_concurrency: 1,
    allow_priority_fallback: false,
    cooldown_until: null,
    last_error: null,
    failure_kind: null,
    failure_summary: null,
    transient_failures: 0,
    availability: null,
    availability_reason: null,
    usage_limit: null,
    usage_window: null,
    ...overrides,
  };
}

function pool(role: Role, names: string[], unavailable: number = 0): ProjectRolePool {
  return {
    role,
    entries: names.map((name, index) =>
      entry({
        agent_id: `${role}-${index}`,
        name,
        available: index < names.length - unavailable,
      }),
    ),
  };
}

const THREE_ROLES: ProjectRolePool[] = [
  pool("planner", ["Planner A", "Planner B"]),
  pool("executor", ["Executor A", "Executor B", "Executor C"], 1),
  pool("verifier", []),
];

const noopAsync = () => Promise.resolve();

function render(overrides: Partial<Parameters<typeof AgentPoolPanel>[0]> = {}): string {
  return renderToStaticMarkup(
    createElement(AgentPoolPanel, {
      title: "项目 Agent 池",
      subtitle: "说明",
      roles: THREE_ROLES,
      onMove: noopAsync,
      onReorder: noopAsync,
      onToggle: noopAsync,
      defaultOpen: true,
      ...overrides,
    }),
  );
}

function countMatches(html: string, pattern: RegExp): number {
  return (html.match(pattern) ?? []).length;
}

describe("roleTabs", () => {
  it("保持服务端角色顺序并给出标签与计数", () => {
    const tabs = roleTabs(THREE_ROLES);
    expect(tabs.map((tab) => tab.role)).toEqual(["planner", "executor", "verifier"]);
    expect(tabs.map((tab) => tab.label)).toEqual(["规划", "执行", "验收"]);
    expect(tabs.map((tab) => tab.count)).toEqual([2, 3, 0]);
    expect(tabs.map((tab) => tab.unavailableCount)).toEqual([0, 1, 0]);
  });

  it("空角色计数为 0", () => {
    const tabs = roleTabs([pool("verifier", [])]);
    expect(tabs).toEqual([
      { role: "verifier", label: "验收", count: 0, unavailableCount: 0 },
    ]);
  });
});

describe("resolveActiveRole", () => {
  const tabs = roleTabs(THREE_ROLES);

  it("保留合法请求角色", () => {
    expect(resolveActiveRole(tabs, "verifier")).toBe("verifier");
  });

  it("忽略不存在的角色", () => {
    const withoutExecutor = roleTabs([pool("planner", ["P"]), pool("verifier", ["V"])]);
    expect(resolveActiveRole(withoutExecutor, "executor")).toBe("planner");
  });

  it("缺省时取第一个有 Agent 的角色", () => {
    const leadingEmpty = roleTabs([pool("planner", []), pool("verifier", ["V"])]);
    expect(resolveActiveRole(leadingEmpty)).toBe("verifier");
  });

  it("全部为空时取第一个角色", () => {
    const empties = roleTabs([pool("planner", []), pool("executor", [])]);
    expect(resolveActiveRole(empties)).toBe("planner");
  });
});

describe("nextRoleTab", () => {
  const tabs = roleTabs(THREE_ROLES);

  it("左右移动并首尾循环", () => {
    expect(nextRoleTab(tabs, "planner", "next")).toBe("executor");
    expect(nextRoleTab(tabs, "executor", "next")).toBe("verifier");
    expect(nextRoleTab(tabs, "verifier", "next")).toBe("planner");
    expect(nextRoleTab(tabs, "planner", "previous")).toBe("verifier");
    expect(nextRoleTab(tabs, "verifier", "previous")).toBe("executor");
  });
});

describe("AgentPoolPanel 选项卡渲染", () => {
  it("每个角色一个 tab，只渲染默认角色的 Agent", () => {
    const html = render();
    expect(countMatches(html, /role="tablist"/g)).toBe(1);
    expect(countMatches(html, /role="tab"/g)).toBe(3);
    expect(countMatches(html, /role="tabpanel"/g)).toBe(1);
    expect(countMatches(html, /aria-selected="true"/g)).toBe(1);
    expect(html).toMatch(/aria-selected="true"[^>]*>[^<]*<span[^>]*>规划/);

    expect(html).toContain("Planner A");
    expect(html).toContain("Planner B");
    expect(html).not.toContain("Executor A");
    expect(html).not.toContain("Verifier");

    // tab 上显示 Agent 数与不可用数
    expect(html).toContain("2 个");
    expect(html).toContain("3 个 · 1 不可用");
    expect(html).toContain("0 个");
  });

  it("同一角色的 Agent 行只出现一次", () => {
    const html = render();
    expect(countMatches(html, /Planner A/g)).toBe(1);
  });

  it("传入 activeRole 时默认展示执行角色", () => {
    const html = render({ activeRole: "executor" });
    expect(countMatches(html, /role="tab"/g)).toBe(3);
    expect(html).toContain("Executor A");
    expect(html).not.toContain("Planner A");
    expect(html).toMatch(/aria-selected="true"[^>]*>[^<]*<span[^>]*>执行/);
  });

  it("请求角色在本次数据中不存在时回落到第一个有 Agent 的角色", () => {
    const html = render({
      activeRole: "executor",
      roles: [pool("planner", []), pool("verifier", ["Verifier A"])],
    });
    expect(html).toContain("Verifier A");
    expect(html).toMatch(/aria-selected="true"[^>]*>[^<]*<span[^>]*>验收/);
  });

  it("请求角色存在但没有 Agent 时仍选中该角色并提示为空", () => {
    const html = render({
      activeRole: "verifier",
      roles: [pool("planner", []), pool("executor", ["Executor A"]), pool("verifier", [])],
    });
    expect(html).toContain("这个角色还没有 Agent。");
    expect(html).not.toContain("Executor A");
    expect(html).toMatch(/aria-selected="true"[^>]*>[^<]*<span[^>]*>验收/);
  });

  it("默认取第一个有 Agent 的角色", () => {
    const html = render({
      roles: [pool("planner", []), pool("executor", ["Executor A"]), pool("verifier", ["Verifier A"])],
    });
    expect(html).toContain("Executor A");
    expect(html).not.toContain("Verifier A");
    expect(html).toMatch(/aria-selected="true"[^>]*>[^<]*<span[^>]*>执行/);
  });

  it("渲染顺序与服务端角色顺序一致", () => {    const html = render({
      roles: [pool("verifier", ["Verifier A"]), pool("planner", ["Planner A"])],
    });
    const labels = Array.from(html.matchAll(/agent-pool-tab-label">([^<]+)</g)).map(
      (match) => match[1],
    );
    expect(labels).toEqual(["验收", "规划"]);
    expect(html).toContain("Verifier A");
    expect(html).not.toContain("Planner A");
  });

  it("任务池展示项目层停用并禁用任务开关", () => {
    const html = render({
      activeRole: "executor",
      scope: "task",
      roles: [
        {
          role: "executor",
          entries: [entry({ name: "Masked", project_enabled: false })],
        },
      ],
    });
    expect(html).toContain("项目已停用");
    expect(html).toMatch(/disabled="" title="项目 Agent 池已停用该 Agent/);
  });
});

describe("AgentPoolPanel 让位标识", () => {
  it("开启 allow_priority_fallback 的条目渲染可让位徽标", () => {
    const html = render({
      roles: [
        {
          role: "planner",
          entries: [
            // 接口缺少该字段时按关闭处理。
            entry({
              agent_id: "p-missing",
              name: "Planner Missing",
              allow_priority_fallback: undefined as unknown as boolean,
            }),
            entry({ agent_id: "p-open", name: "Planner Open", allow_priority_fallback: true }),
          ],
        },
      ],
    });
    expect(countMatches(html, /可让位/g)).toBe(1);
    expect(html).toMatch(/title="[^"]*并发已满[^"]*"[^>]*>可让位/);
  });

  it("关闭 allow_priority_fallback 的条目不渲染可让位徽标", () => {
    const html = render({
      roles: [
        {
          role: "planner",
          entries: [entry({ agent_id: "p-closed", name: "Planner Closed" })],
        },
      ],
    });
    expect(html).not.toContain("可让位");
  });
});
