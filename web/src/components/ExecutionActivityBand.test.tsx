import { renderToStaticMarkup } from "react-dom/server";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { JobSummary, Task } from "../types";
import { ExecutionActivityBand } from "./ExecutionActivityBand";

function task(id: string, status: string | null = "running", stage = "executing"): Task {
  return { id, key: id, title: `任务 ${id}`, status, stage } as Task;
}

function job(id: string, taskId: string | null, status = "running"): JobSummary {
  return { id, task_id: taskId, role: "analysis", agent_name: "规划 Agent", status } as JobSummary;
}

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("运行轨道", () => {
  it("任务去重计数，作业单独计数，排队与暂停任务保持静态", () => {
    const tasks = [task("A"), task("B", "running", "verifying"), task("C", "queued"), task("D", "paused"), task("E", "running", "completed")];
    const markup = renderToStaticMarkup(<ExecutionActivityBand tasks={tasks} jobs={[job("a1", "A"), job("a2", "A"), job("b1", "B"), job("q1", "C", "queued")]} />);
    expect(markup).toContain("2 个任务并行");
    expect(markup).toContain("3 个运行作业");
    expect((markup.match(/class="execution-lane"/g) ?? []).length).toBe(2);
    expect(markup).not.toContain("任务 C");
    expect(markup).not.toContain("任务 D");
    expect(markup).not.toContain("任务 E");
  });

  it("规划等项目级作业有独立轨道，轨道数量有上限", () => {
    const tasks = Array.from({ length: 8 }, (_, index) => task(`T${index}`));
    const markup = renderToStaticMarkup(<ExecutionActivityBand tasks={tasks} jobs={[job("analysis", null)]} />);
    expect(markup).toContain("8 个任务并行");
    expect((markup.match(/class="execution-lane"/g) ?? []).length).toBe(6);
    expect(markup).toContain("另有 3 路活动");
    const planning = renderToStaticMarkup(<ExecutionActivityBand tasks={[]} jobs={[job("analysis", null)]} />);
    expect(planning).toContain("项目分析");
    expect(planning).toContain("规划 Agent");
  });

  it("没有运行任务与作业时不展示运行轨道", () => {
    expect(renderToStaticMarkup(<ExecutionActivityBand tasks={[task("A", null, "completed"), task("B", "queued")]} jobs={[]} />)).toBe("");
  });
});

describe("完成反馈", () => {
  let rendered: ReactTestRenderer | null = null;
  afterEach(async () => {
    if (rendered) await act(async () => rendered?.unmount());
    rendered = null;
    vi.useRealTimers();
  });

  it("只响应实际观察到的完成跳转，反馈到期后收束且轮询不重复触发", async () => {
    vi.useFakeTimers();
    await act(async () => {
      rendered = create(<ExecutionActivityBand tasks={[task("history", null, "completed"), task("A")]} jobs={[]} />);
    });
    expect(rendered!.root.findAllByProps({ "data-completed": true })).toHaveLength(0);
    const done = [task("history", null, "completed"), task("A", null, "completed")];
    await act(async () => rendered!.update(<ExecutionActivityBand tasks={done} jobs={[]} />));
    expect(rendered!.root.findAllByProps({ "data-completed": true })).toHaveLength(1);
    await act(async () => { vi.advanceTimersByTime(1600); });
    expect(rendered!.toJSON()).toBeNull();
    await act(async () => rendered!.update(<ExecutionActivityBand tasks={[...done]} jobs={[]} />));
    expect(rendered!.toJSON()).toBeNull();
  });

  it("完成反馈保持任务列表中的轨道顺序", async () => {
    await act(async () => {
      rendered = create(<ExecutionActivityBand tasks={[task("A"), task("B"), task("C")]} jobs={[]} />);
    });
    await act(async () => rendered!.update(<ExecutionActivityBand tasks={[task("A"), task("B", null, "completed"), task("C")]} jobs={[]} />));
    const lanes = rendered!.root.findAllByProps({ className: "execution-lane" });
    expect(lanes.map(lane => lane.props.title)).toEqual([
      "A · 任务 A · 执行",
      "B · 任务 B · 已完成",
      "C · 任务 C · 执行",
    ]);
    expect(lanes[1].props["data-completed"]).toBe(true);
  });

  it("卸载时释放完成反馈定时器", async () => {
    vi.useFakeTimers();
    await act(async () => { rendered = create(<ExecutionActivityBand tasks={[task("A")]} jobs={[]} />); });
    await act(async () => rendered!.update(<ExecutionActivityBand tasks={[task("A", null, "completed")]} jobs={[]} />));
    expect(vi.getTimerCount()).toBe(1);
    await act(async () => rendered!.unmount());
    rendered = null;
    expect(vi.getTimerCount()).toBe(0);
  });
});
