import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Agent, ProjectRolePool } from "./types";
import { AgentCard } from "./components/AgentCard";
import { AgentPoolPanel } from "./components/AgentPoolPanel";
import { agentFailureStatus } from "./presentation/agentFailure";

const RAW = "RAW_ERROR_MARKER_7a1d";
const NOW = Date.parse("2026-01-10T12:00:00Z");
const FUTURE = "2026-01-10T12:30:00Z";

function agent(overrides: Partial<Agent> = {}): Agent {
  return {
    id: "a1",
    name: "执行者一号",
    kind: "opencode",
    model: "gpt-5-codex",
    roles: ["executor"],
    max_concurrency: 2,
    allow_priority_fallback: false,
    enabled: true,
    command: [],
    model_args: [],
    cooldown_until: null,
    last_error: null,
    failure_kind: null,
    failure_summary: null,
    transient_failures: 0,
    availability: null,
    usage_limit: null,
    usage_window: null,
    created_at: "2026-01-10T10:00:00Z",
    updated_at: "2026-01-10T10:00:00Z",
    ...overrides,
  };
}

function failing(): Agent {
  return agent({
    cooldown_until: FUTURE,
    failure_kind: "rate_limit",
    transient_failures: 3,
    failure_summary: {
      kind: "rate_limit",
      message: "配额已用尽，请稍后重试",
      status: 429,
      retry_after_ms: 60000,
    },
    last_error: RAW,
  });
}

/** 截取卡片状态徽标行，用于断言该行只渲染一个徽标。 */
function statusBadgeFor(markup: string): string {
  const start = markup.indexOf('class="agent-status"');
  expect(start).toBeGreaterThan(0);
  return markup.slice(start, markup.indexOf("</div>", start));
}

/** 计算卡片状态徽标行中的徽标数。 */
function statusBadgeCount(markup: string): number {
  return (statusBadgeFor(markup).match(/<span class="badge /g) ?? []).length;
}

function renderCard(value: Agent): string {
  return renderToStaticMarkup(
    <AgentCard
      agent={value}
      now={NOW}
      onEdit={() => {}}
      onToggle={() => {}}
      onRemove={() => {}}
      onResetUsage={() => {}}
    />,
  );
}

describe("AgentCard 失败状态徽标", () => {
  it("失败信息合并为一行徽标，摘要进入 title，原始错误位于折叠元素内", () => {
    const markup = renderCard(failing());
    const status = statusBadgeFor(markup);
    expect((status.match(/<span class="badge /g) ?? []).length).toBe(1);
    expect(status).toContain("限流");
    expect(status).toContain("波动 3");
    expect(status).toContain("冷却至");
    const title = /title="([^"]*)"/.exec(status)?.[1] ?? "";
    expect(title).toContain("配额已用尽");
    expect(title).toContain("HTTP 429");
    // 摘要正文不单独占行：徽标行内只出现压缩文案，完整摘要只在 title 中。
    expect(status.replace(title, "")).not.toContain("配额已用尽");

    const statusEnd = markup.indexOf("</div>", markup.indexOf('class="agent-status"'));
    const detailsIndex = markup.indexOf('<details class="raw-error">');
    expect(detailsIndex).toBeGreaterThan(statusEnd);
    const detailsTag = markup.slice(detailsIndex, markup.indexOf(">", detailsIndex));
    expect(detailsTag).not.toContain("open");
    expect(markup.slice(detailsIndex)).toContain(RAW);
    expect(markup.slice(0, detailsIndex)).not.toContain(RAW);

    // 失败卡片不再使用整块警示横幅。
    expect(markup).not.toContain('class="warn-banner"');
  });

  it("无失败信息的卡片同样只有一行状态徽标", () => {
    const markup = renderCard(agent());
    const status = statusBadgeFor(markup);
    expect((status.match(/<span class="badge /g) ?? []).length).toBe(1);
    expect(status).toContain("运行正常");
    expect(markup).not.toContain("查看原始错误");
    expect(markup).not.toContain("agent-status-line");
  });

  it("失败与正常卡片的徽标数量差不超过 1", () => {
    const failure = statusBadgeCount(renderCard(failing()));
    const ok = statusBadgeCount(renderCard(agent()));
    expect(failure).toBe(1);
    expect(ok).toBe(1);
    expect(Math.abs(failure - ok)).toBeLessThanOrEqual(1);
  });

  it("只有冷却、没有失败时仍给出单行冷却徽标", () => {
    const markup = renderCard(agent({ cooldown_until: FUTURE }));
    const status = statusBadgeFor(markup);
    expect(status).toContain("冷却至");
    expect((status.match(/<span class="badge /g) ?? []).length).toBe(1);
  });

  it("冷却已过期时不渲染失败徽标", () => {
    const status = agentFailureStatus(
      agent({ cooldown_until: "2026-01-10T11:00:00Z" }),
      { now: NOW },
    );
    expect(status).toBeNull();
  });

  it("徽标文案始终单行，摘要完整保留在 title 中", () => {
    const status = agentFailureStatus(failing(), { now: NOW });
    expect(status).not.toBeNull();
    expect(status?.label).not.toContain("\n");
    expect(status?.title).toContain("配额已用尽，请稍后重试");
    expect(status?.title).toContain("供应商波动 3 次");
  });
});

function poolEntry(overrides: Partial<ProjectRolePool["entries"][number]> = {}) {
  return {
    agent_id: "a1",
    name: "执行者一号",
    kind: "opencode" as const,
    model: "gpt-5-codex",
    priority: 1,
    enabled: true,
    agent_enabled: true,
    project_enabled: true,
    available: true,
    unavailable_reason: null,
    max_concurrency: 2,
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

describe("AgentPoolPanel 失败摘要压缩", () => {
  it("失败摘要进入徽标 title，不再单独占行", () => {
    const roles: ProjectRolePool[] = [
      {
        role: "executor",
        entries: [
          poolEntry({
            failure_kind: "rate_limit",
            transient_failures: 4,
            failure_summary: {
              kind: "rate_limit",
              message: "POOL_SUMMARY_MARKER",
              status: 429,
              retry_after_ms: null,
            },
          }),
          poolEntry({ agent_id: "a2", name: "执行者二号" }),
        ],
      },
    ];
    const markup = renderToStaticMarkup(
      <AgentPoolPanel
        title="项目 Agent 池"
        subtitle="说明"
        roles={roles}
        defaultOpen
        onMove={async () => {}}
        onToggle={async () => {}}
      />,
    );
    expect(markup).not.toContain("agent-pool-reason");
    expect(markup).not.toContain(">POOL_SUMMARY_MARKER<");
    expect(markup).toContain("POOL_SUMMARY_MARKER");
    expect(markup).toContain("波动 4");
    const failureBadges = markup.match(/title="[^"]*POOL_SUMMARY_MARKER[^"]*"/g) ?? [];
    expect(failureBadges.length).toBe(1);
  });
});
