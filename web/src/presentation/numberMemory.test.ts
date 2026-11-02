import { beforeEach, describe, expect, it } from "vitest";
import {
  clearNumberMemory,
  numberMemoryKey,
  numberMemorySize,
  NUMBER_MEMORY_LIMIT,
  readNumberMemory,
  writeNumberMemory,
} from "./numberMemory";

describe("数字滚动记忆", () => {
  beforeEach(() => {
    clearNumberMemory();
  });

  it("首次读取返回 null，写入后返回最近一次的值", () => {
    const key = numberMemoryKey("project:p1", "usage:cost");
    expect(readNumberMemory(key)).toBeNull();
    writeNumberMemory(key, "0.5");
    expect(readNumberMemory(key)).toBe("0.5");
    writeNumberMemory(key, "0.6");
    expect(readNumberMemory(key)).toBe("0.6");
  });

  it("不同会话与槽位互不影响", () => {
    const a = numberMemoryKey("project:p1", "usage:cost");
    const b = numberMemoryKey("project:p2", "usage:cost");
    const c = numberMemoryKey("project:p1", "usage:input-tokens");
    writeNumberMemory(a, "1");
    expect(readNumberMemory(b)).toBeNull();
    expect(readNumberMemory(c)).toBeNull();
  });

  it("超过上限时淘汰最早的键，最近写入的键保留", () => {
    writeNumberMemory("k1", "1", 2);
    writeNumberMemory("k2", "2", 2);
    writeNumberMemory("k3", "3", 2);
    expect(numberMemorySize()).toBe(2);
    expect(readNumberMemory("k1")).toBeNull();
    expect(readNumberMemory("k2")).toBe("2");
    expect(readNumberMemory("k3")).toBe("3");
  });

  it("重复写入同一键会刷新其位置，淘汰时先丢最久未写的键", () => {
    writeNumberMemory("k1", "1", 2);
    writeNumberMemory("k2", "2", 2);
    writeNumberMemory("k1", "1b", 2);
    writeNumberMemory("k3", "3", 2);
    expect(readNumberMemory("k2")).toBeNull();
    expect(readNumberMemory("k1")).toBe("1b");
    expect(readNumberMemory("k3")).toBe("3");
  });

  it("读取不改变淘汰顺序", () => {
    writeNumberMemory("k1", "1", 2);
    writeNumberMemory("k2", "2", 2);
    readNumberMemory("k1");
    writeNumberMemory("k3", "3", 3);
    expect(numberMemorySize()).toBe(3);
    expect(readNumberMemory("k1")).toBe("1");
  });

  it("默认上限为常量值", () => {
    expect(NUMBER_MEMORY_LIMIT).toBeGreaterThan(0);
  });

  it("记忆键要求会话与槽位都非空", () => {
    expect(() => numberMemoryKey("", "usage")).toThrow(RangeError);
    expect(() => numberMemoryKey("project:p1", "  ")).toThrow(RangeError);
    expect(numberMemoryKey(" project:p1 ", " usage ")).toBe(
      "project:p1::usage",
    );
  });
});
