import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { UserActionNotice } from "./UserActionNotice";

describe("用户处理说明", () => {
  it("非软件缺项先展示可操作说明，技术依据与执行指导默认折叠", () => {
    const html = renderToStaticMarkup(<UserActionNotice action={{
      summary: "请提供原始销售数据", why: "缺少原始记录，无法核对报表。", owner: "数据提供方",
      steps: ["上传本月 CSV，并告知文件路径。"], after: "记录路径并继续，AI 将核验数据和完成报表。",
    }} guidance="核对 CSV 字段并重跑统计" details="source hash: abc123" />);
    for (const text of ["请提供原始销售数据", "数据提供方", "上传本月 CSV", "记录路径并继续"]) expect(html).toContain(text);
    expect(html.indexOf("上传本月 CSV")).toBeLessThan(html.indexOf("source hash"));
    expect(html).toContain("Agent 执行指导");
    expect(html).toContain("技术详情与判断依据");
    expect(html).not.toMatch(/<details[^>]*\bopen/);
  });

  it("保留 Markdown 命令但不自动执行，也不从历史长文猜测操作", () => {
    const html = renderToStaticMarkup(<UserActionNotice details="历史记录：曾缺 SDK，但已解决。" />);
    expect(html).toContain("没有单独的用户处理说明");
    expect(html).toContain("历史记录：曾缺 SDK，但已解决。");
    expect(html).not.toContain("需要做什么");
    const command = renderToStaticMarkup(<UserActionNotice action={{ summary: "补充工具", why: "检查需要工具", owner: "用户", steps: ["```bash\nexample --version\n```"], after: "记录指导并继续" }} />);
    expect(command).toContain("example --version");
    expect(command).toContain("<pre");
    expect(command).not.toContain("<script");
  });
});
