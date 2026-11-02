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

/// 单条动效声明里第一项既是动画名又不是关键字时返回该动画名。
function animationNameOf(declaration: string): string | null {
  const value = declaration.replace(/^[^:]*:\s*/, "").trim();
  const first = value.split(/[\s,]+/)[0];
  if (!first || !/^[a-z][a-z0-9-]*$/i.test(first)) return null;
  return first.toLowerCase();
}

interface AnimatedRule {
  /// 规则块的选择器列表（已按逗号拆开并去空白）；at-rule 头本身为空。
  selectors: string[];
  /// 规则块内出现的动画名（含 none），以及是否为“只改透明度/颜色”的白名单动画。
  names: { name: string; opacityOnly: boolean }[];
  /// 是否位于 prefers-reduced-motion 媒体块内。
  reduced: boolean;
}

/// 只改透明度或颜色的动画：产生淡入、呼吸或变色，不涉及位移、缩放与循环扫光，
/// 减少动效下保持原样也能被接受。键是动画名，值是理由；新增这类动画时在此登记。
const OPACITY_ONLY_ANIMATIONS: Record<string, string> = {
  "motion-fade-in": "不透明度从 0 变到 1，不改变位置与尺寸。",
  "motion-pulse-dot": "只在 0.35 与 1 之间呼吸不透明度，元素本身不移动。",
};

/// 按大括号配对解析样式表，得到每个规则块的选择器、动画名与所属媒体块。
/// @keyframes 块内的 from/to/百分比不是选择器，整段跳过；@media 块保留条件头。
function animatedRules(source: string): AnimatedRule[] {
  const rules: AnimatedRule[] = [];
  // 注释会附在前一个规则头后面，先整体去掉，避免污染选择器文本。
  const clean = source.replace(/\/\*[\s\S]*?\*\//g, "");

  /// 从 start 开始线性扫描到配对闭括号；返回闭括号下标。
  function walk(start: number, atRules: string[]): number {
    let preludeStart = start;
    let index = start;
    while (index < clean.length) {
      const char = clean[index];
      if (char === "}") return index;
      if (char !== "{") {
        index += 1;
        continue;
      }
      const prelude = clean.slice(preludeStart, index).trim();
      if (/^@keyframes\b/i.test(prelude)) {
        // 关键帧块内部只有帧选择器与声明，不参与覆盖判断。
        index = matchingBrace(clean, index + 1) + 1;
        preludeStart = index;
        continue;
      }
      const bodyEnd = matchingBrace(clean, index + 1);
      if (prelude.startsWith("@")) {
        // @media 等分组块：把条件头带入下一层。
        walk(index + 1, [...atRules, prelude]);
      } else {
        const names: AnimatedRule["names"] = [];
        for (const declaration of clean
          .slice(index + 1, bodyEnd)
          .matchAll(/(?:^|;)\s*animation(?:-name)?\s*:[^;{}]*/gi)) {
          const name = animationNameOf(declaration[0].trim());
          if (!name) continue;
          names.push({
            name,
            opacityOnly: OPACITY_ONLY_ANIMATIONS[name] !== undefined,
          });
        }
        if (names.length > 0) {
          rules.push({
            selectors: prelude
              .split(",")
              .map((part) => part.trim())
              .filter(Boolean),
            names,
            reduced: atRules.some((entry) =>
              entry.includes("prefers-reduced-motion: reduce"),
            ),
          });
        }
      }
      index = bodyEnd + 1;
      preludeStart = index;
    }
    return index;
  }

  walk(0, []);
  return rules;
}

/// 从 open 处（{ 之后）找到与之配对的 } 的下标。
function matchingBrace(source: string, open: number): number {
  let level = 1;
  for (let index = open; index < source.length; index += 1) {
    if (source[index] === "{") level += 1;
    if (source[index] === "}") level -= 1;
    if (level === 0) return index;
  }
  return source.length;
}

/// 减少动效块的选择器：同一规则块内出现减少动效声明的选择器。
function reducedSelectors(rules: AnimatedRule[]): Set<string> {
  const selectors = new Set<string>();
  for (const rule of rules) {
    if (!rule.reduced) continue;
    for (const selector of rule.selectors) selectors.add(selector);
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

describe("styles.css 减少动效覆盖完整性", () => {
  const rules = animatedRules(stylesSource);
  const covered = reducedSelectors(rules);
  const reducedRules = rules.filter((rule) => rule.reduced);
  const mainSource = reducedMotionBlocks(stylesSource).at(-1) ?? "";

  it("每个非 none 动画名要么被减少动效覆盖，要么登记为只改透明度", () => {
    const unchecked: string[] = [];
    for (const rule of rules) {
      if (rule.reduced || rule.selectors.length === 0) continue;
      const uncovered = rule.names.filter(
        ({ name, opacityOnly }) => name !== "none" && !opacityOnly,
      );
      if (uncovered.length === 0) continue;
      const missing = rule.selectors.filter((selector) => !covered.has(selector));
      if (missing.length > 0) {
        unchecked.push(
          `${missing.join(", ")} → ${uncovered.map(({ name }) => name).join(", ")}`,
        );
      }
    }
    expect(unchecked).toEqual([]);
  });

  it("动画名白名单里每一项都有解释，且与实际动画名一致", () => {
    for (const [name, reason] of Object.entries(OPACITY_ONLY_ANIMATIONS)) {
      expect(keyframeNames(stylesSource).has(name)).toBe(true);
      expect(reason.trim().length).toBeGreaterThan(0);
    }
    for (const { names } of rules) {
      for (const { name, opacityOnly } of names) {
        expect(opacityOnly).toBe(OPACITY_ONLY_ANIMATIONS[name] !== undefined);
      }
    }
  });

  it("每个 @keyframes 都被某处 animation 或在减少动效块中使用", () => {
    const used = animatedNames(stylesSource);
    for (const name of keyframeNames(stylesSource)) {
      expect(used.has(name)).toBe(true);
    }
  });

  it("减少动效块中的规则只使用 motion-fade-in 或 none", () => {
    expect(reducedRules.length).toBeGreaterThan(0);
    for (const rule of reducedRules) {
      for (const { name } of rule.names) {
        expect(["motion-fade-in", "none"]).toContain(name);
      }
    }
  });

  describe("任务抽屉执行轮次与验收尝试", () => {
    /// 轮次与验收尝试的规则只覆盖动画；轮次头的悬停规则是 transition，没有动画名。
    const historySelectors = [
      ".drawer .task-history .task-history-round",
      ".drawer .task-history .task-history-attempt",
      ".drawer .task-history .task-history-unlinked",
    ];

    it("轮次、验收尝试与未关联提示都在减少动效块中被覆盖", () => {
      for (const selector of historySelectors) {
        expect(covered.has(selector)).toBe(true);
      }
    });

    it("轮次与验收尝试只淡入或维持透明度，不做位移", () => {
      for (const selector of historySelectors) {
        const rule = reducedRules.find((entry) =>
          entry.selectors.includes(selector),
        );
        expect(rule).toBeDefined();
        expect(rule?.names.map(({ name }) => name)).toContain("motion-fade-in");
      }
    });

    it("轮次头悬停只保留颜色过渡", () => {
      const body = mainSource.match(
        /\.drawer \.task-history \.task-history-round-head,\s*\.drawer \.task-history \.task-history-unlinked\s*\{([^}]*)\}/,
      );
      expect(body).not.toBeNull();
      expect(body?.[1] ?? "").toContain("transform: none");
      expect(body?.[1] ?? "").toContain("border-color");
      expect(body?.[1] ?? "").not.toContain("transform var");
    });
  });

  describe("概览卡片与用量摘要", () => {
    const overviewSelectors = [
      ".overview-stat",
      ".overview-todo",
      ".usage-summary",
      ".usage-summary-stat",
    ];

    it("概览卡片、待办与用量摘要卡片都在减少动效块中被覆盖", () => {
      for (const selector of overviewSelectors) {
        expect(covered.has(selector)).toBe(true);
      }
    });

    it("概览与用量摘要卡片挂载时只淡入，不做位移", () => {
      const rule = reducedRules.find((entry) =>
        entry.selectors.includes(".overview-stat"),
      );
      expect(rule).toBeDefined();
      for (const selector of overviewSelectors) {
        expect(rule?.selectors).toContain(selector);
      }
      expect(rule?.names.map(({ name }) => name)).toEqual(["motion-fade-in"]);
    });

    it("概览与用量摘要里的数值滚动只淡入", () => {
      for (const selector of [
        ".overview-value .animated-number-digit.is-up",
        ".overview-value .animated-number-digit.is-down",
        ".usage-summary-value .animated-number-digit.is-up",
        ".usage-summary-value .animated-number-digit.is-down",
      ]) {
        expect(covered.has(selector)).toBe(true);
      }
      const body = mainSource.match(
        /\.overview-value \.animated-number-digit\.is-up,[\s\S]*?\.usage-summary-value \.animated-number-digit\.is-down\s*\{([^}]*)\}/,
      );
      expect(body).not.toBeNull();
      expect(body?.[1] ?? "").toContain("animation-name: motion-fade-in");
    });

    it("概览网格重排与用量摘要卡片没有引入位移或缩放动画", () => {
      for (const rule of rules) {
        if (rule.reduced) continue;
        const touchesOverview = rule.selectors.some(
          (selector) =>
            selector.startsWith(".overview-") || selector.startsWith(".usage-summary"),
        );
        if (!touchesOverview) continue;
        for (const { name } of rule.names) {
          expect(OPACITY_ONLY_ANIMATIONS[name] !== undefined).toBe(true);
        }
      }
    });
  });
});
