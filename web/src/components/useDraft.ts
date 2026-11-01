import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Dispatch, SetStateAction } from "react";
import {
  browserDraftStorage,
  clearDraft,
  draftStorageKey,
  readDraft,
  readDraftJson,
  writeDraft,
  writeDraftJson,
  type DraftScope,
  type DraftStorage,
} from "../presentation/draft";

/// 受控草稿状态：首次挂载从本机草稿恢复，之后每次变化写回。
/// 清空会同时删除存储与跳过下一次写回，避免把刚清空的值又写回草稿。

function scopeKey(scope: DraftScope): string {
  return draftStorageKey(scope);
}

function storageOrNull(): DraftStorage | null {
  return browserDraftStorage();
}

export interface DraftValueState {
  value: string;
  setValue: Dispatch<SetStateAction<string>>;
  clear: (next?: string) => void;
  key: string;
}

export function useDraftValue(
  scope: DraftScope,
  initial: string,
): DraftValueState {
  const key = useMemo(
    () => scopeKey(scope),
    [scope.namespace, scope.projectId, scope.taskId, scope.field],
  );
  const [value, setValue] = useState<string>(() => {
    if (typeof window === "undefined") return initial;
    return readDraft(storageOrNull(), key) ?? initial;
  });
  const skipWrite = useRef(false);
  const valueRef = useRef(value);
  valueRef.current = value;

  useEffect(() => {
    if (skipWrite.current) {
      skipWrite.current = false;
      return;
    }
    writeDraft(storageOrNull(), key, value);
  }, [key, value]);

  const clear = useCallback(
    (next: string = initial) => {
      clearDraft(storageOrNull(), key);
      if (next === valueRef.current) {
        skipWrite.current = false;
        return;
      }
      skipWrite.current = true;
      setValue(next);
    },
    [key, initial],
  );

  return { value, setValue, clear, key };
}

export interface DraftJsonState<T> {
  value: T;
  setValue: Dispatch<SetStateAction<T>>;
  clear: (next: T) => void;
  key: string;
}

/// JSON 形态的草稿状态，用于添加任务表单、Agent 表单等结构化字段。
/// 校验函数决定损坏或旧格式的数据是否可用；不可用时回退到初始值。
export function useDraftJson<T>(
  scope: DraftScope,
  initial: T,
  isValid: (value: unknown) => value is T,
): DraftJsonState<T> {
  const key = useMemo(
    () => scopeKey(scope),
    [scope.namespace, scope.projectId, scope.taskId, scope.field],
  );
  const [value, setValue] = useState<T>(() => {
    if (typeof window === "undefined") return initial;
    return readDraftJson(storageOrNull(), key, isValid) ?? initial;
  });
  const skipWrite = useRef(false);
  const valueRef = useRef(value);
  valueRef.current = value;

  useEffect(() => {
    if (skipWrite.current) {
      skipWrite.current = false;
      return;
    }
    writeDraftJson(storageOrNull(), key, value);
  }, [key, value]);

  const clear = useCallback(
    (next: T) => {
      clearDraft(storageOrNull(), key);
      if (Object.is(next, valueRef.current)) {
        skipWrite.current = false;
        return;
      }
      skipWrite.current = true;
      setValue(next);
    },
    [key],
  );

  return { value, setValue, clear, key };
}
