/// 任务抽屉布局：首区块按状态决定，其余区块固定顺序，Agent 池始终位于末尾配置区。

export type DrawerLead = "gate" | "result" | "control";

export type DrawerTailSection =
  | "control"
  | "result"
  | "milestone"
  | "acceptance"
  | "description"
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
  archived?: boolean;
}

export interface DrawerLayout {
  /// 首区块：gate 承载裁决/人工验收，result 回答结果，control 承载进行中任务的控制条。
  lead: DrawerLead;
  /// 首区块之后依次呈现的区块；tail 内每个区块只出现一次，末尾固定为 Agent 池。
  tail: DrawerTailSection[];
  /// gate 面板承载失败原因与原始错误，抽屉其它位置不再完整重复。
  gateCarriesError: boolean;
}

/// 首区块选择：blocked 与里程碑人工验收先裁决；已完成/已取消先给结果；其余进行中先给控制条。
export function drawerLead(task: DrawerTaskInput): DrawerLead {
  if (task.human_gate === "milestone") return "gate";
  if (task.status === "blocked") return "gate";
  if (task.stage === "completed" || task.status === "canceled") return "result";
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
    if (task.result_summary) tail.push("result");
  } else if (lead === "result") {
    // 结果摘要已在首区块，控制条只对少数仍可操作的状态补充。
    if (canControlTask(task)) tail.push("control");
  } else {
    tail.push("milestone", "result");
  }

  tail.push("acceptance", "description", "derive", "deps");
  // 原始错误只保留一个默认收起的入口；有 gate 时由 gate 面板承载。
  if (!gateCarriesError && task.last_error) tail.push("rawError");
  tail.push("guidance", "jobs", "agentPool");

  return { lead, tail, gateCarriesError };
}

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
