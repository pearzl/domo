import { useEffect, useRef, useState } from "react";
import { writeClipboard } from "../presentation/clipboard";

/// 行内复制按钮：复制成功后短暂显示“已复制”，不改变所在区块的展开状态。
export function CopyButton({
  value,
  label,
  className,
}: {
  value: string;
  label: string;
  className?: string;
}) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => {
    return () => {
      if (timer.current !== undefined) window.clearTimeout(timer.current);
    };
  }, []);

  async function copy() {
    const ok = await writeClipboard(value);
    if (!ok) return;
    setCopied(true);
    if (timer.current !== undefined) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(false), 1600);
  }

  return (
    <button
      type="button"
      className={`ghost copy-button${className ? ` ${className}` : ""}`}
      aria-label={`复制${label}`}
      title={`复制${label}`}
      onClick={() => void copy()}
    >
      {copied ? "已复制" : "复制"}
    </button>
  );
}
