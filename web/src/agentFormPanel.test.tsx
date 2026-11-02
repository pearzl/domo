import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Agent } from "./types";
import {
  AgentFormPanel,
  buildAvailability,
  buildUsageLimit,
  emptyForm,
  formForAgent,
  isAgentFormDraft,
  parseJsonArray,
} from "./components/AgentFormPanel";
import type { AgentFormState } from "./components/AgentFormPanel";

function agent(): Agent {
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
  };
}

function renderPanel(form: AgentFormState, editing: Agent | null = null): string {
  return renderToStaticMarkup(
    <AgentFormPanel
      agent={editing}
      form={form}
      onChange={() => {}}
      onSubmit={() => {}}
      onClose={() => {}}
    />,
  );
}

/** 截取分段标题（summary 文本），用于断言四个分组按顺序存在。 */
function sectionTitles(markup: string): string[] {
  return Array.from(markup.matchAll(/<summary>([^<]*)<\/summary>/g)).map((m) => m[1]);
}

describe("AgentFormPanel 浮层契约", () => {
  it("面板是模态对话框，带 role=dialog、aria-modal 与标题关联", () => {
    const markup = renderPanel(emptyForm());
    const tag = /<aside[^>]*>/.exec(markup)?.[0] ?? "";
    expect(tag).toContain('role="dialog"');
    expect(tag).toContain('aria-modal="true"');
    const labelledBy = /aria-labelledby="([^"]+)"/.exec(tag)?.[1];
    expect(labelledBy).toBeTruthy();
    expect(markup).toContain(`id="${labelledBy}"`);
    expect(markup).toContain("添加 Agent");
  });

  it("包含基本/调度/限额/命令四个分段标题，且默认展开", () => {
    const markup = renderPanel(emptyForm());
    expect(sectionTitles(markup)).toEqual(["基本", "调度", "限额", "命令"]);
    const opens = markup.match(/<details class="agent-form-section" open/g) ?? [];
    expect(opens.length).toBe(4);
  });

  it("编辑已有 Agent 时复用同一面板并显示 Agent 名称", () => {
    const value = agent();
    const markup = renderPanel(formForAgent(value), value);
    expect(markup).toContain("编辑 执行者一号");
    const tag = /<aside[^>]*>/.exec(markup)?.[0] ?? "";
    expect(tag).toContain('role="dialog"');
    expect(tag).toContain('aria-modal="true"');
    expect(sectionTitles(markup)).toEqual(["基本", "调度", "限额", "命令"]);
    expect(markup).toContain('value="gpt-5-codex"');
  });

  it("面板渲染为遮罩浮层，关闭按钮与保存按钮可用", () => {
    const markup = renderPanel(emptyForm());
    expect(markup).toContain('class="drawer-backdrop agent-form-backdrop"');
    expect(markup).toContain('class="drawer agent-form-drawer"');
    expect(markup).toContain('type="submit"');
    expect(markup).toContain("关闭");
    expect(markup).toContain("取消");
  });

  it("错误信息显示在面板内部而不是页面顶部", () => {
    const markup = renderToStaticMarkup(
      <AgentFormPanel
        agent={null}
        form={emptyForm()}
        error="PANEL_ERROR_MARKER"
        onChange={() => {}}
        onSubmit={() => {}}
        onClose={() => {}}
      />,
    );
    const bodyIndex = markup.indexOf('class="drawer-body agent-form-body"');
    expect(bodyIndex).toBeGreaterThan(0);
    expect(markup.indexOf("PANEL_ERROR_MARKER")).toBeGreaterThan(bodyIndex);
  });
});

describe("Agent 表单字段与校验保持不变", () => {
  it("草稿形状校验接受表单状态、拒绝缺字段", () => {
    expect(isAgentFormDraft(emptyForm())).toBe(true);
    expect(isAgentFormDraft({ ...emptyForm(), slots: [] })).toBe(false);
    expect(isAgentFormDraft(null)).toBe(false);
  });

  it("可用时段未选任何一天时报错，选中断后生成 weekday", () => {
    const form = { ...emptyForm(), availabilityEnabled: true };
    expect(() => buildAvailability(form)).toThrow("至少选择一天");
    const slots = form.slots.map((slot, index) =>
      index === 0 ? { ...slot, enabled: true } : slot,
    );
    expect(buildAvailability({ ...form, slots })).toEqual({
      enabled: true,
      slots: [{ weekday: 1, start: "09:00", end: "18:00" }],
    });
  });

  it("用量限额必须大于 0，命令模板必须是字符串数组", () => {
    expect(() =>
      buildUsageLimit({ ...emptyForm(), usageLimitEnabled: true, usageLimit: "0" }),
    ).toThrow("限额必须大于 0");
    expect(parseJsonArray('["codex","exec"]')).toEqual(["codex", "exec"]);
    expect(() => parseJsonArray("[1]")).toThrow("字符串数组");
  });
});
