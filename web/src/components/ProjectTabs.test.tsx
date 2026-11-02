import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ProjectTabs, tabButtonId, tabPanelId } from "./ProjectTabs";
import {
  PROJECT_TAB_LABELS,
  projectBlockOrder,
} from "../presentation/projectLayout";

function render(overrides: Partial<Parameters<typeof ProjectTabs>[0]> = {}) {
  return renderToStaticMarkup(
    <ProjectTabs
      order={projectBlockOrder("analysis")}
      active="analysis"
      onSelect={() => {}}
      {...overrides}
    />,
  );
}

describe("ProjectTabs ARIA 语义与徽标", () => {
  it("使用 tablist/tab 语义，当前选项卡 aria-selected 且只有它可 Tab 聚焦", () => {
    const markup = render();
    expect(markup).toContain('role="tablist"');
    expect((markup.match(/role="tab"/g) ?? []).length).toBe(
      projectBlockOrder("analysis").length,
    );
    expect(markup).toContain('aria-selected="true"');
    expect((markup.match(/aria-selected="false"/g) ?? []).length).toBe(
      projectBlockOrder("analysis").length - 1,
    );
    // roving tabindex：选中的是 0，其余是 -1。
    expect((markup.match(/tabindex="0"/g) ?? []).length).toBe(1);
    expect(markup).toContain(`aria-controls="${tabPanelId("analysis")}"`);
    expect(markup).toContain(`id="${tabButtonId("tasks")}"`);
  });

  it("选项卡按阶段顺序渲染文案", () => {
    const markup = render();
    const order = projectBlockOrder("analysis");
    const indexes = order.map((key) =>
      markup.indexOf(PROJECT_TAB_LABELS[key]),
    );
    expect(indexes.every((index) => index > 0)).toBe(true);
    expect([...indexes].sort((a, b) => a - b)).toEqual(indexes);
  });

  it("渲染徽标：任务数、待确认方案与报告就绪", () => {
    const markup = render({
      badges: {
        analysis: "待确认方案",
        tasks: "任务 3",
        report: "报告就绪",
      },
    });
    expect(markup).toContain("待确认方案");
    expect(markup).toContain("任务 3");
    expect(markup).toContain("报告就绪");
    expect(markup).toContain('class="project-tab-badge"');
  });

  it("没有徽标时不渲染空徽标节点", () => {
    const markup = render({ badges: {} });
    expect(markup).not.toContain("project-tab-badge");
  });
});
