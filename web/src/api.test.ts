import { afterEach, describe, expect, it, vi } from "vitest";
import { subscribeEvents } from "./api";

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  onopen?: () => void;
  onerror?: () => void;
  onmessage?: (event: { data: string }) => void;
  close = vi.fn();
  constructor(readonly url: string) { FakeEventSource.instances.push(this); }
}

afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); FakeEventSource.instances = []; });

describe("共享事件订阅", () => {
  it("共享连接、过滤项目、合并突发事件，最后退订时关闭连接", () => {
    vi.useFakeTimers();
    vi.stubGlobal("EventSource", FakeEventSource);
    const first = vi.fn();
    const second = vi.fn();
    const global = vi.fn();
    const status = vi.fn();
    const stopFirst = subscribeEvents(first, status, "p1");
    const stopSecond = subscribeEvents(second, undefined, "p2");
    const stopGlobal = subscribeEvents(global);
    try {
      expect(FakeEventSource.instances).toHaveLength(1);
      const source = FakeEventSource.instances[0];
      source.onopen?.();
      expect(status).toHaveBeenLastCalledWith(true);
      for (let n = 0; n < 5; n++) source.onmessage?.({ data: JSON.stringify({ project_id: "p1" }) });
      vi.advanceTimersByTime(100);
      expect(first).toHaveBeenCalledTimes(1);
      expect(second).not.toHaveBeenCalled();
      expect(global).toHaveBeenCalledTimes(1);
      source.onerror?.();
      expect(status).toHaveBeenLastCalledWith(false);
      source.onmessage?.({ data: JSON.stringify({ project_id: null }) });
      stopSecond();
      vi.advanceTimersByTime(100);
      expect(second).not.toHaveBeenCalled();
      expect(source.close).not.toHaveBeenCalled();
    } finally {
      stopFirst(); stopSecond(); stopGlobal();
    }
    expect(FakeEventSource.instances[0].close).toHaveBeenCalled();
  });
});
