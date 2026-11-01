import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { JobSummary, Project, ProjectDetail, Task } from "./types";
import { ProjectHeader } from "./components/ProjectHeader";

function executingBlockedProject(): Project {
  return {
    id: "p1",
    name: "示例项目",
    goal: "交付示例功能",
    repo_path: "/home/pearzl/repos/example",
    base_branch: "main",
    branch: "domo/p1",
    workspace: "/home/pearzl/.domo/workspace/p1",
    stage: "executing",
    status: "blocked",
    blocked_reason: "验收连续失败，需要人工确认。",
    report: null,
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
  };
}

function task(index: number, completed: boolean): Task {
  return {
    id: `t${index}`,
    project_id: "p1",
    seq: index,
    queue_order: index,
    queue_position: index,
    key: `T${index}`,
    title: `任务 ${index}`,
    description: "",
    acceptance: [],
    deps: [],
    parent_id: null,
    milestone: false,
    human_gate: null,
    forced_agent_id: null,
    forced_agent_role: null,
    stage: completed ? "completed" : "executing",
    status: completed ? null : "running",
    attempt: 0,
    created_at: "2026-01-10T10:00:00Z",
    updated_at: "2026-01-10T10:00:00Z",
  } as unknown as Task;
}

function runningJob(): JobSummary {
  return {
    id: "job-1",
    project_id: "p1",
    task_id: null,
    role: "analysis",
    agent_id: null,
    agent_name: "Agent",
    attempt: 1,
    status: "running",
    exit_code: null,
    error: null,
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
    started_at: null,
    finished_at: null,
    created_at: "2026-01-10T10:00:00Z",
  };
}

function executingBlockedDetail(): ProjectDetail {
  return {
    project: executingBlockedProject(),
    tasks: [
      task(1, true),
      task(2, true),
      task(3, false),
      task(4, false),
      task(5, false),
    ],
    jobs: [runningJob()],
    messages: [],
    events: [],
    agents: [],
    proposal: null,
    merged_into_base: false,
  } as unknown as ProjectDetail;
}

function renderHeader(detail: ProjectDetail) {
  return renderToStaticMarkup(
    <ProjectHeader
      detail={detail}
      canReplan
      busy={false}
      merging={false}
      mergeNotice={null}
      onResume={() => {}}
      onPause={() => {}}
      onForcePause={() => {}}
      onToggleReplan={() => {}}
      onArchive={() => {}}
      onMerge={() => {}}
    />,
  );
}

describe("ProjectHeader 状态条与项目源信息", () => {
  it("executing 且 blocked 时同时展示阶段与状态徽标", () => {
    const markup = renderHeader(executingBlockedDetail());
    expect(markup).toContain('<header class="project-header panel">');
    expect(markup).toContain("执行中");
    expect(markup).toContain("阻塞中");
    expect(markup).toContain("任务进度");
    expect(markup).toContain("2/5");
    expect(markup).toContain("40%");
    expect(markup).toContain("运行作业");
    expect(markup).toContain("1 个");
  });

  it("项目源信息默认折叠，包含仓库、交付分支与工作区", () => {
    const markup = renderHeader(executingBlockedDetail());
    expect(markup).toContain('<details class="project-source">');
    expect(markup).not.toContain('<details class="project-source" open');
    expect(markup).toContain("项目源信息");
    expect(markup).toContain("/home/pearzl/repos/example");
    expect(markup).toContain("main → domo/p1");
    expect(markup).toContain("/home/pearzl/.domo/workspace/p1");
    expect(markup).toContain("复制仓库");
  });

  it("保留原有操作按钮与项目源信息，不出现项目设置区块", () => {
    const markup = renderHeader(executingBlockedDetail());
    for (const label of ["暂停", "强制暂停", "重新规划", "归档"]) {
      expect(markup).toContain(`>${label}</button>`);
    }
    expect(markup).not.toContain("项目设置");
    expect(markup).not.toContain("项目元信息");
  });

  it("阶段与状态徽标文案来自 STAGE_LABELS 与 STATUS_LABELS，并在变化时淡入", async () => {
    const { STAGE_LABELS, STATUS_LABELS } = await import("./ui");
    const blocked = renderHeader(executingBlockedDetail());
    expect(blocked).toContain(STAGE_LABELS.executing);
    expect(blocked).toContain(STATUS_LABELS.blocked);
    expect(blocked).toContain("project-status-fade");
    const running = executingBlockedDetail();
    running.project.status = "running";
    const markup = renderHeader(running);
    expect(markup).toContain(STATUS_LABELS.running);
    expect(markup).not.toContain(STATUS_LABELS.blocked);
  });
});
