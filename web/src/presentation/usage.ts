import type {
  Agent,
  ProjectSummary,
  UsageGroup,
  UsageReport,
  UsageTotals,
} from "../types";
import { ROLE_LABELS } from "../ui";

/// 用量页的分组维度；页面一次只展示一个维度的一张表。
export type UsageDimension = "project" | "agent" | "role";

export const USAGE_DIMENSIONS: UsageDimension[] = ["project", "agent", "role"];

export const USAGE_DIMENSION_LABELS: Record<UsageDimension, string> = {
  project: "按项目",
  agent: "按 Agent",
  role: "按角色",
};

export const USAGE_DIMENSION_HINTS: Record<UsageDimension, string> = {
  project: "每个项目内所有作业的用量合计。",
  agent: "每个 Agent 承担的作业用量；未记录 Agent 的作业单独归入未知分组。",
  role: "按作业角色汇总，便于对比项目分析、直接完成、执行、验收与总结的成本。",
};

/// 表格首列表头随维度变化，名称列始终说明分组依据。
export const USAGE_NAME_HEADERS: Record<UsageDimension, string> = {
  project: "项目",
  agent: "Agent",
  role: "角色",
};

export const USAGE_ROLE_FILTERS = [
  "analysis",
  "direct",
  "executor",
  "verifier",
  "summary",
];

/// 用量页筛选条件；空串表示不限制。
export interface UsageFilters {
  project_id: string;
  agent_id: string;
  role: string;
  from: string;
  to: string;
}

export const EMPTY_USAGE_FILTERS: UsageFilters = {
  project_id: "",
  agent_id: "",
  role: "",
  from: "",
  to: "",
};

/// 报告请求参数；筛选条件与维度一起决定请求，切换维度不改变筛选。
export function usageRequestParams(
  filters: UsageFilters,
  dimension: UsageDimension,
): {
  project_id?: string;
  agent_id?: string;
  role?: string;
  from?: string;
  to?: string;
  group_by: UsageDimension;
} {
  return {
    project_id: filters.project_id || undefined,
    agent_id: filters.agent_id || undefined,
    role: filters.role || undefined,
    from: toIso(filters.from),
    to: toIso(filters.to),
    group_by: dimension,
  };
}

/// 本地时间输入转 ISO；空值或无法解析时视为不限制。
function toIso(value: string): string | undefined {
  if (!value) return undefined;
  const time = new Date(value).getTime();
  if (Number.isNaN(time)) return undefined;
  return new Date(time).toISOString();
}

/// 分组显示名：项目名、Agent 名或角色标签，找不到时回退到标识。
export function usageGroupName(
  dimension: UsageDimension,
  group: UsageGroup,
  projects: ProjectSummary[],
  agents: Agent[],
): string {
  if (dimension === "project") {
    return (
      projects.find((project) => project.id === group.project_id)?.name ??
      group.project_id ??
      "未记录项目"
    );
  }
  if (dimension === "agent") {
    if (!group.agent_id) return "未记录 Agent";
    return (
      group.agent_name ??
      agents.find((agent) => agent.id === group.agent_id)?.name ??
      group.agent_id
    );
  }
  return group.role ? ROLE_LABELS[group.role] ?? group.role : "未记录角色";
}


/// 只保留当前维度对应的一份报告；切换维度时不会同时渲染多张表。
export function currentUsageReport(
  dimension: UsageDimension,
  report: UsageReport | null,
): UsageReport | null {
  if (!report) return null;
  return report.group_by === dimension ? report : null;
}

/// 项目维度下拉的选项：项目列表项在前，当前选中但不在列表中的项目 id 追加在后。
/// 这样地址栏或项目页带来的项目过滤在列表加载前、加载后都能显示为已选中。
export function usageProjectOptions(
  projectId: string,
  projects: ProjectSummary[],
): { value: string; label: string }[] {
  const id = projectId.trim();
  const options = projects.map((project) => ({
    value: project.id,
    label: project.name,
  }));
  if (id && !options.some((option) => option.value === id)) {
    options.push({ value: id, label: `${id}（未在项目列表中）` });
  }
  return options;
}

/// 项目过滤后的汇总与分组行：报告按项目过滤时汇总只含该项目；按 Agent 或角色
/// 分组时分组行不带项目归属，无法逐行收窄，沿用报告的汇总。
export function restrictUsageToProject(
  report: UsageReport | null,
  projectId: string,
): UsageReport | null {
  const id = projectId.trim();
  if (!report || !id || report.group_by !== "project") return report;
  const groups = report.groups.filter((group) => group.project_id === id);
  return { ...report, groups, totals: usageTotalsOf(groups) };
}

/// 分组行的用量合计：作业数、时长与未知作业直接相加，token 与费用只累加已记录的部分。
function usageTotalsOf(groups: UsageGroup[]): UsageTotals {
  return groups.reduce<UsageTotals>(
    (sum, group) => ({
      jobs: sum.jobs + group.jobs,
      input_tokens: sum.input_tokens + group.input_tokens,
      output_tokens: sum.output_tokens + group.output_tokens,
      cache_read_tokens: sum.cache_read_tokens + group.cache_read_tokens,
      cache_write_tokens: sum.cache_write_tokens + group.cache_write_tokens,
      reasoning_tokens: sum.reasoning_tokens + group.reasoning_tokens,
      cost: sum.cost + group.cost,
      duration_ms: sum.duration_ms + group.duration_ms,
      known_jobs: sum.known_jobs + group.known_jobs,
      unknown_jobs: sum.unknown_jobs + group.unknown_jobs,
    }),
    {
      jobs: 0,
      input_tokens: 0,
      output_tokens: 0,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
      reasoning_tokens: 0,
      cost: 0,
      duration_ms: 0,
      known_jobs: 0,
      unknown_jobs: 0,
    },
  );
}
