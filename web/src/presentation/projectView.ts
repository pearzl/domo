/// 项目页视图状态与 URL 查询参数之间的纯转换。
/// 选项卡与选中的任务都写在查询串里，刷新、复制链接与浏览器前进后退都能回到同一视图。

export const PROJECT_TAB_PARAM = "tab";
export const PROJECT_TASK_PARAM = "task";

export interface ProjectViewState {
  tab: string | null;
  taskId: string | null;
}

function entries(search: string): [string, string][] {
  const raw = search.startsWith("?") ? search.slice(1) : search;
  if (!raw) return [];
  const result: [string, string][] = [];
  for (const part of raw.split("&")) {
    if (!part) continue;
    const index = part.indexOf("=");
    if (index < 0) continue;
    try {
      result.push([
        decodeURIComponent(part.slice(0, index).replace(/\+/g, " ")),
        decodeURIComponent(part.slice(index + 1).replace(/\+/g, " ")),
      ]);
    } catch {
      // 单个非法片段不影响其余参数。
    }
  }
  return result;
}

/// 读取首个非空的参数值；同名参数取第一个有效值。
function readValue(search: string, name: string): string | null {
  for (const [key, value] of entries(search)) {
    if (key !== name) continue;
    const trimmed = value.trim();
    if (trimmed) return trimmed;
  }
  return null;
}

/// 解析 URL 中的选项卡与任务；非法编码、缺失或空值都按未指定处理。
export function readProjectView(
  search: string,
  validTabs: readonly string[] = [],
): ProjectViewState {
  const tab = readValue(search, PROJECT_TAB_PARAM);
  const taskId = readValue(search, PROJECT_TASK_PARAM);
  return {
    tab: tab && (validTabs.length === 0 || validTabs.includes(tab)) ? tab : null,
    taskId,
  };
}

/// 生成新的查询串，保留无关参数与顺序，只替换选项卡与任务。
/// 值为 null 或空串时删除该参数。
export function updateProjectView(
  search: string,
  patch: Partial<ProjectViewState>,
): string {
  const kept = entries(search).filter(([key]) => {
    /// 未提供的字段原样保留，只有显式给出 null 或空串时才删除。
    if (key === PROJECT_TAB_PARAM) return patch.tab === undefined;
    if (key === PROJECT_TASK_PARAM) return patch.taskId === undefined;
    return true;
  });
  if (patch.tab) kept.push([PROJECT_TAB_PARAM, patch.tab]);
  if (patch.taskId) kept.push([PROJECT_TASK_PARAM, patch.taskId]);
  if (kept.length === 0) return "";
  return `?${kept
    .map(([key, value]) => `${encodeURIComponent(key)}=${encodeURIComponent(value)}`)
    .join("&")}`;
}

/// 项目页地址：保留路径，替换查询串。
export function projectViewHref(pathname: string, search: string): string {
  return `${pathname}${search}`;
}
