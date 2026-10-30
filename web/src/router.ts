/// 应用内链接与查询参数的构造与解析。
/// 页面之间只通过这些纯函数互相跳转，路径与参数名集中维护。

export const USAGE_PATH = "/usage";
export const USAGE_PROJECT_PARAM = "project_id";

/// 用量页地址：未指定项目时返回不带参数的 /usage，指定时附带项目过滤。
export function usagePath(projectId?: string | null): string {
  const id = typeof projectId === "string" ? projectId.trim() : "";
  if (!id) return USAGE_PATH;
  const search = new URLSearchParams();
  search.set(USAGE_PROJECT_PARAM, id);
  return `${USAGE_PATH}?${search.toString()}`;
}

/// 把查询串拆成键值对；缺失的 "=" 与空段按非法查询处理，编码无法解码时返回 null。
type QueryEntry = [string, string];

function parseQuery(search: string): QueryEntry[] | null {
  const raw = search.startsWith("?") ? search.slice(1) : search;
  if (!raw) return [];
  const entries: QueryEntry[] = [];
  for (const part of raw.split("&")) {
    if (!part) continue;
    const index = part.indexOf("=");
    if (index < 0) return null;
    const key = part.slice(0, index);
    const value = part.slice(index + 1);
    if (!key) return null;
    try {
      entries.push([
        decodeComponent(key),
        decodeComponent(value),
      ]);
    } catch {
      return null;
    }
  }
  return entries;
}

/// 表单编码中 "+" 表示空格，与 URLSearchParams 的语义一致。
function decodeComponent(raw: string): string {
  return decodeURIComponent(raw.replace(/\+/g, " "));
}

/// 从 location.search 解析项目过滤；缺省、空串或非法查询都返回 null。
/// 同名参数取首个非空值，空串不会被当成项目 id。
export function readUsageProjectId(search: string): string | null {
  if (typeof search !== "string") return null;
  const entries = parseQuery(search);
  if (!entries) return null;
  for (const [key, value] of entries) {
    if (key !== USAGE_PROJECT_PARAM) continue;
    const id = value.trim();
    if (id) return id;
  }
  return null;
}
