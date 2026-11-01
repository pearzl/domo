import { describe, expect, it } from "vitest";
import {
  DRAFT_NAMESPACE,
  DRAFT_MAX_AGE_MS,
  clearDraft,
  draftStorageKey,
  pruneDrafts,
  readDraft,
  readDraftJson,
  writeDraft,
  writeDraftJson,
  type DraftStorage,
} from "./draft";

class MemoryStorage implements DraftStorage {
  private data = new Map<string, string>();
  failReads = false;
  failWrites = false;

  get length(): number {
    return this.data.size;
  }

  key(index: number): string | null {
    return Array.from(this.data.keys())[index] ?? null;
  }

  getItem(key: string): string | null {
    if (this.failReads) throw new Error("SecurityError");
    return this.data.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    if (this.failWrites) throw new Error("QuotaExceededError");
    this.data.set(key, value);
  }

  removeItem(key: string): void {
    this.data.delete(key);
  }

  raw(key: string): string | null {
    return this.data.get(key) ?? null;
  }

  keys(): string[] {
    return Array.from(this.data.keys());
  }
}

describe("draftStorageKey", () => {
  it("包含命名空间、项目、任务与字段，并编码分隔符", () => {
    const key = draftStorageKey({
      projectId: "p/1",
      taskId: "t 2",
      field: "analysis:message",
    });
    expect(key.startsWith(`${DRAFT_NAMESPACE}:`)).toBe(true);
    expect(key).toContain("project:p%2F1");
    expect(key).toContain("task:t%202");
    expect(key).toContain("field:analysis%3Amessage");
  });

  it("同一作用域稳定，不同项目、任务或字段互不覆盖", () => {
    const base = { projectId: "p1", taskId: "t1", field: "guidance" };
    expect(draftStorageKey(base)).toBe(draftStorageKey({ ...base }));
    expect(draftStorageKey(base)).not.toBe(
      draftStorageKey({ ...base, projectId: "p2" }),
    );
    expect(draftStorageKey(base)).not.toBe(
      draftStorageKey({ ...base, taskId: "t2" }),
    );
    expect(draftStorageKey(base)).not.toBe(
      draftStorageKey({ ...base, field: "reason" }),
    );
  });

  it("支持自定义命名空间与无项目/任务的表单草稿", () => {
    const key = draftStorageKey({
      namespace: "custom",
      field: "agent-form:new",
    });
    expect(key).toBe("custom:field:agent-form%3Anew");
  });

  it("字段名为空时拒绝生成", () => {
    expect(() => draftStorageKey({ field: "  " })).toThrow(RangeError);
  });
});

describe("writeDraft / readDraft", () => {
  it("写入后读取原值，空字符串清空", () => {
    const storage = new MemoryStorage();
    const key = draftStorageKey({ projectId: "p1", field: "analysis" });
    expect(writeDraft(storage, key, "项目目标", 1_000)).toBe(true);
    expect(readDraft(storage, key, { now: 1_000 })).toBe("项目目标");
    expect(writeDraft(storage, key, "")).toBe(true);
    expect(readDraft(storage, key)).toBeNull();
  });

  it("非法 JSON、结构不符与错误版本都静默降级", () => {
    const storage = new MemoryStorage();
    const key = draftStorageKey({ projectId: "p1", field: "analysis" });
    for (const raw of [
      "{",
      "null",
      "[]",
      JSON.stringify({ version: 2, savedAt: 1, value: "x" }),
      JSON.stringify({ version: 1, savedAt: "yesterday", value: "x" }),
      JSON.stringify({ version: 1, savedAt: 1, value: 42 }),
    ]) {
      storage.setItem(key, raw);
      expect(readDraft(storage, key, { now: 2 })).toBeNull();
    }
  });

  it("超过最大长度的内容不写入，并移除旧草稿", () => {
    const storage = new MemoryStorage();
    const key = draftStorageKey({ projectId: "p1", field: "analysis" });
    writeDraft(storage, key, "旧内容", 1_000);
    expect(writeDraft(storage, key, "x".repeat(20_001), 2_000)).toBe(false);
    expect(storage.raw(key)).toBeNull();
    expect(readDraft(storage, key, { now: 2_000 })).toBeNull();
  });

  it("超长与过期的历史数据读取时视为无效", () => {
    const storage = new MemoryStorage();
    const key = draftStorageKey({ projectId: "p1", field: "analysis" });
    writeDraft(storage, key, "x".repeat(100), 1_000);
    expect(readDraft(storage, key, { now: 1_001, maxLength: 10 })).toBeNull();
    expect(
      readDraft(storage, key, { now: 1_000 + DRAFT_MAX_AGE_MS + 1 }),
    ).toBeNull();
  });

  it("隐私模式或存储抛错时读写清空都不抛错", () => {
    const storage = new MemoryStorage();
    const key = draftStorageKey({ projectId: "p1", field: "analysis" });
    storage.failWrites = true;
    expect(writeDraft(storage, key, "内容")).toBe(false);
    storage.failWrites = false;
    writeDraft(storage, key, "内容", 1_000);
    storage.failReads = true;
    expect(readDraft(storage, key, { now: 1_000 })).toBeNull();
    expect(() => clearDraft(storage, key)).not.toThrow();
    expect(() => readDraft(null, key)).not.toThrow();
    expect(writeDraft(null, key, "内容")).toBe(false);
  });
});

describe("JSON 草稿", () => {
  const isStringArray = (value: unknown): value is string[] =>
    Array.isArray(value) && value.every((item) => typeof item === "string");

  it("合法 JSON 通过校验后返回原结构", () => {
    const storage = new MemoryStorage();
    const key = draftStorageKey({ projectId: "p1", field: "add-task-deps" });
    expect(writeDraftJson(storage, key, ["T1", "T2"], 1_000)).toBe(true);
    expect(
      readDraftJson(storage, key, isStringArray, { now: 1_000 }),
    ).toEqual(["T1", "T2"]);
  });

  it("非法 JSON 与校验失败返回 null", () => {
    const storage = new MemoryStorage();
    const key = draftStorageKey({ projectId: "p1", field: "add-task-deps" });
    writeDraft(storage, key, "not-json", 1_000);
    expect(readDraftJson(storage, key, isStringArray, { now: 1_000 })).toBeNull();
    writeDraft(storage, key, JSON.stringify({ a: 1 }), 1_000);
    expect(readDraftJson(storage, key, isStringArray, { now: 1_000 })).toBeNull();
  });
});

describe("pruneDrafts", () => {
  it("只清理本命名空间下的过期、损坏与超长条目", () => {
    const storage = new MemoryStorage();
    const now = 10_000;
    const valid = draftStorageKey({ projectId: "p1", field: "analysis" });
    const expired = draftStorageKey({ projectId: "p1", field: "replan" });
    const broken = draftStorageKey({ projectId: "p1", field: "feedback" });
    const overlong = draftStorageKey({ projectId: "p1", field: "guidance" });
    const other = "another.app:key";
    writeDraft(storage, valid, "保留", now);
    writeDraft(storage, expired, "过期", now - DRAFT_MAX_AGE_MS - 1);
    storage.setItem(broken, "{broken");
    storage.setItem(overlong, JSON.stringify({ version: 1, savedAt: now, value: "x".repeat(30_000) }));
    storage.setItem(other, "其他数据");

    expect(pruneDrafts(storage, { now })).toBe(3);
    expect(storage.raw(valid)).not.toBeNull();
    expect(storage.raw(expired)).toBeNull();
    expect(storage.raw(broken)).toBeNull();
    expect(storage.raw(overlong)).toBeNull();
    expect(storage.raw(other)).toBe("其他数据");
  });

  it("没有可清理项时返回 0", () => {
    const storage = new MemoryStorage();
    const key = draftStorageKey({ projectId: "p1", field: "analysis" });
    writeDraft(storage, key, "保留", 1_000);
    expect(pruneDrafts(storage, { now: 1_000 })).toBe(0);
    expect(pruneDrafts(null)).toBe(0);
  });
});
