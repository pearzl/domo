import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { InputBox } from "./InputBox";

describe("InputBox 预览模式", () => {
  it("把 Markdown 渲染为对应标签", () => {
    const markup = renderToStaticMarkup(
      <InputBox
        value={"# 项目目标\n\n- 第一项\n- 第二项\n\n**加粗** 与 `code`"}
        onChange={() => {}}
        defaultMode="preview"
      />,
    );
    expect(markup).toContain("<h1");
    expect(markup).toContain("项目目标");
    expect(markup).toContain("<ul");
    expect(markup).toContain("<li>");
    expect(markup).toContain("<strong>加粗</strong>");
    expect(markup).toContain("rich-inline-code");
    expect(markup).not.toContain("<textarea");
  });

  it("展示字符数与编辑/预览切换", () => {
    const markup = renderToStaticMarkup(
      <InputBox value={"# 标题"} onChange={() => {}} defaultMode="preview" />,
    );
    expect(markup).toContain("4 字符");
    expect(markup).toContain("编辑");
    expect(markup).toContain("预览");
    expect(markup).toContain('aria-pressed="true"');
  });

  it("限制长度时字数展示为 count/max", () => {
    const markup = renderToStaticMarkup(
      <InputBox value={"abc"} onChange={() => {}} maxLength={100} />,
    );
    expect(markup).toContain("3/100 字符");
    expect(markup).toContain('maxLength="100"');
  });

  it("空内容预览给出提示而不是空白区域", () => {
    const markup = renderToStaticMarkup(
      <InputBox value="   " onChange={() => {}} defaultMode="preview" />,
    );
    expect(markup).toContain("还没有内容");
  });
});

describe("InputBox 超限与操作区", () => {
  it("长文本带内部滚动类名并提示超限", () => {
    const markup = renderToStaticMarkup(
      <InputBox
        value={"长".repeat(600)}
        onChange={() => {}}
        maxAutoGrowChars={480}
      />,
    );
    expect(markup).toContain("input-box--scroll");
    expect(markup).toContain("超限，内部滚动");
    expect(markup).toContain("600 字符");
    expect(markup).toContain("<textarea");
  });

  it("操作区渲染在输入框容器内，长文本时随容器固定", () => {
    const markup = renderToStaticMarkup(
      <InputBox
        value={"长".repeat(600)}
        onChange={() => {}}
        actions={<button type="button">发送</button>}
      />,
    );
    expect(markup).toContain('class="input-box-actions"');
    expect(markup).toContain("发送");
    /// 操作区在超限输入框之后渲染，固定在容器底部，长文本时仍可达。
    expect(markup.indexOf("</textarea>")).toBeLessThan(
      markup.indexOf("input-box-actions"),
    );
    expect(markup.indexOf("input-box-actions")).toBeLessThan(
      markup.lastIndexOf("</div>"),
    );
  });
});
