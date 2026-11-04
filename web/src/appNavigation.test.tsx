import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "./App";

vi.mock("./api", () => ({
  api: { health: vi.fn(async () => ({ ok: true, version: "0.1", git_hash: "test" })), listProjects: vi.fn(async () => []) },
  subscribeEvents: () => () => {},
}));
vi.mock("./pages/ProjectsPage", () => ({ ProjectsPage: () => <div>PROJECT_LIST</div> }));
vi.mock("./pages/ProjectPage", () => ({ ProjectPage: ({ id }: { id: string }) => <div>DETAIL_{id}</div> }));
vi.mock("./pages/AttentionPage", () => ({ AttentionPage: ({ onOpen }: { onOpen: (id: string, task: string) => void }) => <button onClick={() => onOpen("p1", "task/1")}>处理任务</button> }));
vi.mock("./pages/AgentsPage", () => ({ AgentsPage: () => null }));
vi.mock("./pages/UsagePage", () => ({ UsagePage: () => null }));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let rendered: ReactTestRenderer | undefined;

afterEach(async () => {
  if (rendered) await act(async () => rendered?.unmount());
  rendered = undefined;
  vi.unstubAllGlobals();
});

function browser(path: string) {
  const url = new URL(path, "http://localhost");
  const pushState = vi.fn();
  const replaceState = vi.fn();
  vi.stubGlobal("window", {
    location: { pathname: url.pathname, search: url.search, hash: url.hash, href: url.href },
    history: { state: null, pushState, replaceState },
    addEventListener: vi.fn(), removeEventListener: vi.fn(),
    setInterval: vi.fn(), clearInterval: vi.fn(), scrollTo: vi.fn(),
  });
  return { pushState, replaceState };
}

describe("应用导航", () => {
  it("待办任务参数保留在地址栏，项目详情只按 pathname 匹配", async () => {
    const { pushState } = browser("/attention");
    await act(async () => { rendered = create(<App />); });
    const open = rendered!.root.findAllByType("button").find(node => node.children.includes("处理任务"))!;
    await act(async () => open.props.onClick());
    expect(pushState).toHaveBeenCalledWith({}, "", "/projects/p1?tab=tasks&task=task%2F1");
    expect(JSON.stringify(rendered!.toJSON())).toContain("DETAIL_");
    expect(JSON.stringify(rendered!.toJSON())).not.toContain("PROJECT_LIST");
  });

  it("规范化尾斜杠不丢失任务定位和锚点", async () => {
    const { replaceState } = browser("/projects/p1/?tab=tasks&task=t1#result");
    await act(async () => { rendered = create(<App />); });
    expect(replaceState).toHaveBeenCalledWith(null, "", "/projects/p1?tab=tasks&task=t1#result");
    expect(rendered!.root.findByProps({ id: "main-content" }).props.tabIndex).toBe(-1);
    expect(rendered!.root.findAllByProps({ "aria-current": "page" })).toHaveLength(1);
  });
});
