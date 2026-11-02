/// 项目详情页主体选项卡与顶层区块的布局规则。

/// 主体选项卡：阶段优先区块按阶段排序，概览与信息页固定排在后面。
export const PROJECT_BLOCK_KEYS = [
  "analysis",
  "tasks",
  "report",
  "overview",
  "info",
] as const;

export type ProjectBlockKey = (typeof PROJECT_BLOCK_KEYS)[number];

export const PROJECT_TAB_LABELS: Record<ProjectBlockKey, string> = {
  analysis: "项目分析",
  tasks: "任务",
  report: "项目报告",
  overview: "概览",
  info: "信息",
};

/// 与信息选项卡同级、始终可见的顶层区块；活动与审计默认折叠。
export const PROJECT_TOP_BLOCK_KEYS = ["agents", "activity"] as const;

export type ProjectTopBlockKey = (typeof PROJECT_TOP_BLOCK_KEYS)[number];

export const PROJECT_TOP_BLOCK_LABELS: Record<ProjectTopBlockKey, string> = {
  agents: "项目 Agent 池",
  activity: "活动与审计",
};

/// 阶段未知时按任务优先展示，避免异常阶段值让页面空掉或抛错。
const TASKS_FIRST: ProjectBlockKey[] = ["tasks", "analysis", "report"];

export function isProjectBlockKey(
  value: string | null | undefined,
): value is ProjectBlockKey {
  return !!value && (PROJECT_BLOCK_KEYS as readonly string[]).includes(value);
}

/**
 * 阶段决定分析、任务与报告三块的先后，概览与信息页稳定排在末尾：
 * 分析阶段先给目标、方案与确认入口，执行阶段先给任务，总结与完成阶段先给报告。
 * 阶段值缺失或未知时回退到任务优先。
 */
export function projectBlockOrder(
  stage: string | null | undefined,
): ProjectBlockKey[] {
  return [...stageOrder(stage), "overview", "info"];
}

function stageOrder(stage: string | null | undefined): ProjectBlockKey[] {
  switch (stage) {
    case "analysis":
      return ["analysis", "tasks", "report"];
    case "executing":
      return ["tasks", "analysis", "report"];
    case "summarizing":
    case "completed":
      return ["report", "tasks", "analysis"];
    default:
      return [...TASKS_FIRST];
  }
}

/// 当前选项卡失效时回退到阶段默认选项卡（阶段优先区块的第一块）。
export function resolveProjectTab(
  stage: string | null | undefined,
  current: string | null | undefined,
): ProjectBlockKey {
  const order = projectBlockOrder(stage);
  return isProjectBlockKey(current) && order.includes(current)
    ? current
    : order[0];
}

export interface ProjectBadgeInput {
  /// 未取消的任务数；null 表示项目详情尚未加载。
  taskCount: number | null;
  /// 阶段内已生成待确认方案。
  proposalPending: boolean;
  /// 阶段内已有项目报告且仍在等待验收。
  reportReady: boolean;
}

/// 选项卡徽标文案：任务数、待确认方案与报告就绪标记；无可展示内容时返回 null。
export function projectTabBadge(
  key: ProjectBlockKey,
  input: ProjectBadgeInput,
): string | null {
  switch (key) {
    case "analysis":
      return input.proposalPending ? "待确认方案" : null;
    case "tasks":
      return input.taskCount !== null && input.taskCount > 0
        ? `任务 ${input.taskCount}`
        : null;
    case "report":
      return input.reportReady ? "报告就绪" : null;
    default:
      return null;
  }
}

/// 键盘事件到移动方向的映射；未占用的按键返回 null，交由浏览器默认行为。
const KEY_DIRECTIONS: Record<string, "left" | "right" | "first" | "last"> = {
  ArrowLeft: "left",
  ArrowRight: "right",
  Home: "first",
  End: "last",
};

export function tabDirectionForKey(
  key: string,
): "left" | "right" | "first" | "last" | null {
  return KEY_DIRECTIONS[key] ?? null;
}

/// 选项卡间左右移动：Home/End 直达首尾，左右在两端循环，保证键盘焦点不丢失。
export function nextProjectTab(
  keys: readonly ProjectBlockKey[],
  current: ProjectBlockKey,
  direction: "left" | "right" | "first" | "last",
): ProjectBlockKey {
  if (keys.length === 0) return current;
  if (direction === "first") return keys[0];
  if (direction === "last") return keys[keys.length - 1];
  const index = keys.indexOf(current);
  if (index < 0) return direction === "left" ? keys[keys.length - 1] : keys[0];
  const step = direction === "left" ? -1 : 1;
  return keys[(index + step + keys.length) % keys.length];
}
