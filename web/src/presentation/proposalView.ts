import type { AnalysisProposal, Task } from "../types";

/// 方案展示的小节键。顺序即展示顺序：先看目标与前提，再看拆分与执行，最后看如何自检。
export const PROPOSAL_SECTION_KEYS = [
  "goal",
  "assumptions",
  "acceptance",
  "tasks",
] as const;

export type ProposalSectionKey = (typeof PROPOSAL_SECTION_KEYS)[number];

export const PROPOSAL_SECTION_LABELS: Record<ProposalSectionKey, string> = {
  goal: "目标",
  assumptions: "假设",
  acceptance: "验收口径",
  tasks: "任务方案",
};

/// 任务方案小节标题固定带数量，便于不开卡片就能看出拆分规模。
export function proposalTaskSectionLabel(count: number): string {
  return `${PROPOSAL_SECTION_LABELS.tasks}（${count} 个任务）`;
}

export interface ProposalSection {
  key: ProposalSectionKey;
  label: string;
  /// 小节条数：目标为 0（单段文本），其余为列表项数量。
  count: number;
  /// 有内容的小节默认展开；空小节不渲染。
  expanded: boolean;
}

/// 方案超过这个内容量时空小节也保留折叠入口，让结构始终可见。
export const PROPOSAL_LONG_CHARS = 600;

function lengthOf(values: string[]): number {
  return values.reduce((total, value) => total + value.length, 0);
}

export function proposalSize(proposal: AnalysisProposal): number {
  return (
    proposal.summary.length +
    (proposal.goal?.length ?? 0) +
    lengthOf(proposal.assumptions) +
    lengthOf(proposal.acceptance) +
    proposal.tasks.reduce(
      (total, task) =>
        total +
        task.title.length +
        task.description.length +
        lengthOf(task.acceptance),
      0,
    )
  );
}

export function isLongProposal(proposal: AnalysisProposal): boolean {
  return proposalSize(proposal) >= PROPOSAL_LONG_CHARS;
}

/// 方案按固定六节组织；长方案只展开总结与任务方案，其余小节收进折叠，
/// 用户仍可逐个展开查看完整内容。
export function proposalSections(proposal: AnalysisProposal): ProposalSection[] {
  const counts: Record<ProposalSectionKey, number> = {
    goal: 0,
    assumptions: proposal.assumptions.length,
    acceptance: proposal.acceptance.length,
    tasks: proposal.tasks.length,
  };
  const filled: Record<ProposalSectionKey, boolean> = {
    goal: !!proposal.goal?.trim(),
    assumptions: counts.assumptions > 0,
    acceptance: counts.acceptance > 0,
    tasks: counts.tasks > 0,
  };
  const long = isLongProposal(proposal);
  return PROPOSAL_SECTION_KEYS.map((key) => ({
    key,
    label:
      key === "tasks"
        ? proposalTaskSectionLabel(counts.tasks)
        : PROPOSAL_SECTION_LABELS[key],
    count: long ? counts[key] : 0,
    expanded: long ? key === "tasks" : filled[key],
  }));
}

export function sectionSummary(section: ProposalSection): string {
  if (section.key === "goal") return "确认后的项目目标";
  return `${section.count} 条`;
}

/// 自动确认开关的可操作边界：归档与暂停会锁住开关，分析作业运行中仍可调整。
export function autoConfirmLocked(status: string | null | undefined): boolean {
  return status === "archived" || status === "paused" || status === "pausing";
}

/// 分析作业运行中调整确认方式时，提示新设置从下一轮分析开始生效。
export function confirmModeRuntimeNote(analysisRunning: boolean): string | null {
  return analysisRunning ? "将在下一轮分析生效" : null;
}

/// 切换开关行的展示：状态文案、说明与是否为开启态。
export interface ConfirmModeRow {
  enabled: boolean;
  title: string;
  description: string;
}

export function confirmModeRow(mode: "manual" | "auto"): ConfirmModeRow {
  if (mode === "auto") {
    return {
      enabled: true,
      title: "自动确认已开启",
      description:
        "方案足够明确时由 AI 直接开始执行；仍有歧义时规划者会提问并等待你确认。",
    };
  }
  return {
    enabled: false,
    title: "自动确认已关闭",
    description:
      "每个方案都需要你点击确认后才开始执行；规划者的结论会先停在这里等你。",
  };
}

/// 确认后的影响：方案落地为任务、项目进入执行、自动确认是否生效。
export interface ConfirmImpact {
  items: string[];
  note: string;
}

export function confirmImpact(
  proposal: AnalysisProposal,
  activeTasks: Pick<Task, "key">[],
  mode: "manual" | "auto",
): ConfirmImpact {
  if (proposal.replan_impact) {
    const replan = proposal.replan_impact;
    const items = [
      replan.canceled_task_keys.length
        ? `取消 ${replan.canceled_task_keys.length} 个未完成任务（${replan.canceled_task_keys.join("、")}）`
        : "没有未完成任务需要取消",
      `保留 ${replan.retained_task_keys.length} 个已完成任务`,
    ];
    if (replan.added_task_keys.length) {
      items.push(`扩大任务范围（${replan.added_task_keys.join("、")}）`);
    }
    items.push("已确认目标保持不变");
    if (replan.acceptance_changed) items.push("修改项目或任务验收标准");
    items.push("应用时一次性切换计划；分析失败不会破坏当前计划");
    return {
      items,
      note: replan.high_impact
        ? "这是高影响重规划草稿；生成后由 DOMO 自动应用，不受方案确认方式影响。"
        : "这是低影响重规划草稿；生成后由 DOMO 自动应用，不受方案确认方式影响。",
    };
  }
  const counts = proposal.tasks.length
    ? `${proposal.tasks.length} 个任务（${proposal.tasks
        .slice(0, 4)
        .map((task) => task.key)
        .join("、")}${proposal.tasks.length > 4 ? " 等" : ""}）`
    : "空任务清单";
  const hasDeps = proposal.tasks.some((task) => task.deps.length > 0);
  return {
    items: [
      activeTasks.length > 0
        ? `现有 ${activeTasks.length} 个任务会被新的任务方案作废`
        : `按方案创建 ${counts}`,
      hasDeps
        ? "任务按依赖顺序派发，前置任务完成后才开始"
        : "任务之间没有依赖，可按派发顺序并行开始",
      "项目进入执行阶段，任务选项卡开始出现待执行的作业",
    ],
    note:
      mode === "auto"
        ? "已开启自动确认：方案足够明确时不会停在这里；你在这里确认会立即开始执行。"
        : "已关闭自动确认：必须由上方的确认按钮开始执行。",
  };
}
