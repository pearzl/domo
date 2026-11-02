import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/// 直接读源文件：样式表不是 JS 模块，?raw 在 CSS 上取到的是空串。
const stylesSource = readFileSync(
  new URL("../styles.css", import.meta.url).pathname,
  "utf8",
);

/// 所有 @keyframes 动画名。
function keyframeNames(source: string): Set<string> {
  const names = new Set<string>();
  for (const match of source.matchAll(/@keyframes\s+([a-z0-9-]+)/gi)) {
    names.add(match[1]);
  }
  return names;
}

/// 每处 animation / animation-name 声明后面跟的第一个动画名。
function animatedNames(source: string): Set<string> {
  const names = new Set<string>();
  for (const match of source.matchAll(
    /animation(?:-name)?:\s*([a-z][a-z0-9-]*)\b/gi,
  )) {
    names.add(match[1]);
  }
  return names;
}

/// 取最外层 prefers-reduced-motion 块（按大括号配对），返回块内文本。
function reducedMotionBlocks(source: string): string[] {
  const blocks: string[] = [];
  const marker = "@media (prefers-reduced-motion: reduce)";
  let cursor = 0;
  while (cursor < source.length) {
    const start = source.indexOf(marker, cursor);
    if (start < 0) break;
    const open = source.indexOf("{", start);
    let depth = 1;
    let index = open + 1;
    while (index < source.length && depth > 0) {
      if (source[index] === "{") depth += 1;
      if (source[index] === "}") depth -= 1;
      index += 1;
    }
    blocks.push(source.slice(open + 1, index - 1));
    cursor = index;
  }
  return blocks;
}

/// 选择器列表：忽略注释后，取每个规则块的头部，按逗号拆开。
function selectorsOf(source: string): string[] {
  const clean = source.replace(/\/\*[\s\S]*?\*\//g, "");
  const selectors: string[] = [];
  for (const match of clean.matchAll(/([^{}]+)\{[^{}]*\}/g)) {
    const raw = match[1].trim();
    if (!raw || raw.startsWith("@")) continue;
    for (const part of raw.split(",")) {
      const selector = part.trim();
      if (selector) selectors.push(selector);
    }
  }
  return selectors;
}

describe("styles.css 动效变量", () => {
  it("在 :root 定义统一的时长、缓动与位移变量", () => {
    for (const name of [
      "--motion-fast",
      "--motion-base",
      "--motion-slow",
      "--motion-ease",
      "--motion-ease-out",
      "--motion-rise",
      "--motion-lift",
      "--motion-press",
      "--motion-color",
    ]) {
      expect(stylesSource).toContain(`${name}:`);
    }
  });

  it("选项卡切换与活动条目使用统一动画变量而不是各自的时长", () => {
    expect(stylesSource).toContain(
      "animation: project-panel-in var(--motion-base) var(--motion-ease-out) both;",
    );
    expect(stylesSource).toContain(
      "animation: motion-fade-in var(--motion-base) var(--motion-ease) both;",
    );
  });
});

describe("styles.css 减少动效覆盖", () => {
  const keyframes = keyframeNames(stylesSource);
  const reducedBlocks = reducedMotionBlocks(stylesSource);
  const reducedSource = reducedBlocks.join("\n");
  const selectors = selectorsOf(reducedSource);
  const mainSource = reducedBlocks[reducedBlocks.length - 1] ?? "";

  it("存在 prefers-reduced-motion: reduce 媒体块", () => {
    expect(reducedBlocks.length).toBeGreaterThan(0);
  });

  it("减少动效只保留透明度动画 motion-fade-in", () => {
    const used = animatedNames(reducedSource);
    expect(used.size).toBeGreaterThan(0);
    expect([...used].sort()).toEqual(["motion-fade-in", "none"]);
    expect(keyframes.has("motion-fade-in")).toBe(true);
  });

  it("位移与缩放的动画类都被覆盖", () => {
    for (const selector of [
      ".motion-rise",
      ".project-panel",
      ".animated-number-digit.is-up",
      ".animated-number-digit.is-down",
    ]) {
      expect(selectors).toContain(selector);
    }
  });

  it("动效层的循环与位移动画类都有减少动效覆盖", () => {
    for (const selector of [
      ".skeleton",
      ".status-dot",
      ".animated-number-digit.is-up",
      ".animated-number-digit.is-down",
      ".task-row",
      ".project-status-fade",
      ".motion-fade",
      ".project-panel",
    ]) {
      expect(selectors).toContain(selector);
    }
    // 按压回弹只作用于按钮，选择器带伪类，单独判断。
    expect(selectors).toContain("button:not(:disabled):active");
  });

  it("循环动画与按压回弹被关闭", () => {
    expect(reducedSource).toMatch(/button:not\(:disabled\):active\s*\{[^}]*animation: none;/);
    expect(reducedSource).toMatch(/\.skeleton\s*\{[^}]*animation: none;/);
    expect(reducedSource).toMatch(
      /\.status-dot,\s*\.activity-dot-live\s*\{[^}]*animation: none;/,
    );
  });

  it("悬停抬升只保留颜色过渡", () => {
    expect(reducedSource).not.toContain("@keyframes");
    const match = reducedSource.match(/\.task-row,\s*\.project-card\s*\{([^}]*)\}/);
    expect(match).not.toBeNull();
    const body = match?.[1] ?? "";
    expect(body).toContain("transform: none");
    expect(body).toContain("border-color");
    expect(body).not.toContain("transform var");
    expect(mainSource).toMatch(/\.task-row:hover[^{]*\{[^}]*transform: none;/);
  });
});
