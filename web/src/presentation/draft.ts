/// 本机草稿的存储键、编码、容错读取与过期清理。
/// 草稿只保存在当前浏览器的 localStorage，不进入后端，也不在设备之间同步。
/// 所有导出函数都不抛错：隐私模式、配额耗尽、存储被禁用或数据损坏时静默降级。

/// 草稿键的统一前缀。清理时只处理带此前缀的键，不碰其他站点数据。
export const DRAFT_NAMESPACE = "domo.draft.v1";

/// 单条草稿的最大字符数。超过上限的内容不写入，读取时也视为无效。
export const DRAFT_MAX_LENGTH = 20000;

/// 草稿默认保留 7 天；更早的数据在读取与清理时都视为过期。
export const DRAFT_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000;

const ENVELOPE_VERSION = 1;

export interface DraftScope {
  /// 项目 id；项目之外的页面可以省略。
  projectId?: string | null;
  /// 任务 id；同一项目里按任务区分的草稿使用。
  taskId?: string | null;
  /// 字段名，调用方保证同一作用域内唯一。
  field: string;
  /// 可选命名空间，默认使用 DRAFT_NAMESPACE。
  namespace?: string;
}

/// localStorage 的最小接口；测试与降级实现都不依赖浏览器全局对象。
export interface DraftStorage {
  readonly length: number;
  key(index: number): string | null;
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

interface DraftEnvelope {
  version: number;
  savedAt: number;
  value: string;
}

export interface DraftReadOptions {
  now?: number;
  maxAgeMs?: number;
  maxLength?: number;
}

function encodePart(value: string): string {
  return encodeURIComponent(value);
}

/// 生成稳定的草稿键：namespace + 项目 + 任务 + 字段。
/// 各段独立编码，避免 id 中的分隔符与字段名混淆。
export function draftStorageKey(scope: DraftScope): string {
  const field = scope.field.trim();
  if (!field) throw new RangeError("草稿字段名不能为空");
  const namespace = (scope.namespace?.trim() || DRAFT_NAMESPACE).replace(/\s+/g, "-");
  const parts = [namespace];
  const projectId = scope.projectId?.trim();
  if (projectId) parts.push("project", encodePart(projectId));
  const taskId = scope.taskId?.trim();
  if (taskId) parts.push("task", encodePart(taskId));
  parts.push("field", encodePart(field));
  return parts.join(":");
}

/// 浏览器存储句柄；存储被禁用或读取 localStorage 本身抛错时返回 null。
export function browserDraftStorage(): DraftStorage | null {
  try {
    if (typeof window === "undefined" || !window.localStorage) return null;
    return window.localStorage;
  } catch {
    return null;
  }
}

function safeGet(storage: DraftStorage | null, key: string): string | null {
  if (!storage) return null;
  try {
    return storage.getItem(key);
  } catch {
    return null;
  }
}

function safeSet(storage: DraftStorage | null, key: string, value: string): boolean {
  if (!storage) return false;
  try {
    storage.setItem(key, value);
    return true;
  } catch {
    return false;
  }
}

function safeRemove(storage: DraftStorage | null, key: string): boolean {
  if (!storage) return false;
  try {
    storage.removeItem(key);
    return true;
  } catch {
    return false;
  }
}

/// 解析外壳；格式错误、版本不符、时间异常都返回 null。
function parseEnvelope(raw: string | null): DraftEnvelope | null {
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object") return null;
  const record = parsed as Record<string, unknown>;
  if (record.version !== ENVELOPE_VERSION) return null;
  if (typeof record.value !== "string") return null;
  if (typeof record.savedAt !== "number" || !Number.isFinite(record.savedAt)) {
    return null;
  }
  if (record.savedAt <= 0) return null;
  return {
    version: ENVELOPE_VERSION,
    savedAt: record.savedAt,
    value: record.value,
  };
}

function ageLimit(options: DraftReadOptions): number {
  const maxAge = options.maxAgeMs ?? DRAFT_MAX_AGE_MS;
  return Number.isFinite(maxAge) && maxAge >= 0 ? maxAge : DRAFT_MAX_AGE_MS;
}

function lengthLimit(options: DraftReadOptions): number {
  const maxLength = options.maxLength ?? DRAFT_MAX_LENGTH;
  return Number.isFinite(maxLength) && maxLength >= 0 ? maxLength : DRAFT_MAX_LENGTH;
}

/// 读取草稿字符串。非法 JSON、结构不符、超长与过期数据都返回 null。
export function readDraft(
  storage: DraftStorage | null,
  key: string,
  options: DraftReadOptions = {},
): string | null {
  const raw = safeGet(storage, key);
  if (raw === null) return null;
  const envelope = parseEnvelope(raw);
  if (!envelope) return null;
  const now = options.now ?? Date.now();
  if (now - envelope.savedAt > ageLimit(options)) return null;
  if (envelope.value.length > lengthLimit(options)) return null;
  return envelope.value;
}

/// 写入草稿。空字符串表示清空；超长内容不写入并移除旧值，避免旧草稿继续生效。
export function writeDraft(
  storage: DraftStorage | null,
  key: string,
  value: string,
  now = Date.now(),
): boolean {
  if (!value) return safeRemove(storage, key);
  if (value.length > DRAFT_MAX_LENGTH) {
    safeRemove(storage, key);
    return false;
  }
  const envelope: DraftEnvelope = {
    version: ENVELOPE_VERSION,
    savedAt: now,
    value,
  };
  return safeSet(storage, key, JSON.stringify(envelope));
}

/// 清空单条草稿；键不存在或存储不可用时同样静默返回。
export function clearDraft(storage: DraftStorage | null, key: string): void {
  safeRemove(storage, key);
}

/// 读取 JSON 草稿；内容不是合法 JSON 或校验函数拒绝时返回 null。
export function readDraftJson<T>(
  storage: DraftStorage | null,
  key: string,
  isValid: (value: unknown) => value is T,
  options: DraftReadOptions = {},
): T | null {
  const raw = readDraft(storage, key, options);
  if (raw === null) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  return isValid(parsed) ? parsed : null;
}

/// 写入 JSON 草稿；序列化失败或超长时静默返回 false。
export function writeDraftJson(
  storage: DraftStorage | null,
  key: string,
  value: unknown,
  now = Date.now(),
): boolean {
  let encoded: string;
  try {
    encoded = JSON.stringify(value);
  } catch {
    return false;
  }
  if (typeof encoded !== "string") return false;
  return writeDraft(storage, key, encoded, now);
}

/// 清理本应用命名空间下的过期、损坏与超长草稿，返回移除数量。
/// 其他命名空间与不相关键一律不动。
export function pruneDrafts(
  storage: DraftStorage | null,
  options: DraftReadOptions = {},
): number {
  if (!storage) return 0;
  const prefix = DRAFT_NAMESPACE;
  const keys: string[] = [];
  try {
    for (let index = 0; index < storage.length; index += 1) {
      const key = storage.key(index);
      if (key && key.startsWith(prefix)) keys.push(key);
    }
  } catch {
    return 0;
  }
  const now = options.now ?? Date.now();
  let removed = 0;
  for (const key of keys) {
    const raw = safeGet(storage, key);
    const envelope = parseEnvelope(raw);
    const invalid =
      !envelope ||
      now - envelope.savedAt > ageLimit(options) ||
      envelope.value.length > lengthLimit(options);
    if (invalid && safeRemove(storage, key)) removed += 1;
  }
  return removed;
}
