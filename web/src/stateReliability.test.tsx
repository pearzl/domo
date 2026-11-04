import { renderToStaticMarkup } from "react-dom/server";
import {
  act,
  create,
  type ReactTestInstance,
  type ReactTestRenderer,
} from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "./api";
import type {
  JobDetail,
  JobSummary,
  Project,
  ProjectDetail,
  UsageReport,
} from "./types";
import { JobDetailView } from "./components/JobDetailView";
import { ProjectReportPanel } from "./components/ProjectReportPanel";
import { AgentsPage } from "./pages/AgentsPage";
import { ProjectsPage } from "./pages/ProjectsPage";
import { UsagePage } from "./pages/UsagePage";
import {
  useProjectPageController,
  type ProjectPageController,
} from "./presentation/projectPageController";

vi.mock("./api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api")>();
  return {
    ...actual,
    api: {
      ...actual.api,
      getJob: vi.fn(),
      getProject: vi.fn(),
      listProjects: vi.fn(),
      createProject: vi.fn(),
      listAgents: vi.fn(),
      getDefaultTeam: vi.fn(),
      getUsage: vi.fn(),
    },
  };
});

vi.mock("./components/JobActivity", () => ({
  JobActivity: () => <div>作业活动</div>,
}));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const intervals: Array<() => void> = [];
let rendered: ReactTestRenderer | null = null;

beforeEach(() => {
  intervals.length = 0;
  vi.stubGlobal("window", {
    location: { pathname: "/projects", search: "" },
    history: {
      state: null,
      pushState: vi.fn(),
      replaceState: vi.fn(),
    },
    localStorage: null,
    setInterval: vi.fn((callback: () => void) => {
      intervals.push(callback);
      return intervals.length;
    }),
    clearInterval: vi.fn(),
    setTimeout: vi.fn(() => 1),
    clearTimeout: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  });
});

afterEach(async () => {
  if (rendered) {
    const current = rendered;
    rendered = null;
    await act(async () => current.unmount());
  }
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

function text(node: ReactTestInstance): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : text(child)))
    .join("");
}

function pageText(renderer: ReactTestRenderer): string {
  return text(renderer.root);
}

function button(renderer: ReactTestRenderer, label: string): ReactTestInstance {
  const match = renderer.root
    .findAllByType("button")
    .find((item) => text(item) === label);
  if (!match) throw new Error(`找不到按钮：${label}`);
  return match;
}

function jobSummary(overrides: Partial<JobSummary> = {}): JobSummary {
  return {
    execution_job_id: null,
    id: "job-1",
    project_id: "p1",
    task_id: null,
    role: "analysis",
    agent_id: null,
    agent_name: "Agent 一",
    status: "running",
    attempt: 1,
    error: null,
    exit_code: null,
    failure_kind: null,
    failure_summary: null,
    input_tokens: null,
    output_tokens: null,
    cache_read_tokens: null,
    cache_write_tokens: null,
    reasoning_tokens: null,
    cost: null,
    duration_ms: null,
    usage_known: false,
    started_at: "2026-01-10T10:00:00Z",
    finished_at: null,
    created_at: "2026-01-10T10:00:00Z",
    ...overrides,
  };
}

function jobDetail(
  summary: JobSummary,
  result: string,
): JobDetail {
  return {
    ...summary,
    prompt: "提示词",
    result,
    stdout: null,
    stderr: null,
    pid: null,
  };
}

function project(overrides: Partial<Project> = {}): Project {
  return {
    id: "p1",
    name: "示例项目",
    goal: "目标",
    repo_path: "/repo",
    base_branch: "main",
    branch: "domo/p1",
    workspace: "/workspace",
    stage: "summarizing",
    status: "running",
    blocked_reason: null,
    report: "报告内容",
    report_feedback: null,
    replan_reason: null,
    replan_source: null,
    replan_requested_at: null,
    confirm_mode: "manual",
    confirmed_at: null,
    completion_mode: "tasks",
    merged_at: null,
    merged_commit: null,
    created_at: "2026-01-10T10:00:00Z",
    updated_at: "2026-01-10T10:00:00Z",
    ...overrides,
  };
}

const EMPTY_USAGE: UsageReport = {
  group_by: "project",
  filters: { project_id: null, agent_id: null, role: null, from: null, to: null },
  totals: {
    jobs: 0,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    reasoning_tokens: 0,
    cost: 0,
    duration_ms: 0,
    known_jobs: 0,
    unknown_jobs: 0,
    known_metrics: { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0, cost: 0 },
  },
  groups: [],
};

describe("归档只读", () => {
  it("归档报告保留内容但不渲染提交与添加任务入口", () => {
    const markup = renderToStaticMarkup(
      <ProjectReportPanel
        project={project({ status: "archived" })}
        reviewMessages={[]}
        feedback="意见"
        feedbackDraftKey="feedback"
        onFeedbackChange={() => {}}
        busy={false}
        archived
        onSubmitReport={() => {}}
        onRequestReplan={() => {}}
        onAddTask={() => {}}
      />,
    );
    expect(markup).toContain("报告内容");
    for (const label of ["通过并完成", "提交意见", "重新撰写", "要求重新规划", "增加新任务"]) {
      expect(markup).not.toContain(label);
    }
  });

  it("作业详情在只读模式下不渲染重试入口", () => {
    const markup = renderToStaticMarkup(
      <JobDetailView
        job={jobSummary({ status: "failed" })}
        retryable
        readOnly
        onRetry={() => {}}
      />,
    );
    expect(markup).not.toContain("让这个失败作业重新排队");
    expect(markup).not.toContain(">重试</button>");
  });
});

describe("作业详情状态", () => {
  it("运行到完成后重新请求并展示完成结果", async () => {
    const running = jobSummary();
    const finished = jobSummary({
      status: "succeeded",
      finished_at: "2026-01-10T10:01:00Z",
      exit_code: 0,
    });
    vi.mocked(api.getJob)
      .mockResolvedValueOnce(jobDetail(running, "运行中的中间结果"))
      .mockResolvedValueOnce(jobDetail(finished, "最终完成结果"));

    await act(async () => {
      rendered = create(<JobDetailView job={running} />);
    });
    await settle();
    expect(pageText(rendered!)).toContain("运行中的中间结果");

    await act(async () => {
      rendered!.update(<JobDetailView job={finished} />);
    });
    await settle();

    expect(api.getJob).toHaveBeenCalledTimes(2);
    expect(pageText(rendered!)).toContain("最终完成结果");
    expect(pageText(rendered!)).not.toContain("运行中的中间结果");
  });
});

describe("后台读取与操作错误", () => {
  it("项目详情读取串行，等待中的多个刷新合并为一次后续读取", async () => {
    const detail = { project: project({ stage: "executing" }), tasks: [], jobs: [] } as unknown as ProjectDetail;
    let resolveFirst!: (value: ProjectDetail) => void;
    vi.mocked(api.getProject)
      .mockImplementationOnce(() => new Promise(resolve => { resolveFirst = resolve; }))
      .mockResolvedValue(detail);
    const controller: { current?: ProjectPageController } = {};
    function Harness() {
      controller.current = useProjectPageController({ id: "p1" });
      return null;
    }
    await act(async () => { rendered = create(<Harness />); });
    await act(async () => {
      void controller.current!.refresh(true);
      void controller.current!.refresh(true);
      void controller.current!.refresh(true);
    });
    expect(api.getProject).toHaveBeenCalledTimes(1);
    await act(async () => { resolveFirst(detail); });
    await settle();
    expect(api.getProject).toHaveBeenCalledTimes(2);
    expect(controller.current!.detail).toBe(detail);
  });

  it("项目详情心跳成功后仍保留操作错误", async () => {
    const detail = {
      project: project({ stage: "executing" }),
      tasks: [],
      jobs: [],
    } as unknown as ProjectDetail;
    vi.mocked(api.getProject).mockResolvedValue(detail);
    const controller: { current?: ProjectPageController } = {};
    function Harness() {
      controller.current = useProjectPageController({ id: "p1" });
      return null;
    }

    await act(async () => {
      rendered = create(<Harness />);
    });
    await settle();
    await act(async () => {
      await controller.current!.run(() => Promise.reject(new Error("操作失败")));
    });
    expect(controller.current!.error).toBe("操作失败");

    await act(async () => {
      intervals[0]();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(controller.current!.error).toBe("操作失败");
  });

  it("项目列表轮询成功后仍保留创建错误", async () => {
    vi.mocked(api.listProjects).mockResolvedValue([]);
    vi.mocked(api.createProject).mockRejectedValue(new Error("创建失败"));
    await act(async () => {
      rendered = create(<ProjectsPage onOpen={() => {}} />);
    });
    await settle();
    await act(async () => button(rendered!, "新建项目").props.onClick());
    await act(async () => {
      await rendered!.root.findByType("form").props.onSubmit({
        preventDefault: () => {},
      });
    });
    expect(pageText(rendered!)).toContain("创建失败");

    await act(async () => {
      intervals[0]();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(pageText(rendered!)).toContain("创建失败");
  });
});

describe("初次请求失败与重试", () => {
  it("项目列表失败不显示空状态，重试后显示真实空状态", async () => {
    vi.mocked(api.listProjects)
      .mockRejectedValueOnce(new Error("项目读取失败"))
      .mockResolvedValueOnce([]);
    await act(async () => {
      rendered = create(<ProjectsPage onOpen={() => {}} />);
    });
    await settle();
    expect(pageText(rendered!)).toContain("项目读取失败");
    expect(pageText(rendered!)).not.toContain("还没有项目");

    await act(async () => button(rendered!, "重试").props.onClick());
    await settle();
    expect(api.listProjects).toHaveBeenCalledTimes(2);
    expect(pageText(rendered!)).toContain("还没有项目");
  });

  it("Agent 列表失败不显示空状态，重试后显示真实空状态", async () => {
    vi.mocked(api.listAgents)
      .mockRejectedValueOnce(new Error("Agent 读取失败"))
      .mockResolvedValueOnce([]);
    vi.mocked(api.getDefaultTeam)
      .mockRejectedValueOnce(new Error("Agent 读取失败"))
      .mockResolvedValueOnce({ project_id: "default", roles: [] });
    await act(async () => {
      rendered = create(<AgentsPage />);
    });
    await settle();
    expect(pageText(rendered!)).toContain("Agent 读取失败");
    expect(pageText(rendered!)).not.toContain("还没有 Agent");

    await act(async () => button(rendered!, "重试").props.onClick());
    await settle();
    expect(api.listAgents).toHaveBeenCalledTimes(2);
    expect(pageText(rendered!)).toContain("还没有 Agent");
  });

  it("用量请求失败不显示空数据文案，重试后显示真实空状态", async () => {
    vi.mocked(api.listProjects).mockResolvedValue([]);
    vi.mocked(api.listAgents).mockResolvedValue([]);
    vi.mocked(api.getUsage)
      .mockRejectedValueOnce(new Error("用量读取失败"))
      .mockResolvedValueOnce(EMPTY_USAGE);
    await act(async () => {
      rendered = create(<UsagePage />);
    });
    await settle();
    expect(pageText(rendered!)).toContain("用量读取失败");
    expect(pageText(rendered!)).not.toContain("暂时没有可展示的用量数据");
    expect(pageText(rendered!)).not.toContain("当前筛选范围内没有作业记录");

    await act(async () => button(rendered!, "重试").props.onClick());
    await settle();
    expect(api.getUsage).toHaveBeenCalledTimes(2);
    expect(pageText(rendered!)).toContain("当前筛选范围内没有作业记录");
  });
});
