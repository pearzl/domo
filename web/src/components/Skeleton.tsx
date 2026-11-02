/**
 * 加载骨架：数据到达前铺几条占位，尺寸按行固定，替换真实内容时不改变页面高度。
 * 动画只在 CSS 里播放（.skeleton 的背景扫光），减少动效时由 prefers-reduced-motion 块关闭。
 *
 * 读取内容的辅助文本保留在可访问名称里，屏幕阅读器仍能知道当前正在加载。
 */
export function SkeletonLines({ label }: { label: string }) {
  return (
    <div className="skeleton-lines" role="status" aria-label={label}>
      <span className="skeleton" aria-hidden="true" />
      <span className="skeleton" aria-hidden="true" />
      <span className="skeleton" aria-hidden="true" />
    </div>
  );
}
