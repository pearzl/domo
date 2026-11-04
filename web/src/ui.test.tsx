import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ErrorBox, PageGuide, Progress } from "./ui";
import { AgentPoolPanel } from "./components/AgentPoolPanel";

describe("共享界面语义", () => {
  it("进度提供读屏可读数值，并区分尚无任务与零进度", () => {
    const progress = renderToStaticMarkup(<Progress total={5} completed={2} />);
    expect(progress).toContain('role="progressbar"');
    expect(progress).toContain('aria-valuenow="40"');
    expect(progress).toContain('aria-valuetext="已完成 2 / 5 项任务"');
    expect(renderToStaticMarkup(<Progress total={0} completed={0} />)).toContain("尚无任务");
  });

  it("异常进度不会产生越界或无效宽度", () => {
    expect(renderToStaticMarkup(<Progress total={5} completed={9} />)).toContain('aria-valuenow="100"');
    expect(renderToStaticMarkup(<Progress total={5} completed={-1} />)).toContain('aria-valuenow="0"');
    expect(renderToStaticMarkup(<Progress total={NaN} completed={2} />)).toContain('aria-valuenow="0"');
  });

  it("错误可被及时读出，长说明默认折叠但保留正文", () => {
    expect(renderToStaticMarkup(<ErrorBox message="加载失败" />)).toContain('role="alert"');
    const guide = renderToStaticMarkup(<PageGuide>工作区说明</PageGuide>);
    expect(guide).toContain("<summary>使用说明</summary>");
    expect(guide).not.toContain(" open=");
    expect(guide).toContain("工作区说明");
  });

  it("同页多个 Agent 池拥有独立的选项卡与面板关联", () => {
    const props = { title: "团队", subtitle: "角色", roles: [{ role: "planner" as const, entries: [] }], onMove: async () => {}, onToggle: async () => {} };
    const markup = renderToStaticMarkup(<><AgentPoolPanel {...props} /><AgentPoolPanel {...props} /></>);
    const ids = [...markup.matchAll(/\sid="([^"]+)"/g)].map(match => match[1]);
    expect(new Set(ids).size).toBe(ids.length);
    const controlled = [...markup.matchAll(/aria-controls="([^"]+)"/g)].map(match => match[1]);
    expect(controlled).toHaveLength(2);
    expect(controlled.every(id => ids.includes(id))).toBe(true);
  });
});
