import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "./api";
import { AttentionPage } from "./pages/AttentionPage";
import type { ProjectSummary } from "./types";

vi.mock("./api", async (importOriginal) => ({
  ...await importOriginal<typeof import("./api")>(),
  api: { listProjects: vi.fn() },
}));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let rendered: ReactTestRenderer | undefined;

afterEach(async () => {
  if (rendered) await act(async () => rendered?.unmount());
  rendered = undefined;
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("待办读取错误", () => {
  it("同一运行项目的多个待办独立呈现，并定位到对应任务", async () => {
    vi.stubGlobal("window", { setInterval: vi.fn(), clearInterval: vi.fn() });
    const action = { kind: "review" as const, reason: "等待里程碑人工验收", since: "2026-01-10T10:00:00Z", affected_tasks: 3, blocks_milestone: true, blocks_summary: true, project_advancing: true };
    vi.mocked(api.listProjects).mockResolvedValue([{ id: "p1", name: "并行交付", goal: "完成交付", workspace_type: "plain", stage: "executing", status: "running", updated_at: "2026-01-10T10:00:00Z", attention_items: [{ ...action, id: "a1", task: { id: "t1", key: "T1", title: "第一里程碑" } }, { ...action, id: "a2", task: { id: "t2", key: "T2", title: "第二里程碑" } }] }] as ProjectSummary[]);
    const open = vi.fn();
    await act(async () => { rendered = create(<AttentionPage onOpen={open} />); });
    const items = rendered!.root.findAllByType("article");
    expect(items).toHaveLength(2);
    const content = JSON.stringify(rendered!.toJSON());
    expect(content).toContain("项目仍可推进");
    expect(content).toContain("阻挡里程碑");
    await act(async () => items[1].props.onClick());
    expect(open).toHaveBeenCalledWith("p1", "t2");
  });
  it("失败不显示已清空的假象，提供重试，成功后才展示空状态", async () => {
    vi.stubGlobal("window", { setInterval: vi.fn(), clearInterval: vi.fn() });
    vi.mocked(api.listProjects).mockRejectedValueOnce(new Error("连接失败"));
    await act(async () => { rendered = create(<AttentionPage onOpen={() => {}} />); });
    expect(JSON.stringify(rendered!.toJSON())).toContain("连接失败");
    expect(JSON.stringify(rendered!.toJSON())).not.toContain("没有待处理事项");
    const retry = rendered!.root.findAllByType("button").find(node => node.children.includes("重试"))!;
    expect(retry).toBeDefined();
    vi.mocked(api.listProjects).mockResolvedValueOnce([]);
    await act(async () => retry.props.onClick());
    expect(JSON.stringify(rendered!.toJSON())).toContain("没有待处理事项");
    expect(JSON.stringify(rendered!.toJSON())).not.toContain("连接失败");
  });
});
