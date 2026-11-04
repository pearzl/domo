import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AgentAdvice, GlobalQueue } from "./TeamOperations";
import { ResourcePanel, resourceRequest } from "./ResourcePanel";
import type { ProjectDetail } from "../types";
import { vi } from "vitest";

describe("项目资源与团队操作", () => {
  const detail = { project: { id: "p", stage: "completed", status: null, workspace_type: "plain" }, tasks: [], agents: [] } as unknown as ProjectDetail;
  it("普通目录展示保护边界但没有项目合入操作", () => {
    const html = renderToStaticMarkup(<ResourcePanel detail={detail} onSaved={() => {}} />);
    expect(html).toContain("项目共享信息");
    expect(html).toContain("不承诺恢复历史内容");
    expect(html).toContain("授权该任务修改指定路径");
    expect(html).not.toContain("确认风险并处理冲突");
  });
  it("归档项目不提供写入授权或检查确认", () => {
    const archived = { ...detail, project: { ...detail.project, status: "archived" } };
    const html = renderToStaticMarkup(<ResourcePanel detail={archived} onSaved={() => {}} />);
    expect(html).not.toContain("授权该任务修改指定路径");
    expect(html).toContain("输入与产物");
  });
  it("Git 完成项目明确说明人工风险和独立项目作业", () => {
    const git = { ...detail, project: { ...detail.project, workspace_type: "git" as const } };
    const html = renderToStaticMarkup(<ResourcePanel detail={git} onSaved={() => {}} />);
    expect(html).toContain("确认风险并处理冲突");
    expect(html).toContain("不创建普通任务");
  });
  it("队列保留人工顺序，配置建议不自动改池", () => {
    expect(renderToStaticMarkup(<GlobalQueue />)).toContain("不自动按关键路径重排");
    expect(renderToStaticMarkup(<AgentAdvice />)).toContain("不自动修改池");
  });
  it("资源请求保留服务端错误，DELETE 不发送虚构请求体", async () => {
    const mock = vi.fn().mockResolvedValue({ ok: false, json: async () => ({ error: "保留现场" }) });
    vi.stubGlobal("fetch", mock);
    try {
      await expect(resourceRequest("/api/projects/p/merge/assist", "DELETE")).rejects.toThrow("保留现场");
      expect(mock.mock.calls[0][1].method).toBe("DELETE");
      expect(mock.mock.calls[0][1].body).toBeUndefined();
    } finally { vi.unstubAllGlobals(); }
  });
});
