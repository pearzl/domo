/// 添加任务表单的草稿结构与持久化校验。
/// 草稿只保存在浏览器本地，读取时按字段类型校验，避免旧版本残留的结构进入表单。

export interface AddTaskDraft {
  key: string;
  title: string;
  description: string;
  acceptance: string;
  deps: string[];
  milestone: boolean;
}

export const EMPTY_TASK_DRAFT: AddTaskDraft = {
  key: "",
  title: "",
  description: "",
  acceptance: "",
  deps: [],
  milestone: false,
};

export function isAddTaskDraft(value: unknown): value is AddTaskDraft {
  if (!value || typeof value !== "object") return false;
  const record = value as Record<string, unknown>;
  return (
    typeof record.key === "string" &&
    typeof record.title === "string" &&
    typeof record.description === "string" &&
    typeof record.acceptance === "string" &&
    Array.isArray(record.deps) &&
    record.deps.every((item) => typeof item === "string") &&
    typeof record.milestone === "boolean"
  );
}
