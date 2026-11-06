import { renderToStaticMarkup } from "react-dom/server";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
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

  it("只有前置完成且后继运行的依赖传递流光，暂停与排队保持静态", () => {
    const tasks = [
      { ...task("A"), stage: "completed" },
      { ...task("B", ["A"]), stage: "executing", status: "running" },
      { ...task("C", ["A"]), stage: "verifying", status: "queued" },
      { ...task("D", ["A"]), stage: "executing", status: "paused" },
    ];
    const markup = renderToStaticMarkup(<TaskDag tasks={tasks} selectedTaskId={null} onSelect={() => {}} />);
    expect((markup.match(/class="dag-edge-flow/g) ?? []).length).toBe(1);
    expect((markup.match(/data-running="true"/g) ?? []).length).toBe(1);
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

  it("跨层与相邻层依赖均沿直角通道绘制", () => {
    const layout = layoutTaskDag(TASKS);
    const cross = layout.edges.find((edge) => edge.crossLevel);
    expect(cross).toBeDefined();
    const markup = render(null);
    for (const edge of layout.edges) {
      expect(edge.routing).toBe("channel");
      expect(edge.path).toContain(" L ");
      expect(edge.path).not.toContain(" C ");
      expect(markup).toContain(edge.path);
      edge.points.slice(1).forEach((point, index) => {
        const previous = edge.points[index];
        expect(point[0] === previous[0] || point[1] === previous[1]).toBe(true);
      });
    }
  });
});

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("TaskDag 视口手势", () => {
  let rendered: ReactTestRenderer | null = null;
  const listeners = new Map<string, EventListener>();
  const viewport = {
    clientWidth: 900, clientHeight: 500, clientLeft: 1, clientTop: 1,
    getBoundingClientRect: () => ({ left: 20, top: 30 }),
    addEventListener: vi.fn((name: string, callback: EventListener) => listeners.set(name, callback)),
    removeEventListener: vi.fn((name: string) => listeners.delete(name)),
    setPointerCapture: vi.fn(), hasPointerCapture: vi.fn(() => true), releasePointerCapture: vi.fn(), focus: vi.fn(),
  };

  afterEach(async () => {
    if (rendered) await act(async () => rendered?.unmount());
    rendered = null;
    listeners.clear();
    vi.clearAllMocks();
  });

  async function mount(tasks = TASKS) {
    const onSelect = vi.fn();
    await act(async () => {
      rendered = create(<TaskDag tasks={tasks} selectedTaskId={null} onSelect={onSelect} />, {
        createNodeMock: element => (element.props as { className?: string }).className === "dag-scroll" ? viewport : null,
      });
    });
    return onSelect;
  }

  function region() { return rendered!.root.findByProps({ className: "dag-scroll" }); }
  function transform() {
    const value = rendered!.root.findByProps({ className: "dag-canvas" }).props.style.transform as string;
    const values = value.match(/-?[\d.]+/g)!.map(Number);
    return { x: values[0], y: values[1], zoom: values[2] };
  }

  it("滚轮以指针所在的图坐标缩放，并阻止页面滚动", async () => {
    await mount();
    expect(viewport.addEventListener).toHaveBeenCalledWith("wheel", expect.any(Function), { passive: false });
    const preventDefault = vi.fn();
    await act(async () => listeners.get("wheel")!({ deltaY: -100, deltaMode: 0, clientX: 421, clientY: 275, preventDefault } as unknown as Event));
    const view = transform();
    expect(view.zoom).toBeGreaterThan(1);
    expect((400 - view.x) / view.zoom).toBeCloseTo(400);
    expect((200 - view.y) / view.zoom).toBeCloseTo(200);
    expect(preventDefault).toHaveBeenCalledOnce();
  });

  it("节点拖动能平移画布，拖动后的点击不会打开任务，普通点击仍可打开", async () => {
    const onSelect = await mount();
    const pointer = { button: 0, isPrimary: true, pointerId: 1, clientX: 100, clientY: 100, currentTarget: viewport, target: { closest: (selector: string) => selector === ".dag-node" ? {} : null } };
    await act(async () => region().props.onPointerDown(pointer));
    await act(async () => region().props.onPointerMove({ ...pointer, clientX: 104, clientY: 100 }));
    expect(transform().x).toBe(0);
    await act(async () => region().props.onPointerMove({ ...pointer, clientX: 160, clientY: 135 }));
    expect(transform()).toEqual({ x: 60, y: 35, zoom: 1 });
    expect(viewport.setPointerCapture).toHaveBeenCalledWith(1);
    await act(async () => region().props.onPointerUp(pointer));
    const event = { preventDefault: vi.fn(), stopPropagation: vi.fn() };
    region().props.onClickCapture(event);
    expect(event.stopPropagation).toHaveBeenCalledOnce();
    expect(onSelect).not.toHaveBeenCalled();
    await act(async () => region().props.onPointerDown(pointer));
    await act(async () => region().props.onPointerUp(pointer));
    rendered!.root.findAllByProps({ className: "dag-node" })[0].props.onClick();
    expect(onSelect).toHaveBeenCalledWith("id-A");
  });

  it("适应窗口在平移后将图居中，键盘可以继续平移", async () => {
    await mount();
    await act(async () => rendered!.root.findByProps({ "aria-label": "适应窗口" }).props.onClick());
    const view = transform();
    const layout = layoutTaskDag(TASKS);
    expect(view.x + layout.width * view.zoom / 2).toBeCloseTo(450);
    expect(view.y + layout.height * view.zoom / 2).toBeCloseTo((500 - 44) / 2);
    await act(async () => region().props.onKeyDown({ key: "ArrowRight", target: viewport, currentTarget: viewport, preventDefault: vi.fn() }));
    expect(transform().x).toBeCloseTo(view.x - 40);
  });

  it("空图有任务后绑定滚轮监听，卸载后释放监听", async () => {
    await mount([]);
    expect(listeners.has("wheel")).toBe(false);
    await act(async () => rendered!.update(<TaskDag tasks={TASKS} selectedTaskId={null} onSelect={() => {}} />));
    expect(listeners.has("wheel")).toBe(true);
    await act(async () => rendered!.unmount());
    rendered = null;
    expect(listeners.has("wheel")).toBe(false);
  });
});
