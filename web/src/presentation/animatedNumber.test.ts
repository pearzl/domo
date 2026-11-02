import { describe, expect, it } from "vitest";
import {
  changedValue,
  isDigitChar,
  progressPercent,
  rollingPlan,
} from "./animatedNumber";

describe("rollingPlan 首次渲染", () => {
  it("没有上一次值时所有位都不滚动", () => {
    const plan = rollingPlan(null, "1,234");
    expect(plan).toEqual([null, null, null, null, null]);
    expect(plan).toHaveLength("1,234".length);
  });

  it("值与上一次相同时不产生滚动", () => {
    expect(rollingPlan("12 个", "12 个").every((step) => step === null)).toBe(true);
  });

  it("非数字文本（未知占位与单位）永不滚动", () => {
    expect(rollingPlan("—", "—")).toEqual([null]);
    expect(rollingPlan("0", "—")).toEqual([null]);
    // 占位变成数字时只淡入（没有可比的上一位），不上下滚动。
    expect(rollingPlan("—", "0")[0]).toEqual({ fromAbove: false, key: "in:0" });
  });
});

describe("rollingPlan 数值变化", () => {
  it("只对变化了的数字位给出滚动，未变的位保持静态", () => {
    const plan = rollingPlan("128", "129");
    expect(plan[0]).toBeNull();
    expect(plan[1]).toBeNull();
    expect(plan[2]).toEqual({ fromAbove: true, key: "8>9" });
  });

  it("数字变大时从上方落入，变小时从下方浮上", () => {
    expect(rollingPlan("1234", "1235")[3]).toMatchObject({ fromAbove: true });
    expect(rollingPlan("1235", "1234")[3]).toMatchObject({ fromAbove: false });
  });

  it("键编码了新旧字符，同一位再次变化会重新播放动画", () => {
    const first = rollingPlan("1", "2")[0]?.key;
    const second = rollingPlan("2", "1")[0]?.key;
    expect(first).toBe("1>2");
    expect(second).toBe("2>1");
    expect(first).not.toBe(second);
  });

  it("千分位与单位保持静态，只有数字位滚动", () => {
    const plan = rollingPlan("1,000", "1,234");
    expect(plan[0]).toBeNull();
    expect(plan[1]).toBeNull();
    expect(plan[3]).not.toBeNull();
    expect(plan[4]).not.toBeNull();
  });

  it("位数增加时从右对齐，只有多出的高位滚动", () => {
    const plan = rollingPlan("99", "100");
    const rolling = plan
      .map((step, index) => (step ? index : -1))
      .filter((index) => index >= 0);
    // 右对齐后个位 9→0、十位 9→0 都变化，百位 1 没有可比的上一位，只淡入。
    expect(rolling).toEqual([0, 1, 2]);
    expect(plan[0]).toMatchObject({ fromAbove: false });
  });

  it("位数减少时从右对齐，左侧新出现的位按不确定方向处理", () => {
    const plan = rollingPlan("1,234", "834");
    expect(plan).toHaveLength(3);
    // 右对齐后 "834" 的 8 对应旧值的 2，不是左侧多出的千分位。
    expect(plan[0]).toEqual({ fromAbove: true, key: "2>8" });
    // 低位字符与旧值相同，保持静态。
    expect(plan[1]).toBeNull();
    expect(plan[2]).toBeNull();
  });

  it("费用小数与货币单位只滚动数字", () => {
    const plan = rollingPlan("$0.0001", "$0.0002");
    expect(plan[0]).toBeNull();
    expect(plan[2]).toBeNull();
    expect(plan[6]).toMatchObject({ fromAbove: true });
  });

  it("进度文本中的斜杠不滚动", () => {
    const plan = rollingPlan("3/10", "4/10");
    expect(plan[0]).toMatchObject({ fromAbove: true });
    expect(plan[1]).toBeNull();
    expect(plan[2]).toBeNull();
    expect(plan[3]).toBeNull();
  });
});

describe("辅助判断", () => {
  it("isDigitChar 只认 ASCII 数字", () => {
    expect(isDigitChar("0")).toBe(true);
    expect(isDigitChar("9")).toBe(true);
    expect(isDigitChar("/")).toBe(false);
    expect(isDigitChar("，")).toBe(false);
  });

  it("changedValue 区分首次渲染与重复值", () => {
    expect(changedValue(null, "12")).toBe(false);
    expect(changedValue("12", "12")).toBe(false);
    expect(changedValue("12", "13")).toBe(true);
  });

  it("progressPercent 夹在 0 到 100 并取整", () => {
    expect(progressPercent(0, 0)).toBe(0);
    expect(progressPercent(5, 2)).toBe(40);
    expect(progressPercent(3, 1)).toBe(33);
    expect(progressPercent(3, 9)).toBe(100);
    expect(progressPercent(3, -1)).toBe(0);
  });
});
