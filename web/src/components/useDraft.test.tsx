import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";
import {
  browserDraftStorage,
  clearDraft,
  draftStorageKey,
  writeDraft,
} from "../presentation/draft";
import { useDraftValue } from "./useDraft";

/// 节点环境没有 window/localStorage；这里用最小的内存实现模拟浏览器存储，
/// 验证“挂载时恢复草稿、清空后不再回填”的真实调用路径。

class MemoryStorage {
  private data = new Map<string, string>();

  get length(): number {
    return this.data.size;
  }

  key(index: number): string | null {
    return Array.from(this.data.keys())[index] ?? null;
  }

  getItem(key: string): string | null {
    return this.data.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.data.set(key, value);
  }

  removeItem(key: string): void {
    this.data.delete(key);
  }
}

function installWindow(storage: MemoryStorage | null): void {
  (globalThis as { window?: unknown }).window = storage
    ? { localStorage: storage }
    : {};
}

function uninstallWindow(): void {
  delete (globalThis as { window?: unknown }).window;
}

function DraftProbe({
  projectId,
  field,
  initial,
}: {
  projectId: string;
  field: string;
  initial: string;
}) {
  const draft = useDraftValue({ projectId, field }, initial);
  return (
    <output data-key={draft.key} data-value={draft.value}>
      {draft.value || "(空)"}
    </output>
  );
}

function scopeKey(projectId: string, field: string): string {
  return draftStorageKey({ projectId, field });
}

afterEach(() => {
  uninstallWindow();
});

describe("useDraftValue", () => {
  it("重新挂载时从本机草稿恢复输入内容", () => {
    const storage = new MemoryStorage();
    installWindow(storage);
    const key = scopeKey("p1", "analysis-message");
    expect(writeDraft(browserDraftStorage(), key, "第一轮输入的记忆")).toBe(true);

    const markup = renderToStaticMarkup(
      <DraftProbe projectId="p1" field="analysis-message" initial="" />,
    );
    expect(markup).toContain("第一轮输入的记忆");
    expect(markup).toContain(`data-key="${key}"`);
  });

  it("提交成功后清空草稿，重新挂载回到空值", () => {
    const storage = new MemoryStorage();
    installWindow(storage);
    const key = scopeKey("p1", "analysis-message");
    writeDraft(browserDraftStorage(), key, "准备提交的内容");
    clearDraft(browserDraftStorage(), key);

    const markup = renderToStaticMarkup(
      <DraftProbe projectId="p1" field="analysis-message" initial="" />,
    );
    expect(markup).toContain("(空)");
    expect(markup).not.toContain("准备提交的内容");
  });

  it("草稿损坏或存储不可用时回退到初始值且不抛错", () => {
    const storage = new MemoryStorage();
    installWindow(storage);
    storage.setItem(scopeKey("p1", "analysis-message"), "{broken");

    expect(
      renderToStaticMarkup(
        <DraftProbe projectId="p1" field="analysis-message" initial="" />,
      ),
    ).toContain("(空)");

    // window 存在但读取 localStorage 抛错（隐私模式）时同样静默降级。
    installWindow(null);
    expect(
      renderToStaticMarkup(
        <DraftProbe projectId="p1" field="analysis-message" initial="" />,
      ),
    ).toContain("(空)");
  });

  it("项目与字段不同则互不串用", () => {
    const storage = new MemoryStorage();
    installWindow(storage);
    writeDraft(
      browserDraftStorage(),
      scopeKey("p1", "analysis-message"),
      "P1 的内容",
    );
    writeDraft(
      browserDraftStorage(),
      scopeKey("p2", "analysis-message"),
      "P2 的内容",
    );

    expect(
      renderToStaticMarkup(
        <DraftProbe projectId="p1" field="analysis-message" initial="" />,
      ),
    ).toContain("P1 的内容");
    expect(
      renderToStaticMarkup(
        <DraftProbe projectId="p1" field="replan-reason" initial="" />,
      ),
    ).toContain("(空)");
  });
});
