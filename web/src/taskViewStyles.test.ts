import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/// 样式表约束按源码原文核对，选择器与声明都取当前实现。
const STYLES = readFileSync("src/styles.css", "utf8").replace(/\s+/g, " ");

function block(selector: string): string {
  const start = STYLES.indexOf(`${selector} {`);
  expect(start, `缺少 ${selector} 规则`).toBeGreaterThanOrEqual(0);
  return STYLES.slice(start, STYLES.indexOf("}", start) + 1);
}

/// 选择器列表中的首个选择器：从它开始取到规则结束，便于核对同一组声明。
function ruleStarting(selector: string): string {
  const start = STYLES.indexOf(selector);
  expect(start, `缺少 ${selector} 规则`).toBeGreaterThanOrEqual(0);
  return STYLES.slice(start, STYLES.indexOf("}", start) + 1);
}

/// 媒体查询块内包含指定选择器的那一段。
function media(maxWidth: number, marker: string): string {
  const head = `@media (max-width: ${maxWidth}px) {`;
  let start = STYLES.indexOf(head);
  while (start >= 0) {
    let depth = 0;
    for (let index = start + head.length - 1; index < STYLES.length; index += 1) {
      if (STYLES[index] === "{") depth += 1;
      if (STYLES[index] === "}") {
        depth -= 1;
        if (depth === 0) {
          const chunk = STYLES.slice(start, index + 1);
          if (chunk.includes(marker)) return chunk;
          break;
        }
      }
    }
    start = STYLES.indexOf(head, start + head.length);
  }
  expect.fail(`缺少包含 ${marker} 的 ${head} 媒体查询`);
}

function maxHeight(rule: string): string {
  return /max-height:\s*([^;]+);/.exec(rule)?.[1] ?? "";
}

/// 视口限高声明的最小像素值，用于核对显示区相对 420px 显著放宽。
function minHeightPx(value: string): number {
  const match = /clamp\(\s*(\d+)px/.exec(value);
  return match ? Number(match[1]) : 0;
}

describe("任务抽屉区块容器", () => {
  it("任务描述、验收标准、结果摘要、验收摘要、反馈、历史与作业列表共用同一卡片语言", () => {
    const shared = ruleStarting(".drawer-body > section");
    expect(shared).toContain(".drawer-body > div > section,");
    expect(shared).toContain(".drawer-body > .drawer-lead > section,");
    expect(shared).toContain("padding: 14px 16px;");
    expect(shared).toContain("border: 1px solid var(--line);");
    expect(shared).toContain("border-radius: 12px;");
    expect(shared).toContain("background: var(--bg);");
  });

  it("结果摘要、验收摘要与历史区块不单独覆盖卡片边界或底色", () => {
    for (const selector of [
      ".drawer .task-result",
      ".drawer .task-verification",
      ".drawer .task-history",
    ]) {
      const head = `${selector} {`;
      let start = STYLES.indexOf(head);
      while (start >= 0) {
        const body = STYLES.slice(start, STYLES.indexOf("}", start) + 1);
        expect(body, `${selector} 不应单独定义边界`).not.toContain("border:");
        expect(body, `${selector} 不应单独定义底色`).not.toContain("background:");
        start = STYLES.indexOf(head, start + head.length);
      }
    }
  });

  it("区块标题层级一致", () => {
    const heading = ruleStarting(".drawer-body > section > h3");
    expect(heading).toContain("margin: 0 0 8px;");
  });
});

describe("作业视图显示区", () => {
  it("活动列表限高按视口比例放大且明显高于 420px", () => {
    const value = maxHeight(block(".activity-list"));
    expect(value).toContain("vh");
    expect(minHeightPx(value)).toBeGreaterThan(420);
  });

  it("活动文本行与消息块不形成列表内的嵌套滚动", () => {
    const line = block(".activity-line");
    expect(maxHeight(line)).toBe("none");
    expect(line).toContain("overflow: visible;");
    const message = block(".activity-message");
    expect(message).not.toContain("max-height");
    expect(message).not.toContain("overflow:");
  });

  it("原文与代码块限高同步放宽", () => {
    for (const selector of [".rich-content-raw", ".rich-code"]) {
      const value = maxHeight(block(selector));
      expect(value).toContain("vh");
      expect(minHeightPx(value)).toBeGreaterThanOrEqual(420);
    }
  });

  it("窄屏媒体查询使用视口比例而非固定小窗", () => {
    const narrow = media(840, ".rich-code");
    expect(narrow).toContain(".rich-code");
    expect(narrow).toContain(".rich-content-raw");
    expect(narrow).toContain("max-height: 56vh;");
    expect(narrow).not.toContain("max-height: 320px;");
  });
});
