/**
 * 数字字符滚动的纯计算：把“上一次显示的字符串”变成“逐位滚动的格子计划”。
 *
 * 约束：
 * - 只比较同一位置的字符，一位一个格子；不是数字的字符（单位、百分号、空格）不做滚动。
 * - 从右往左对齐，幅度变化只让多出的高位滚动，低位不会整体平移。
 * - 长度或掩码不同的格子没有可比的上一位，直接按“前一位未知”处理，只做淡入。
 * - 首次渲染（没有上一次值）不产生任何滚动的格子，避免页面打开时满屏跳动。
 * - 一切结果都由输入决定，调用方负责把它渲染成 DOM 与样式。
 */

/// 只有 ASCII 数字参与滚动；千分位、小数点、单位等其余字符保持静态。
export function isDigitChar(char: string): boolean {
  return char >= "0" && char <= "9";
}

export interface RollingDigit {
  /// 滚动方向：true 表示从上方落入当前位，false 表示从下方浮上。
  fromAbove: boolean;
  /// 稳定且唯一的键；值变化时变化，驱动 CSS 重新播放动画。
  key: string;
}

/// 单个字符位的渲染计划：无滚动时为 null。
export type RollPlan = Array<RollingDigit | null>;

function digitAt(text: string, index: number): string | null {
  const char = text[index];
  return char !== undefined && isDigitChar(char) ? char : null;
}

function compareDigit(previous: string | null, current: string): RollingDigit {
  if (previous === null) {
    return { fromAbove: false, key: `in:${current}` };
  }
  const up = Number(current) > Number(previous);
  return {
    fromAbove: up,
    key: `${previous}>${current}`,
  };
}

/**
 * 逐位滚动计划。
 * @param previous 上一次显示的字符串；首次渲染传 null。
 * @param current 当前要显示的字符串。
 * @returns 与 current 等长的数组，非数字位与未变化的数字位为 null。
 */
export function rollingPlan(
  previous: string | null,
  current: string,
): RollPlan {
  const plan: RollPlan = new Array(current.length).fill(null);
  if (previous === null || previous === current) return plan;

  // 从右往左对齐：右侧位置相同，左侧多出的高位在旧值中不存在。
  const offset = current.length - previous.length;
  for (let index = 0; index < current.length; index += 1) {
    const currentChar = digitAt(current, index);
    if (currentChar === null) continue;
    const previousIndex = index - offset;
    const previousChar =
      previousIndex >= 0 && previousIndex < previous.length
        ? digitAt(previous, previousIndex)
        : null;
    if (previousChar === currentChar) continue;
    plan[index] = compareDigit(previousChar, currentChar);
  }
  return plan;
}

/// 上一次值与当前值是否构成一次真实的数值变化；用于避免首次渲染和重复值触发动画。
export function changedValue(previous: string | null, current: string): boolean {
  return previous !== null && previous !== current;
}

/// 进度条百分比：把完成数与总数换算成 0–100 的整数，总数非正时为 0。
export function progressPercent(total: number, completed: number): number {
  if (!Number.isFinite(total) || total <= 0) return 0;
  if (!Number.isFinite(completed)) return 0;
  const ratio = Math.round((completed / total) * 100);
  return Math.min(100, Math.max(0, ratio));
}
