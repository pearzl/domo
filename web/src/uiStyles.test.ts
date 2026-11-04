import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const styles = readFileSync("src/styles.css", "utf8");
const variables = new Map([...styles.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)].map(match => [match[1], match[2].trim()]));

function luminance(hex: string) {
  const channels = hex.replace("#", "").match(/../g)!.map(channel => {
    const value = parseInt(channel, 16) / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722;
}

describe("全站界面样式契约", () => {
  it("所有引用的设计变量都有定义", () => {
    for (const match of styles.matchAll(/var\((--[\w-]+)/g)) {
      expect(variables.has(match[1]), match[1]).toBe(true);
    }
  });

  it("语义色与辅助文字满足小字号文字 4.5:1 对比度", () => {
    for (const [foreground, background] of [
      ["--success", "--success-soft"], ["--warn", "--warn-soft"],
      ["--danger", "--danger-soft"], ["--accent", "--accent-soft"],
      ["--muted", "--bg"], ["--muted", "--accent-soft"], ["--muted", "--danger-soft"],
    ]) {
      const values = [luminance(variables.get(foreground)!), luminance(variables.get(background)!)].sort((a, b) => b - a);
      expect((values[0] + 0.05) / (values[1] + 0.05), `${foreground} on ${background}`).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("表单与卡片网格的最小列宽受容器限制", () => {
    expect(styles).toContain("minmax(min(100%, 240px), 1fr)");
    expect(styles).toContain("minmax(min(100%, 340px), 1fr)");
    const disabled = styles.match(/\.agent-card\.disabled\s*\{([^}]+)\}/)![1];
    expect(disabled).not.toContain("opacity");
  });
});
