import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { browserDraftStorage, writeDraft } from "../presentation/draft";
import { Markdown } from "./markdown";

/**
 * 统一的受控文本输入框。
 *
 * 值完全由父组件提供，组件只维护“编辑/预览”视图与实测溢出状态。
 * 预览使用站内 Markdown 渲染器；内容超过自动增高上限后输入框内部滚动，
 * 操作区固定在底部，长文本时发送、保存按钮仍然可见。
 * 传入 draftKey 时，每次输入同步写入本机 localStorage；恢复由 useDraft 负责。
 *
 * 人工核对清单（项目分析输入框）：
 * 1. 在项目分析输入框输入文字，刷新页面，文字仍在。
 * 2. 输入文字后离开项目页再后退/前进，文字仍在。
 * 3. 点击“发送”成功提交后，输入框清空，草稿键被删除；再次刷新仍为空。
 * 4. 切换到“预览”，Markdown 标题、列表与代码块按对应标签渲染。
 * 5. 输入长文本，超过上限后输入框内部滚动，底部按钮保持可见。
 */
export interface InputBoxProps {
  value: string;
  onChange: (value: string) => void;
  /// 本机草稿键；只做写入，恢复由父组件用 useDraft 完成。
  draftKey?: string;
  placeholder?: string;
  disabled?: boolean;
  readOnly?: boolean;
  rows?: number;
  /// 超过该字符数即视为超限，使用内部滚动并展示提示。
  maxAutoGrowChars?: number;
  /// 展示为 count/max 并交给浏览器限制输入。
  maxLength?: number;
  /// 首次渲染的视图；默认编辑。
  defaultMode?: "edit" | "preview";
  label?: string;
  hint?: ReactNode;
  /// 固定在底部的操作区，例如发送、保存按钮。
  actions?: ReactNode;
  className?: string;
  ariaInvalid?: boolean;
  onKeyDown?: (event: KeyboardEvent<HTMLTextAreaElement>) => void;
}

const DEFAULT_AUTO_GROW_CHARS = 480;
const MAX_TEXTAREA_HEIGHT = 260;

function characterCount(value: string): number {
  return Array.from(value).length;
}

export function InputBox({
  value,
  onChange,
  draftKey,
  placeholder,
  disabled = false,
  readOnly = false,
  rows = 3,
  maxAutoGrowChars = DEFAULT_AUTO_GROW_CHARS,
  maxLength,
  defaultMode = "edit",
  label,
  hint,
  actions,
  className,
  ariaInvalid,
  onKeyDown,
}: InputBoxProps) {
  const [mode, setMode] = useState<"edit" | "preview">(defaultMode);
  const [measuredOverflow, setMeasuredOverflow] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const countId = useId();
  const hintId = useId();
  const preview = mode === "preview";
  const count = useMemo(() => characterCount(value), [value]);
  const overLimit = count > maxAutoGrowChars || measuredOverflow;
  const countText = maxLength ? `${count}/${maxLength} 字符` : `${count} 字符`;
  const classes = [
    "input-box",
    preview ? "input-box--preview" : "input-box--edit",
    overLimit ? "input-box--scroll" : "",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");
  const describedBy = hint ? `${countId} ${hintId}` : countId;

  function handleChange(next: string) {
    onChange(next);
    if (draftKey) writeDraft(browserDraftStorage(), draftKey, next);
  }

  /// 编辑态按内容高度自动增高，达到上限后交给内部滚动；预览态不测量。
  useEffect(() => {
    if (preview) return;
    const area = textareaRef.current;
    if (!area) return;
    area.style.height = "auto";
    const overflow = area.scrollHeight > MAX_TEXTAREA_HEIGHT;
    area.style.height = `${Math.min(area.scrollHeight, MAX_TEXTAREA_HEIGHT)}px`;
    setMeasuredOverflow(overflow);
  }, [value, preview, rows]);

  return (
    <div className={classes} data-mode={mode}>
      <div className="input-box-toolbar">
        <div className="input-box-modes" role="group" aria-label="输入视图">
          <button
            type="button"
            className={mode === "edit" ? "active" : undefined}
            aria-pressed={mode === "edit"}
            onClick={() => setMode("edit")}
          >
            编辑
          </button>
          <button
            type="button"
            className={mode === "preview" ? "active" : undefined}
            aria-pressed={mode === "preview"}
            onClick={() => setMode("preview")}
          >
            预览
          </button>
        </div>
        <span
          id={countId}
          className={
            overLimit ? "input-box-count input-box-count-warn" : "input-box-count"
          }
          aria-live="polite"
        >
          {countText}
          {overLimit ? " · 超限，内部滚动" : ""}
        </span>
      </div>
      {preview ? (
        <div className="input-box-preview" role="region" aria-label="Markdown 预览">
          {value.trim() ? (
            <Markdown source={value} />
          ) : (
            <p className="muted">还没有内容，切回“编辑”开始输入。</p>
          )}
        </div>
      ) : (
        <textarea
          ref={textareaRef}
          value={value}
          rows={rows}
          disabled={disabled}
          readOnly={readOnly}
          maxLength={maxLength}
          placeholder={placeholder}
          aria-label={label}
          aria-invalid={ariaInvalid ? true : undefined}
          aria-describedby={describedBy}
          onChange={(event) => handleChange(event.target.value)}
          onKeyDown={onKeyDown}
        />
      )}
      {hint ? (
        <div className="input-box-hint" id={hintId}>
          {hint}
        </div>
      ) : null}
      {actions ? <div className="input-box-actions">{actions}</div> : null}
    </div>
  );
}
