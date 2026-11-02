import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { AnimatedNumber } from "./AnimatedNumber";
import { Progress } from "../ui";
import {
  clearNumberMemory,
  numberMemoryKey,
  writeNumberMemory,
} from "../presentation/numberMemory";

/// 渲染成静态标记：与服务端测试一致，首帧渲染不会执行 layout effect，
/// 因此这里验证的是“首帧与已有记忆”两种纯输入，不依赖计时器。
function markup(value: string, session = "project:p1", slot = "usage:cost") {
  return renderToStaticMarkup(
    <AnimatedNumber session={session} slot={slot} value={value} />,
  );
}

const DIGIT = 'class="animated-number-digit';

describe("AnimatedNumber 字符滚动", () => {
  it("首次渲染不产生滚动位，文本与传入值一致", () => {
    clearNumberMemory();
    const html = markup("1,234");
    expect(html).toContain("1,234");
    expect(html).not.toContain(DIGIT);
    expect(html).toContain('data-animated-value="1,234"');
  });

  it("记住上一次值后，只有变化的数字位带滚动动画", () => {
    clearNumberMemory();
    writeNumberMemory(numberMemoryKey("project:p1", "usage:cost"), "1,234");
    const html = markup("1,238");
    // 末位 4→8 变大，从上方落入；其余位保持静态文本。
    expect(html).toContain("is-up");
    expect(html.match(/animated-number-digit/g)?.length).toBe(1);
    expect(html).toContain(">8</span>");
    // 原样文本仍能拼回完整数值，不重复旧字符。
    expect(html.replace(/<[^>]+>/g, "")).toBe("1,238");
  });

  it("数字变小时从下方浮上", () => {
    clearNumberMemory();
    writeNumberMemory(numberMemoryKey("project:p1", "usage:cost"), "1,239");
    const html = markup("1,238");
    expect(html).toContain("is-down");
    expect(html.replace(/<[^>]+>/g, "")).toBe("1,238");
  });

  it("值没变化时保持静态", () => {
    clearNumberMemory();
    writeNumberMemory(numberMemoryKey("project:p1", "usage:cost"), "1,234");
    const html = markup("1,234");
    expect(html).not.toContain(DIGIT);
  });

  it("无法合计的占位“—”不滚动", () => {
    clearNumberMemory();
    writeNumberMemory(numberMemoryKey("project:p1", "usage:cost"), "1,234");
    const html = markup("—");
    expect(html).not.toContain(DIGIT);
    expect(html).toContain("—");
  });

  it("不同会话各自记忆：换项目后按新会话重新开始", () => {
    clearNumberMemory();
    writeNumberMemory(numberMemoryKey("project:p1", "usage:cost"), "10");
    const html = markup("42", "project:p2");
    expect(html).not.toContain(DIGIT);
    expect(html.replace(/<[^>]+>/g, "")).toBe("42");
  });
});

describe("Progress 进度文本", () => {
  it("首次渲染展示完成/总数与百分比宽度", () => {
    clearNumberMemory();
    const html = renderToStaticMarkup(<Progress total={5} completed={2} />);
    expect(html).toContain("2/5");
    expect(html).toContain("width:40%");
    expect(html).not.toContain(DIGIT);
  });

  it("按项目会话记忆后进度变化才滚动", () => {
    clearNumberMemory();
    writeNumberMemory(numberMemoryKey("project:p1", "progress:count"), "2/5");
    const html = renderToStaticMarkup(
      <Progress total={5} completed={3} session="project:p1" />,
    );
    expect(html).toContain(DIGIT);
    expect(html.replace(/<[^>]+>/g, "")).toBe("3/5");
  });
});
