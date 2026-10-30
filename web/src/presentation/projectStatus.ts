import type {
  AnalysisProposal,
  JobSummary,
  Message,
  Project,
  Task,
} from "../types";
/// 任务阶段文案统一取自 ui.tsx，概览与任务列表不会出现两套措辞。
import { taskStageLabel } from "../ui";

/// 概览只读取项目详情中已有的数据，不触发额外请求。
export interface ProjectStatusInput {
  project: Project;
  tasks: Task[];
  jobs: JobSummary[];
  messages: Message[];
  proposal: AnalysisProposal | null;
  /// 目标分支是否已包含项目分支提交，包含 DOMO 之外的合入。
  mergedIntoBase: boolean;
  /// 计算项目用时的参照时刻；页面传入当前时间，调用方可固定时间。
  now: number;
}

export interface ProjectStats {
  taskTotal: number;
  taskCompleted: number;
  taskPercent: number;
  jobTotal: number;
  jobFailed: number;
  usageKnown: number;
  usageUnknown: number;
  elapsedMs: number | null;
}

export type GuidanceTone = "muted" | "info" | "warn" | "danger" | "success";

export interface ExecutionTaskItem {
  key: string;
  stage: string;
  stageLabel: string;
}

export interface ExecutionTaskGroups {
  running: ExecutionTaskItem[];
  queued: ExecutionTaskItem[];
  starting: ExecutionTaskItem[];
}

export interface ProjectGuidance {
  tone: GuidanceTone;
  label: string;
  title: string;
  detail: string | null;
  tasks: ExecutionTaskGroups | null;
}

export interface ProjectStatus {
  stats: ProjectStats;
  guidance: ProjectGuidance;
}

/// 已取消任务不计入任务总数与完成数。
export function projectStats(input: ProjectStatusInput): ProjectStats {
  const activeTasks = input.tasks.filter(
    (task) => task.status !== "canceled",
  );
  const taskCompleted = activeTasks.filter(
    (task) => task.stage === "completed",
  ).length;
  const taskTotal = activeTasks.length;
  const jobTotal = input.jobs.length;
  const jobFailed = input.jobs.filter(
    (job) => job.status === "failed",
  ).length;
  const usageKnown = input.jobs.filter((job) => job.usage_known).length;

  return {
    taskTotal,
    taskCompleted,
    taskPercent:
      taskTotal > 0 ? Math.round((taskCompleted / taskTotal) * 100) : 0,
    jobTotal,
    jobFailed,
    usageKnown,
    usageUnknown: jobTotal - usageKnown,
    elapsedMs: projectElapsed(input.project, input.now),
  };
}

/// 项目用时从创建时刻算到交付、归档或当前时刻；时间字段不可解析时为未知。
export function projectElapsed(
  project: Project,
  now: number,
): number | null {
  const start = Date.parse(project.created_at);
  if (Number.isNaN(start)) return null;
  const terminal =
    project.stage === "completed" || project.status === "archived";
  const end = terminal
    ? Date.parse(project.merged_at ?? project.updated_at)
    : now;
  if (Number.isNaN(end) || end < start) return null;
  return end - start;
}

export function projectStatus(input: ProjectStatusInput): ProjectStatus {
  return {
    stats: projectStats(input),
    guidance: projectGuidance(input),
  };
}

/// 概览的“当前需要你处理”只描述现状与下一步，不执行任何项目操作。
/// 暂停状态优先于阶段判断：暂停中的项目不会继续推进，也不产生阶段内的待办。
export function projectGuidance(input: ProjectStatusInput): ProjectGuidance {
  const { project } = input;

  if (project.status === "archived") {
    return guidance("muted", "只读", "项目已归档", "只读状态，没有待办。");
  }

  if (project.status === "paused" || project.status === "pausing") {
    return guidance(
      "warn",
      "已暂停",
      "恢复项目",
      "项目已暂停；恢复后继续派发任务。",
    );
  }

  // 分析阶段的阻塞来自等待目标、提问、方案确认或无法自动处理的失败。
  if (project.stage === "analysis") {
    if (analysisNeedsManualHandling(project)) {
      return blockedGuidance(project);
    }
    if (projectHasRunningJob(input.jobs, "analysis")) {
      return guidance(
        "info",
        "分析中",
        "等待分析",
        "规划者正在分析项目，完成后会给出可确认的方案。",
      );
    }
    return analysisGuidance(input);
  }

  // 报告就绪时项目会以阻塞状态等待验收，不能按普通阻塞引导。
  if (project.stage === "summarizing") {
    if (project.report) {
      return guidance(
        "success",
        "待验收",
        "验收报告",
        "项目报告已生成，请阅读后通过、提交意见、要求重写或重新规划。",
      );
    }
    return guidance(
      "info",
      "总结中",
      "等待报告",
      "规划者正在撰写项目报告。",
    );
  }

  if (project.stage === "completed") {
    if (project.merged_at) {
      return guidance(
        "success",
        "已交付",
        "已交付",
        `已合入 ${project.base_branch}${mergedCommitLabel(project.merged_commit)}。`,
      );
    }
    if (input.mergedIntoBase) {
      return guidance(
        "success",
        "已交付",
        "已交付",
        `目标分支 ${project.base_branch} 已包含项目分支 ${project.branch}。`,
      );
    }
    return guidance(
      "success",
      "待合入",
      "审阅并合入",
      `改动位于 ${project.branch}，审阅后合入 ${project.base_branch}。`,
    );
  }

  if (project.stage === "executing") {
    if (project.status === "blocked") {
      return blockedGuidance(project);
    }
    return executingGuidance(
      input,
      projectHasRunningJob(input.jobs, "direct"),
    );
  }

  if (project.status === "blocked") {
    return blockedGuidance(project);
  }

  return guidance("muted", "状态未知", "查看项目状态", null);
}

/// 运行中、排队中与待启动的任务分组；已取消、已完成、已暂停与阻塞任务不参与。
export function executionTaskGroups(tasks: Task[]): ExecutionTaskGroups {
  const groups: ExecutionTaskGroups = {
    running: [],
    queued: [],
    starting: [],
  };
  for (const task of tasks) {
    if (task.status === "canceled" || task.stage === "completed") continue;
    if (
      task.status === "paused" ||
      task.status === "pausing" ||
      task.status === "blocked"
    ) {
      continue;
    }
    if (task.status === "queued") {
      groups.queued.push(taskItem(task));
      continue;
    }
    if (
      task.status === "running" ||
      task.stage === "executing" ||
      task.stage === "verifying"
    ) {
      groups.running.push(taskItem(task));
      continue;
    }
    if (task.stage === "pending") {
      groups.starting.push(taskItem(task));
    }
  }
  return groups;
}

export function formatElapsed(milliseconds: number | null): string {
  if (milliseconds === null || milliseconds < 0) return "—";
  const seconds = Math.floor(milliseconds / 1000);
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分钟`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时 ${minutes % 60} 分`;
  const days = Math.floor(hours / 24);
  return `${days} 天 ${hours % 24} 小时`;
}

function hasGoal(goal: string): boolean {
  return goal.trim().length > 0;
}

/// 分析阶段已提交目标、阻塞原因非空且没有规划对话可推进时，需要人工处理，
/// 例如规划作业失败或没有可用的 Agent。
function analysisNeedsManualHandling(project: Project): boolean {
  if (project.status !== "blocked") return false;
  if (!hasGoal(project.goal)) return false;
  return firstLine(project.blocked_reason) !== null;
}

/// 分析阶段待办按目标、方案与规划对话描述下一步。
function analysisGuidance(
  input: ProjectStatusInput,
): ProjectGuidance {
  if (!hasGoal(input.project.goal)) {
    return guidance(
      "warn",
      "待输入",
      "输入项目目标",
      "在项目分析中输入目标、边界和验收预期。",
    );
  }

  const goalMessages = input.messages.filter(
    (message) => message.role !== "review",
  );
  const lastMessage = goalMessages[goalMessages.length - 1];
  const proposal = input.proposal;

  if (proposal) {
    const detail =
      proposal.kind === "direct"
        ? "方案建议直接完成：确认后开始直接完成，或回复规划者继续讨论。"
        : "方案已生成：确认后开始执行，或回复规划者继续讨论。";
    return guidance("success", "待确认", "确认或继续讨论方案", detail);
  }
  if (lastMessage?.role === "planner") {
    return guidance(
      "warn",
      "待回复",
      "回复规划者",
      `规划者在等待你的回复：${firstLine(lastMessage.content, 160) ?? "请查看项目分析中的提问。"}`,
    );
  }
  return guidance(
    "info",
    "分析中",
    "等待分析",
    "目标已提交，等待规划者开始分析。",
  );
}

function executingGuidance(
  input: ProjectStatusInput,
  directRunning: boolean,
): ProjectGuidance {
  const { project } = input;
  if (project.completion_mode === "direct") {
    if (directRunning) {
      return guidance(
        "info",
        "执行中",
        "等待直接完成",
        "规划者正在直接完成实现与自检，完成后进入总结。",
      );
    }
    return guidance(
      "info",
      "执行中",
      "执行中",
      "直接完成作业尚未派发或已结束，等待调度推进。",
    );
  }

  const tasks = executionTaskGroups(input.tasks);
  const count =
    tasks.running.length + tasks.queued.length + tasks.starting.length;
  return guidance(
    "info",
    "执行中",
    "执行中",
    count > 0
      ? "任务会按依赖自动推进；需要时可暂停或调整任务队列。"
      : "当前没有运行、排队或待启动的任务，请检查阻塞原因或重新规划。",
    tasks,
  );
}

function projectHasRunningJob(
  jobs: JobSummary[],
  role: JobSummary["role"],
): boolean {
  return jobs.some(
    (job) => !job.task_id && job.role === role && job.status === "running",
  );
}

function blockedGuidance(project: Project): ProjectGuidance {
  return guidance(
    "danger",
    "需要处理",
    "需要人工处理",
    firstLine(project.blocked_reason) ?? "查看阻塞原因并处理。",
  );
}

function taskItem(task: Task): ExecutionTaskItem {
  return {
    key: task.key,
    stage: task.stage,
    stageLabel: taskStageLabel(task.stage),
  };
}

function mergedCommitLabel(commit: string | null): string {
  return commit ? `（提交 ${commit.slice(0, 7)}）` : "";
}

function firstLine(
  value: string | null | undefined,
  max = 240,
): string | null {
  const line = (value ?? "")
    .split(/\r?\n/)
    .map((item) => item.trim())
    .find((item) => item.length > 0);
  if (!line) return null;
  const chars = Array.from(line);
  if (chars.length <= max) return line;
  return `${chars.slice(0, max - 1).join("")}…`;
}

function guidance(
  tone: GuidanceTone,
  label: string,
  title: string,
  detail: string | null,
  tasks: ExecutionTaskGroups | null = null,
): ProjectGuidance {
  return { tone, label, title, detail, tasks };
}
