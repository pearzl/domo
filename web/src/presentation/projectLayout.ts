/// 项目详情页的顶层区块顺序：阶段决定分析、任务与报告的主次，其余信息保持在页面后部。

export const PROJECT_BLOCK_KEYS = ["analysis", "tasks", "report"] as const;

export type ProjectBlockKey = (typeof PROJECT_BLOCK_KEYS)[number];

/// 阶段未知时按任务优先展示，避免异常阶段值让页面空掉或抛错。
const TASKS_FIRST: ProjectBlockKey[] = ["tasks", "analysis", "report"];

/**
 * 三个顶层区块始终保留在顺序中，只调整先后：
 * 分析阶段先给目标、方案与确认入口，执行阶段先给任务，总结与完成阶段先给报告。
 * 阶段值缺失或未知时回退到任务优先。
 */
export function projectBlockOrder(stage: string | null | undefined): ProjectBlockKey[] {
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
