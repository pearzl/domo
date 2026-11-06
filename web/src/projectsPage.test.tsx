import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "./api";
import { ProjectsPage } from "./pages/ProjectsPage";
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

describe("项目总览快捷筛选", () => {
  it("选择总览卡片清空搜索与其他筛选，并保留完整列表的统计数值", async () => {
    vi.stubGlobal("window", { setInterval: vi.fn(), clearInterval: vi.fn() });
    const base = {
      goal: "交付目标", repo_path: "/repo", base_branch: "main", branch: "domo/project",
      stage: "executing", status: "running", attention_items: [], task_total: 3,
      task_completed: 1, created_at: "2026-01-10T10:00:00Z", updated_at: "2026-01-10T10:00:00Z",
    };
    vi.mocked(api.listProjects).mockResolvedValue([
      { ...base, id: "p1", name: "运行项目" },
      { ...base, id: "p2", name: "待确认项目", status: "blocked", attention_items: [{ kind: "confirm" }] },
      { ...base, id: "p3", name: "完成项目", stage: "completed", status: null },
    ] as ProjectSummary[]);
    await act(async () => { rendered = create(<ProjectsPage onOpen={() => {}} />); });
    const cards = () => rendered!.root.findAllByType("article").map(card => card.findByType("h2").children.join(""));
    const summary = rendered!.root.findByProps({ "aria-label": "项目总览与快捷筛选" });
    const shortcuts = summary.findAllByType("button");
    const counts = () => summary.findAllByType("strong").map(node => node.children.join(""));
    const search = rendered!.root.findByProps({ type: "search" });
    await act(async () => search.props.onChange({ target: { value: "完成项目" } }));
    expect(cards()).toEqual(["完成项目"]);

    await act(async () => shortcuts[1].props.onClick());
    expect(search.props.value).toBe("");
    expect(cards()).toEqual(["运行项目"]);
    expect(shortcuts[1].props["aria-pressed"]).toBe(true);

    await act(async () => shortcuts[2].props.onClick());
    expect(cards()).toEqual(["待确认项目"]);
    expect(shortcuts[1].props["aria-pressed"]).toBe(false);
    expect(shortcuts[2].props["aria-pressed"]).toBe(true);

    await act(async () => shortcuts[3].props.onClick());
    expect(cards()).toEqual(["完成项目"]);
    expect(counts()).toEqual(["3", "1", "1", "1"]);

    await act(async () => shortcuts[0].props.onClick());
    expect(cards()).toHaveLength(3);
    expect(shortcuts[0].props["aria-pressed"]).toBe(true);
  });
});
