import { useEffect, useMemo, useRef, useState } from "react";
import { JsonView } from "./JsonView";
import { Markdown, looksLikeMarkdown } from "./markdown";

type ContentFormat = "auto" | "markdown" | "text";
type ViewMode = "rendered" | "raw";
type ContentKind = "json" | "markdown" | "text";

const KIND_LABELS: Record<ContentKind, string> = {
  json: "JSON",
  markdown: "Markdown",
  text: "文本",
};

function analyze(
  source: string,
  format: ContentFormat,
): { kind: ContentKind; json: unknown } | null {
  const trimmed = source.trim();
  if (!trimmed) return null;
  if (format !== "text" && (trimmed.startsWith("{") || trimmed.startsWith("["))) {
    try {
      const parsed: unknown = JSON.parse(trimmed);
      if (parsed !== null && typeof parsed === "object") {
        return { kind: "json", json: parsed };
      }
    } catch {
      // Not JSON; fall through to Markdown or plain text.
    }
  }
  if (format === "markdown" || (format === "auto" && looksLikeMarkdown(source))) {
    return { kind: "markdown", json: null };
  }
  return { kind: "text", json: null };
}

async function writeClipboard(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Fall back to the selection based copy path below.
  }
  try {
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    const copied = document.execCommand("copy");
    document.body.removeChild(area);
    return copied;
  } catch {
    return false;
  }
}

/**
 * Shows raw content in a rendered view by default and keeps a per-instance
 * switch to the exact original text. Toggling one block never changes another.
 */
export function RichContent({
  content,
  className,
  format = "auto",
  emptyText,
}: {
  content: string | null | undefined;
  className?: string;
  format?: ContentFormat;
  emptyText?: string;
}) {
  const source = content ?? "";
  const [mode, setMode] = useState<ViewMode>("rendered");
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  const analysis = useMemo(() => analyze(source, format), [source, format]);

  useEffect(() => {
    return () => {
      if (timer.current !== undefined) window.clearTimeout(timer.current);
    };
  }, []);

  if (!analysis) {
    if (emptyText === undefined) return null;
    return <div className={`rich-content-empty muted ${className ?? ""}`}>{emptyText}</div>;
  }

  async function copy() {
    const ok = await writeClipboard(source);
    if (!ok) return;
    setCopied(true);
    if (timer.current !== undefined) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(false), 1600);
  }

  return (
    <div
      className={`rich-content rich-content-${analysis.kind}${className ? ` ${className}` : ""}`}
    >
      <div className="rich-content-bar">
        <span className="rich-content-kind">{KIND_LABELS[analysis.kind]}</span>
        <span className="rich-content-actions">
          <button
            type="button"
            className="ghost rich-content-toggle"
            aria-pressed={mode === "raw"}
            onClick={() => setMode((current) => (current === "raw" ? "rendered" : "raw"))}
          >
            {mode === "raw" ? "渲染视图" : "查看原文"}
          </button>
          <button type="button" className="ghost rich-content-copy" onClick={() => void copy()}>
            {copied ? "已复制" : "复制"}
          </button>
        </span>
      </div>
      <div className="rich-content-body">
        {mode === "raw" ? (
          <pre className="rich-content-raw">{source}</pre>
        ) : analysis.kind === "json" ? (
          <JsonView value={analysis.json} />
        ) : analysis.kind === "markdown" ? (
          <Markdown source={source} />
        ) : (
          <pre className="rich-content-raw rich-content-plain">{source}</pre>
        )}
      </div>
    </div>
  );
}
