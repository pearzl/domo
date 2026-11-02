/**
 * 数字字符滚动的轻量 hook。
 *
 * 调用方给出一个“会话键（需包含项目 id）+ 数值槽位 + 当前值”，hook 返回上一次渲染的值
 * 与按当前值算好的逐位滚动计划。上一次值按会话缓存在 presentation/numberMemory.ts，
 * 组件卸载重挂（切换选项卡、折叠再展开）后仍能接上，从而在新值出现时继续滚动。
 *
 * 约束：
 * - 只读写进程内缓存，不请求后端、不写 localStorage；刷新页面后重新开始。
 * - 首次渲染或键变化后没有可比的上一次值时，不产生滚动，直接展示当前值。
 * - 渲染阶段只读缓存（纯查询），写回放在 layout effect：既不在渲染期产生副作用，
 *   也不会因为写回触发重渲染而打断正在播放的滚动动画。
 */

import { useLayoutEffect, useMemo } from "react";
import { rollingPlan, type RollPlan } from "./animatedNumber";
import { numberMemoryKey, readNumberMemory, writeNumberMemory } from "./numberMemory";

export interface AnimatedNumberState {
  /// 上一次渲染时的字符串；没有记录时为 null（首次渲染或换了会话）。
  previous: string | null;
  /// 与 value 等长的逐位滚动计划。
  plan: RollPlan;
}

export function useAnimatedNumber(
  session: string,
  slot: string,
  value: string,
): AnimatedNumberState {
  const key = numberMemoryKey(session, slot);
  // 上一次值只来自缓存：等于当前值时计划为空，值变化时按缓存里的旧值逐位比较。
  const previous = readNumberMemory(key);
  const plan = useMemo(() => rollingPlan(previous, value), [previous, value]);

  // 提交后写回当前值，供下一次渲染与重新挂载后比较。
  useLayoutEffect(() => {
    writeNumberMemory(key, value);
  });

  return { previous, plan };
}
