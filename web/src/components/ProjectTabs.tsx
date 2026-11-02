import { useRef } from "react";
import type { KeyboardEvent } from "react";
import {
  PROJECT_TAB_LABELS,
  nextProjectTab,
  tabDirectionForKey,
  type ProjectBlockKey,
} from "../presentation/projectLayout";

export interface ProjectTabsProps {
  /// 阶段决定的选项卡顺序。
  order: readonly ProjectBlockKey[];
  /// 当前选中的选项卡。
  active: ProjectBlockKey;
  /// 选项卡徽标文案；值为 null 时不渲染徽标。
  badges?: Partial<Record<ProjectBlockKey, string | null>>;
  onSelect: (key: ProjectBlockKey) => void;
}

/**
 * 项目主体选项卡：ARIA tablist/tab 语义，roving tabindex，
 * 左右方向键在当前顺序内切换并同步焦点，Home/End 直达首尾。
 */
export function ProjectTabs({ order, active, badges, onSelect }: ProjectTabsProps) {
  const listRef = useRef<HTMLDivElement | null>(null);

  function move(current: ProjectBlockKey, key: string) {
    const direction = tabDirectionForKey(key);
    if (!direction) return false;
    const target = nextProjectTab(order, current, direction);
    onSelect(target);
    // 切换后把焦点落到目标选项卡，键盘操作不会丢失焦点。
    const node = listRef.current?.querySelector<HTMLButtonElement>(
      `#${tabButtonId(target)}`,
    );
    node?.focus();
    return true;
  }

  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (move(active, event.key)) {
      event.preventDefault();
    }
  }

  return (
    <div
      className="project-tabs"
      role="tablist"
      aria-label="项目主体视图"
      ref={listRef}
      onKeyDown={handleKeyDown}
    >
      {order.map((key) => {
        const selected = key === active;
        const badge = badges?.[key] ?? null;
        return (
          <button
            key={key}
            type="button"
            role="tab"
            id={tabButtonId(key)}
            aria-selected={selected}
            aria-controls={tabPanelId(key)}
            tabIndex={selected ? 0 : -1}
            className={`project-tab${selected ? " active" : ""}`}
            onClick={() => onSelect(key)}
          >
            <span className="project-tab-label">{PROJECT_TAB_LABELS[key]}</span>
            {badge ? (
              <span className="project-tab-badge" title={badge}>
                {badge}
              </span>
            ) : null}
          </button>
        );
      })}
    </div>
  );
}

export function tabButtonId(key: ProjectBlockKey): string {
  return `project-tab-${key}`;
}

export function tabPanelId(key: ProjectBlockKey): string {
  return `project-panel-${key}`;
}
