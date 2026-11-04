import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Task } from "../types";
import { TaskDag } from "./TaskDag";
import { layoutTaskDag } from "./dagLayout";

function task(key: string, deps: string[] = []): Task {
  return {
    id: `id-${key}`,
    project_id: "p1",
    seq: 1,
    queue_order: 1,
    queue_position: null,
    key,
    title: `任务 ${key}`,
    description: "",
    acceptance: [],
    deps,
    parent_id: null,
    milestone: false,
    human_gate: null,
    forced_agent_id: null,
    forced_agent_role: null,
    stage: "pending",
    status: null,
    agent_id: null,
    last_agent_id: null,
    worktree: null,
    branch: null,
    artifacts_released_at: null,
    attempts: 0,
    rejections: 0,
    last_error: null,
    last_failure_kind: null,
    waiting_reason: null,
    result_summary: null,
    verifier_issues: [],
    guidance: "",
    created_at: "2026-01-10T10:00:00Z",
    updated_at: "2026-01-10T10:00:00Z",
  };
}

const TASKS: Task[] = [task("A"), task("B", ["A"]), task("C", ["A", "B"]), task("D")];

function render(selectedTaskId: string | null) {
  return renderToStaticMarkup(
    <TaskDag tasks={TASKS} selectedTaskId={selectedTaskId} onSelect={() => {}} />,
  );
}

/// 把标记里每个 <path> 的 class 抽出来，按出现顺序与布局边一一对应。
function edgeClasses(markup: string): string[] {
  return [...markup.matchAll(/<path\b[^>]*class="([^"]*)"/g)].map(
    (match) => match[1],
  );
}

describe("TaskDag 依赖边高亮", () => {
  it("选中节点时相关边带高亮类名，其余边带弱化类名", () => {
    const layout = layoutTaskDag(TASKS);
    const classes = edgeClasses(render("id-B"));
    expect(classes.length).toBe(layout.edges.length);

    const related = layout.edges.filter(
      (edge) => edge.from === "id-B" || edge.to === "id-B",
    );
    expect(related.length).toBeGreaterThan(0);
    const unrelated = layout.edges.filter(
      (edge) => edge.from !== "id-B" && edge.to !== "id-B",
    );
    expect(unrelated.length).toBeGreaterThan(0);

    layout.edges.forEach((edge, index) => {
      const isRelated = edge.from === "id-B" || edge.to === "id-B";
      const className = classes[index];
      if (isRelated) {
        expect(className).toContain("dag-edge-highlight");
        expect(className).not.toContain("dag-edge-dimmed");
      } else {
        expect(className).toContain("dag-edge-dimmed");
        expect(className).not.toContain("dag-edge-highlight");
      }
    });
  });

  it("没有选中节点时不渲染高亮或弱化类名", () => {
    const markup = render(null);
    expect(markup).not.toContain("dag-edge-highlight");
    expect(markup).not.toContain("dag-edge-dimmed");
  });

  it("渲染层级列头", () => {
    const markup = render(null);
    expect((markup.match(/class="dag-level-header"/g) ?? []).length).toBe(3);
    expect(markup).toContain("第 1 层");
    expect(markup).toContain("第 3 层");
  });
});

describe("TaskDag 缩放与适应窗口", () => {
  it("在滚动容器内渲染放大、缩小与适应窗口按钮", () => {
    const markup = render(null);
    const scrollStart = markup.indexOf('class="dag-scroll"');
    expect(scrollStart).toBeGreaterThan(0);
    const toolbarStart = markup.indexOf('role="toolbar"');
    expect(toolbarStart).toBeGreaterThan(scrollStart);
    expect(markup).toContain('aria-label="放大"');
    expect(markup).toContain('aria-label="缩小"');
    expect(markup).toContain('aria-label="适应窗口"');
    expect(markup).toContain("适应窗口");
    expect(markup).toContain('class="dag-zoom-level"');
    expect(markup).toContain("100%");
  });

  it("跨层依赖用折线路径绘制，相邻层仍用平滑曲线", () => {
    const layout = layoutTaskDag(TASKS);
    const cross = layout.edges.find((edge) => edge.crossLevel);
    const smooth = layout.edges.find((edge) => edge.routing === "curve");
    expect(cross).toBeDefined();
    expect(smooth).toBeDefined();
    expect(cross?.path.startsWith("M ")).toBe(true);
    expect(cross?.path).toContain(" L ");
    expect(cross?.path).not.toContain(" C ");
    expect(smooth?.path).toContain(" C ");

    const markup = render(null);
    expect(markup).toContain(cross?.path);
    expect(markup).toContain(smooth?.path);
  });
});
