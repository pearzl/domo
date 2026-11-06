import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import type { ArbitrationContext, Task } from "../types";
import { ArbitrationPanel } from "./ArbitrationPanel";
import { UserActionNotice } from "./UserActionNotice";

vi.mock("../api", async (original) => {
  const actual = await original<typeof import("../api")>();
  return { ...actual, api: { ...actual.api, getTaskArbitration: vi.fn() } };
});

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let rendered: ReactTestRenderer | null = null;
afterEach(async () => {
  await act(async () => rendered?.unmount());
  rendered = null;
  vi.restoreAllMocks();
});

async function renderDecision(state: string, actions: string[]) {
  vi.mocked(api.getTaskArbitration).mockResolvedValue({
    project: { id: "project", name: "项目", stage: "executing", status: "running", blocked_reason: null },
    role: "executor", task: { id: "child", key: "T1.1", title: "子任务", stage: "executing", status: "blocked",
      attempts: 0, rejections: 0, last_error: null, failure_summary: null, milestone: false,
      human_gate: null, forced_agent_id: null, forced_agent_role: null },
    agents: [], jobs: [], actions,
    replan_request: { state, reviewer_task_id: "parent", reason: "输入不一致" },
  } satisfies ArbitrationContext);
  await act(async () => {
    rendered = create(<ArbitrationPanel projectId="project"
      task={{ id: "child", stage: "executing", status: "blocked" } as Task}
      onRefresh={async () => {}} disabled={false} onLegacyRetry={() => {}} />);
  });
  return rendered!;
}

describe("重规划决策呈现", () => {
  it("等待条件先展示独立的用户操作说明，不要求先读原始错误", async () => {
    const action = { summary: "请连接测试设备", why: "无法完成设备验证", owner: "设备提供方",
      steps: ["连接设备并确认可访问"], after: "记录指导并继续，AI 将核实并补验" };
    vi.mocked(api.getTaskArbitration).mockResolvedValue({
      project: { id: "project", name: "项目", stage: "executing", status: "blocked", blocked_reason: null },
      role: "executor", task: { id: "task", key: "T1", title: "验证", stage: "executing", status: "blocked",
        attempts: 0, rejections: 0, last_error: "长篇完整技术记录", failure_summary: null, milestone: false,
        human_gate: null, forced_agent_id: null, forced_agent_role: null, user_action: action },
      agents: [], jobs: [], actions: ["retry", "guide"], agent_guidance: "重跑完整矩阵", decision_reason: "真实设备不可达",
      replan_request: { state: "waiting", reviewer_task_id: "parent", reason: "原始请求" },
    });
    await act(async () => {
      rendered = create(<ArbitrationPanel projectId="project" task={{ id: "task", status: "blocked", stage: "executing" } as Task}
        onRefresh={async () => {}} disabled={false} onLegacyRetry={() => {}} />);
    });
    expect(rendered!.root.findByType("h3").children).toEqual(["需要补充条件"]);
    const notice = rendered!.root.findByType(UserActionNotice);
    expect(notice.props.action).toEqual(action);
    expect(notice.props.guidance).toBe("重跑完整矩阵");
    expect(notice.props.details).toBe("真实设备不可达");
    for (const detail of rendered!.root.findAllByType("details")) expect(detail.props.open).not.toBe(true);
  });
  it("验收等待提供重新验收入口，不把阻塞统一描述为执行失败", async () => {
    vi.mocked(api.getTaskArbitration).mockResolvedValue({
      project: { id: "project", name: "项目", stage: "executing", status: "blocked", blocked_reason: null },
      role: "verifier", task: { id: "task", key: "T1", title: "报告", stage: "verifying", status: "blocked",
        attempts: 0, rejections: 0, last_error: "用户补齐设备后重新验收", failure_summary: null, milestone: false,
        human_gate: null, forced_agent_id: null, forced_agent_role: null },
      agents: [], jobs: [], actions: ["retry", "guide"], replan_request: null,
    });
    await act(async () => {
      rendered = create(<ArbitrationPanel projectId="project"
        task={{ id: "task", stage: "verifying", status: "blocked" } as Task}
        onRefresh={async () => {}} disabled={false} onLegacyRetry={() => {}} />);
    });
    expect(rendered!.root.findByType("h3").children).toEqual(["验收条件与处置"]);
    const retry = rendered!.root.findAllByType("button").find(button => button.children.includes("条件已补齐，重新验收"));
    expect(retry?.props.disabled).toBe(false);
  });
  it("决策进行中不允许重试或人工换人绕过上级决策", async () => {
    const view = await renderDecision("running", []);
    expect(view.root.findByType("h3").children).toEqual(["上级决策"]);
    const buttons = view.root.findAllByType("button");
    for (const label of ["重新决策", "记录指导并继续"]) {
      expect(buttons.find((button) => button.children.includes(label))?.props.disabled).toBe(true);
    }
    expect(view.root.findAllByType("select")).toHaveLength(0);
  });

  it("等待条件时允许重新决策和指导，不提供换 Agent 执行入口", async () => {
    const view = await renderDecision("waiting", ["retry", "guide"]);
    const retry = view.root.findAllByType("button").find((button) => button.children.includes("重新决策"));
    expect(retry?.props.disabled).toBe(false);
    expect(view.root.findAllByType("select")).toHaveLength(0);
  });
});
