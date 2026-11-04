import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { Agent, ProjectDetail } from "../types";
import { resourceRequest } from "./ResourcePanel";
import { GlobalQueue } from "./TeamOperations";

vi.mock("./ResourcePanel", () => ({ resourceRequest: vi.fn() }));
vi.mock("../api", async (original) => ({ ...await original<typeof import("../api")>(), api: { getProject: vi.fn() } }));
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
const queue = { queued: [], running: [{ id: "job_internal", project_id: "p1", task_id: "t1", agent_id: "agt_internal", role: "executor" }] };
let rendered: ReactTestRenderer | undefined;
const intervals: Array<() => void> = [];
const text = (node: ReactTestInstance): string => node.children.map(child => typeof child === "string" ? child : text(child)).join("");

beforeEach(() => {
  intervals.length = 0;
  vi.stubGlobal("window", { setInterval: vi.fn((callback: () => void) => { intervals.push(callback); return 1; }), clearInterval: vi.fn() });
  vi.mocked(resourceRequest).mockResolvedValue(queue);
  vi.mocked(api.getProject).mockResolvedValue({ project: { id: "p1", name: "真实工作流示例" }, tasks: [{ id: "t1", key: "T2", title: "实现接口" }] } as ProjectDetail);
});

afterEach(async () => {
  if (rendered) await act(async () => rendered?.unmount());
  rendered = undefined;
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});

describe("全局队列呈现", () => {
  it("名称与任务取代原始 ID，运行明细默认收起且按钮统一", async () => {
    await act(async () => { rendered = create(<GlobalQueue agents={[{ id: "agt_internal", name: "执行助理" } as Agent]} />); });
    const content = text(rendered!.root);
    expect(content).toContain("执行助理");
    expect(content).toContain("真实工作流示例 · T2 实现接口");
    expect(content).not.toContain("job_internal");
    expect(content).not.toContain("agt_internal");
    expect(content).not.toContain("executor");
    expect(rendered!.root.findByProps({ className: "queue-details" }).props.open).not.toBe(true);
    expect(rendered!.root.findAllByType("button").every(button => Boolean(button.props.className))).toBe(true);
  });

  it("轮询读取成功不覆盖操作失败", async () => {
    await act(async () => { rendered = create(<GlobalQueue />); });
    vi.mocked(resourceRequest).mockRejectedValueOnce(new Error("暂停未成功，请检查项目状态"));
    const pause = rendered!.root.findAllByType("button").find(button => text(button) === "暂停后续派发")!;
    await act(async () => pause.props.onClick());
    expect(text(rendered!.root)).toContain("暂停未成功");
    vi.mocked(resourceRequest).mockResolvedValue(queue);
    await act(async () => intervals[0]());
    expect(text(rendered!.root)).toContain("暂停未成功");
  });

  it("读取失败不伪装成空队列", async () => {
    vi.mocked(resourceRequest).mockRejectedValue(new Error("队列读取失败"));
    await act(async () => { rendered = create(<GlobalQueue />); });
    expect(text(rendered!.root)).toContain("队列暂不可用");
    expect(text(rendered!.root)).not.toContain("暂无运行或排队作业");
    expect(rendered!.root.findAllByProps({ role: "alert" })).toHaveLength(1);
  });
});
