import { describe, expect, it } from "vitest";
import type { JobActivity } from "./types";
import { createActivityPollLoop } from "./useJobActivity";

/// 作业活动轮询的调度与请求都由测试直接驱动，无需真实定时器或 DOM。

function activity(text: string): JobActivity {
  return {
    format: "text",
    bytes: 16,
    items: [
      {
        kind: "message",
        text,
        tool: null,
        target: null,
        status: null,
        error: null,
      },
    ],
    summary: {
      messages: 1,
      reasoning: 0,
      tool_calls: 0,
      tool_failures: 0,
      steps: 0,
      truncated: false,
    },
  };
}

interface Scheduled {
  handle: number;
  run: () => void;
  delay: number;
}

function createHarness() {
  const scheduled: Scheduled[] = [];
  const cancelled: number[] = [];
  const activities: JobActivity[] = [];
  const errors: unknown[] = [];
  let loads = 0;
  let settled = 0;
  let nextHandle = 0;

  const loop = createActivityPollLoop({
    load: () => {
      loads += 1;
      return Promise.resolve(activity(`第 ${loads} 次`));
    },
    onActivity: (next) => activities.push(next),
    onError: (error) => errors.push(error),
    onSettled: () => {
      settled += 1;
    },
    nextDelay: () => 1500,
    schedule: (run, delay) => {
      nextHandle += 1;
      scheduled.push({ handle: nextHandle, run, delay });
      return nextHandle;
    },
    cancel: (handle) => {
      cancelled.push(handle);
      const index = scheduled.findIndex((entry) => entry.handle === handle);
      if (index >= 0) scheduled.splice(index, 1);
    },
  });

  return {
    loop,
    scheduled,
    cancelled,
    activities,
    errors,
    get loads() {
      return loads;
    },
    get settled() {
      return settled;
    },
  };
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("useJobActivity 轮询生命周期", () => {
  it("排队中的作业只读一次，running 变为 true 后恢复续拍", async () => {
    const harness = createHarness();

    harness.loop.start(false);
    await flush();
    expect(harness.loads).toBe(1);
    expect(harness.scheduled).toHaveLength(0);

    harness.loop.setRunning(true);
    await flush();
    expect(harness.loads).toBe(2);
    expect(harness.scheduled).toHaveLength(1);

    harness.scheduled[0].run();
    await flush();
    expect(harness.loads).toBe(3);
    expect(harness.scheduled).toHaveLength(2);
    expect(harness.activities.map((entry) => entry.items[0].text)).toEqual([
      "第 1 次",
      "第 2 次",
      "第 3 次",
    ]);
  });

  it("running 转为 false 时取消挂起定时器并补一次收尾拉取", async () => {
    const harness = createHarness();

    harness.loop.start(true);
    await flush();
    expect(harness.loads).toBe(1);
    expect(harness.scheduled).toHaveLength(1);

    harness.loop.setRunning(false);
    expect(harness.cancelled).toEqual([1]);
    await flush();

    expect(harness.loads).toBe(2);
    expect(harness.scheduled).toHaveLength(0);
    expect(harness.activities).toHaveLength(2);
    expect(harness.settled).toBe(2);
  });

  it("收尾期间重新运行时恢复续拍而不是停摆", async () => {
    const harness = createHarness();

    harness.loop.start(true);
    await flush();
    harness.loop.setRunning(false);
    harness.loop.setRunning(true);
    await flush();

    expect(harness.loads).toBe(2);
    expect(harness.scheduled).toHaveLength(1);

    harness.scheduled[0].run();
    await flush();
    expect(harness.loads).toBe(3);
  });

  it("停止后不再发起请求或排队", async () => {
    const harness = createHarness();

    harness.loop.start(true);
    harness.loop.stop();
    await flush();

    expect(harness.cancelled).toEqual([]);
    expect(harness.scheduled).toHaveLength(0);
    expect(harness.activities).toHaveLength(0);
  });

  it("读取失败后仍按运行状态继续续拍", async () => {
    let loads = 0;
    const errors: unknown[] = [];
    const scheduled: Scheduled[] = [];
    const loop = createActivityPollLoop({
      load: () => {
        loads += 1;
        return Promise.reject(new Error("网络中断"));
      },
      onActivity: () => undefined,
      onError: (error) => errors.push(error),
      onSettled: () => undefined,
      nextDelay: () => 1500,
      schedule: (run, delay) => {
        scheduled.push({ handle: scheduled.length + 1, run, delay });
        return scheduled.length;
      },
      cancel: () => undefined,
    });

    loop.start(true);
    await flush();
    expect(errors).toHaveLength(1);
    expect(scheduled).toHaveLength(1);

    scheduled[0].run();
    await flush();
    expect(loads).toBe(2);
    expect(scheduled).toHaveLength(2);
  });
});
