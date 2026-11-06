import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ProjectDetail } from "../types";
import { ResourcePanel } from "./ResourcePanel";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function response(body: unknown): Response {
  return { ok: true, json: async () => body } as Response;
}

const detail = {
  project: { id: "p", status: "running", workspace_type: "plain", updated_at: "t" },
  tasks: [],
} as unknown as ProjectDetail;
let renderer: ReactTestRenderer;
let requests: Array<{ url: string } & ReturnType<typeof deferred<Response>>>;

beforeEach(async () => {
  requests = [];
  vi.stubGlobal("fetch", vi.fn((url: string) => {
    if (url.endsWith("/shared")) return Promise.resolve(response({ records: [], version: "v" }));
    if (url.endsWith("/merge/assist")) return Promise.resolve(response({ request: null }));
    if (url.endsWith("/artifacts")) return Promise.resolve(response({
      root: "/workspace", items: ["A", "B"].map(id => ({ id, path: `${id}.md`, kind: "delivery", task_id: null })),
    }));
    const request = { url, ...deferred<Response>() };
    requests.push(request);
    return request.promise;
  }));
  await act(async () => { renderer = create(<ResourcePanel detail={detail} onSaved={() => {}} />); });
});

afterEach(async () => {
  await act(async () => renderer.unmount());
  vi.unstubAllGlobals();
});

async function openArtifact(index: number) {
  await act(async () => {
    renderer.root.findAllByType("button")
      .filter(button => button.children.includes("查看 / 版本"))[index].props.onClick();
  });
}

async function complete(index: number, id: string, commit = "v1") {
  await act(async () => {
    requests[index].resolve(response({ path: `${id}.md`, text: `${id}-${commit}`, commit, versions: ["v1", "v2"] }));
  });
}

function previewText() {
  return renderer.root.findAllByType("pre").map(node => node.children.join(""));
}

describe("artifact preview request ordering", () => {
  it("忽略旧产物的迟到响应，版本请求使用显示内容所属的产物", async () => {
    await openArtifact(0);
    await openArtifact(1);
    await complete(1, "B");
    await complete(0, "A");
    expect(previewText()).toEqual(["B-v1"]);
    await act(async () => {
      renderer.root.findByProps({ "aria-label": "产物版本" }).props.onChange({ target: { value: "v2" } });
    });
    expect(requests[2].url).toBe("/api/projects/p/artifacts/B?commit=v2");
    expect(previewText()).toEqual([]);
    await complete(2, "B", "v2");
    expect(previewText()).toEqual(["B-v2"]);
  });

  it("旧请求失败不覆盖当前预览与错误状态", async () => {
    await openArtifact(0);
    await openArtifact(1);
    await complete(1, "B");
    await act(async () => { requests[0].reject(new Error("旧产物失败")); });
    expect(previewText()).toEqual(["B-v1"]);
    expect(JSON.stringify(renderer.toJSON())).not.toContain("旧产物失败");
  });

  it("切换产物时清空旧预览，当前请求失败可见", async () => {
    await openArtifact(0);
    await complete(0, "A");
    await openArtifact(1);
    expect(previewText()).toEqual([]);
    await act(async () => { requests[1].reject(new Error("当前产物失败")); });
    expect(JSON.stringify(renderer.toJSON())).toContain("当前产物失败");
  });

  it("项目切换后不应用旧项目的预览响应", async () => {
    await openArtifact(0);
    const next = { ...detail, project: { ...detail.project, id: "next" } };
    await act(async () => { renderer.update(<ResourcePanel detail={next} onSaved={() => {}} />); });
    await complete(0, "A");
    expect(previewText()).toEqual([]);
  });
});
