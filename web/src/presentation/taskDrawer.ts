/// 任务抽屉布局：首区块按状态决定，其余区块固定顺序，Agent 池始终位于末尾配置区。

export type DrawerLead = "gate" | "result" | "control";

export type DrawerTailSection =
  | "control"
  | "result"
  | "verification"
  | "history"
  | "milestone"
  | "description"
  | "acceptance"
  | "feedback"
  | "derive"
  | "deps"
  | "rawError"
  | "guidance"
  | "jobs"
  | "agentPool";

export interface DrawerTaskInput {
  stage: string;
  status: string | null;
  human_gate: string | null;
  result_summary?: string | null;
  last_error?: string | null;
  verifier_issues?: string[];
  archived?: boolean;
}

/// 描述与验收标准共用的一级区块类名：两者同级、同样的标题与 Markdown 排版。
export const DRAWER_CONTENT_SECTION_CLASS = "drawer-content-section";
export const DRAWER_DESCRIPTION_TITLE = "任务描述";
export const DRAWER_ACCEPTANCE_TITLE = "验收标准";
/// 验收反馈独立成区块，只在存在反馈时出现。
export const DRAWER_FEEDBACK_TITLE = "验收反馈";

export interface DrawerLayout {
  /// 首区块：gate 承载裁决/人工验收，result 回答结果，control 承载进行中任务的控制条。
  lead: DrawerLead;
  /// 首区块之后依次呈现的区块；tail 内每个区块只出现一次，末尾固定为 Agent 池。
  tail: DrawerTailSection[];
  /// gate 面板承载失败原因与原始错误，抽屉其它位置不再完整重复。
  gateCarriesError: boolean;
}

/// 任务终态：已完成与已取消的任务才展示最终结果摘要与验收摘要。
export function isTaskTerminal(task: DrawerTaskInput): boolean {
  return task.stage === "completed" || task.status === "canceled";
}

/// 首区块选择：blocked 与里程碑人工验收先裁决；已完成/已取消先给结果；其余进行中先给控制条。
export function drawerLead(task: DrawerTaskInput): DrawerLead {
  if (task.human_gate === "milestone") return "gate";
  if (task.status === "blocked") return "gate";
  if (isTaskTerminal(task)) return "result";
  return "control";
}

/// 控制条只在任务仍可操作时出现，已归档、已完成与已取消的任务没有控制动作。
export function canControlTask(task: DrawerTaskInput): boolean {
  return (
    !task.archived && task.stage !== "completed" && task.status !== "canceled"
  );
}

/// 完整布局；调用方按 tail 顺序渲染，Agent 池固定为最后一项。
export function drawerLayout(task: DrawerTaskInput): DrawerLayout {
  const lead = drawerLead(task);
  const gateCarriesError =
    task.human_gate === "milestone" || task.status === "blocked";
  const tail: DrawerTailSection[] = [];

  if (lead === "gate") {
    // 裁决之后先给可执行动作，再给结果；已完成里程碑的结果摘要是裁决依据。
    if (canControlTask(task)) tail.push("control");
    if (isTaskTerminal(task) && task.result_summary) tail.push("result");
  } else if (lead === "result") {
    // 结果摘要已在首区块，控制条只对少数仍可操作的状态补充。
    if (canControlTask(task)) tail.push("control");
  } else {
    tail.push("milestone");
  }

  tail.push("description", "acceptance");
  // 最终验收摘要只在任务终态呈现；进行中的验收过程由历史与各作业视图承载。
  if (isTaskTerminal(task)) tail.push("verification");
  // 验收反馈只在验收者留下问题时出现，与验收标准分开并列。
  if (hasVerifierFeedback(task)) tail.push("feedback");
  // 执行与验收历史汇总逐轮执行与验收作业，回答“每一轮发生了什么”。
  tail.push("history");
  tail.push("derive", "deps");

  // 原始错误只保留一个默认收起的入口；有 gate 时由 gate 面板承载。
  if (!gateCarriesError && task.last_error) tail.push("rawError");
  tail.push("guidance", "jobs", "agentPool");

  return { lead, tail, gateCarriesError };
}

/// 验收反馈是否存在：空白反馈不算反馈。
export function hasVerifierFeedback(task: DrawerTaskInput): boolean {
  return (task.verifier_issues ?? []).some((issue) => issue.trim() !== "");
}

/// 实际展示的验收反馈：去掉空白项。
export function verifierFeedback(issues: string[] | null | undefined): string[] {
  return (issues ?? []).filter((issue) => issue.trim() !== "");
}

/// 验收标准按 Markdown 列表排版，与任务描述使用同一套渲染。
export function acceptanceMarkdown(acceptance: string[]): string {
  return acceptance
    .map((item) => item.trim())
    .filter((item) => item !== "")
    .map((item) => `- ${item}`)
    .join("\n");
}

/// 依赖与位置的关键字段常显，其余字段折叠。
export type DepsField =
  | "deps"
  | "queue"
  | "waitingReason"
  | "branch"
  | "worktree"
  | "failureKind"
  | "agent"
  | "archiveRef"
  | "archiveRecovery";

/// 常显字段：任务排在哪、为什么还没开始。
export const DEPS_PRIMARY_FIELDS: DepsField[] = ["deps", "queue", "waitingReason"];
/// 折叠字段的基础列表：归档后才出现的 ref 与恢复命令由 depsFoldedFields 追加。
export const DEPS_FOLDED_FIELDS: DepsField[] = [
  "failureKind",
  "agent",
  "branch",
  "worktree",
];

/// 折叠区字段：基础列表 + 归档回收后才会出现的归档 ref 与恢复命令。
export function depsFoldedFields({
  archiveReference,
  archiveRecovery,
}: {
  archiveReference: boolean;
  archiveRecovery: boolean;
}): DepsField[] {
  return [
    ...DEPS_FOLDED_FIELDS,
    ...(archiveReference ? (["archiveRef"] as DepsField[]) : []),
    ...(archiveRecovery ? (["archiveRecovery"] as DepsField[]) : []),
  ];
}

export const DEPS_FIELD_LABELS: Record<DepsField, string> = {
  deps: "依赖",
  queue: "排队位置",
  waitingReason: "等待原因",
  branch: "分支",
  worktree: "工作区",
  failureKind: "失败分类",
  agent: "Agent",
  archiveRef: "归档 ref",
  archiveRecovery: "恢复命令",
};

/// 原始错误入口：没有 gate 承载时保留一个默认收起的入口，有 gate 时由 gate 面板承载。
export function rawErrorEntry(layout: DrawerLayout): "collapsed" | "none" {
  return layout.tail.includes("rawError") ? "collapsed" : "none";
}

/// 首区块标题，用于首屏断言。
export const DRAWER_LEAD_TITLES: Record<DrawerLead, string> = {
  gate: "裁决",
  result: "结果",
  control: "控制",
};
