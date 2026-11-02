import type { ReactNode } from "react";
import { useAnimatedNumber } from "../presentation/useAnimatedNumber";

/**
 * 数字类字符滚动：用量与进度在数值变化时逐位滚动，首次渲染与值不变时保持静态。
 *
 * 实现要点：
 * - 只有变化的数字位包进带动画的 span，其余字符原样输出，文本内容与传入 value 完全一致。
 * - 上一次值按“项目会话 + 槽位”缓存在 presentation/numberMemory.ts，切换选项卡或折叠展开
 *   导致组件重挂后仍能接上，不会把已有数值当成首次渲染。
 * - 动画只在 CSS 里播放，减少动效时只保留透明度变化（见 styles.css 的 reduced-motion 块）。
 *
 * 人工核对清单（浏览器手动核对）：
 * 1. 依次切换项目选项卡，面板淡入上移，内容区高度不被动画撑开、无布局跳动。
 * 2. 展开活动时间线（思维链、工具与步骤分组），新条目淡入且列表不抖动、不跳行。
 * 3. 用量或进度数值变化时数字逐位滚动；值不变时不出现动画。
 * 4. 开启系统“减少动效”后，上述动画只做透明度变化，没有位移与循环。
 */
export function AnimatedNumber({
  session,
  slot,
  value,
  className,
}: {
  /// 项目会话键（应包含项目 id），决定上一次值的缓存范围。
  session: string;
  /// 同一会话内的数值槽位名，如 usage:cost。
  slot: string;
  /// 展示用文本（含千分位、单位或“—”）。
  value: string;
  className?: string;
}): ReactNode {
  const { plan } = useAnimatedNumber(session, slot, value);
  const classes = className ? `animated-number ${className}` : "animated-number";

  // 逐字符拼装：滚动位用带键的 span，键里带字符变化信息，只有变化的位会重新播放动画。
  const parts: ReactNode[] = [];
  let plain = "";
  const flushPlain = () => {
    if (!plain) return;
    parts.push(plain);
    plain = "";
  };

  for (let index = 0; index < value.length; index += 1) {
    const step = plan[index];
    if (!step) {
      plain += value[index];
      continue;
    }
    flushPlain();
    parts.push(
      <span
        key={`${index}:${step.key}`}
        className={`animated-number-digit${step.fromAbove ? " is-up" : " is-down"}`}
        style={{ animationDelay: `${Math.min(index * 18, 90)}ms` }}
      >
        {value[index]}
      </span>,
    );
  }
  flushPlain();

  return (
    <span className={classes} data-animated-value={value}>
      {parts}
    </span>
  );
}
