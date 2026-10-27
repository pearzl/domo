import type { ReactNode } from "react";
import { Markdown, looksLikeMarkdown } from "./markdown";

const MARKDOWN_KEY = /(^|[_-])(markdown|md)$/i;

function isMarkdownKey(name: string | null): boolean {
  return name !== null && MARKDOWN_KEY.test(name);
}

function stringValue(name: string | null, value: string): ReactNode {
  if (isMarkdownKey(name) || looksLikeMarkdown(value)) {
    return (
      <div className="json-row json-row-block">
        {name !== null ? <span className="json-key">{name}</span> : null}
        <div className="json-markdown">
          <Markdown source={value} />
        </div>
      </div>
    );
  }
  return (
    <div className="json-row">
      {name !== null ? <span className="json-key">{name}</span> : null}
      <span className="json-string">{JSON.stringify(value)}</span>
    </div>
  );
}

function literalValue(
  name: string | null,
  value: number | boolean | null,
): ReactNode {
  const className =
    value === null ? "json-null" : typeof value === "number" ? "json-number" : "json-boolean";
  return (
    <div className="json-row">
      {name !== null ? <span className="json-key">{name}</span> : null}
      <span className={className}>{String(value)}</span>
    </div>
  );
}

function JsonNode({
  name,
  value,
  depth,
}: {
  name: string | null;
  value: unknown;
  depth: number;
}): ReactNode {
  if (value === null || value === undefined) return literalValue(name, null);
  if (typeof value === "string") return stringValue(name, value);
  if (typeof value === "number" || typeof value === "boolean") {
    return literalValue(name, value);
  }

  const isArray = Array.isArray(value);
  const entries: Array<[string, unknown]> = isArray
    ? (value as unknown[]).map((item, index) => [String(index), item])
    : Object.entries(value as Record<string, unknown>);
  const summary = isArray ? `[${entries.length}]` : `{${entries.length}}`;

  return (
    <details className="json-node" open={depth < 2}>
      <summary className="json-summary">
        {name !== null ? <span className="json-key">{name}</span> : null}
        <span className="json-bracket">{summary}</span>
      </summary>
      <div className="json-children">
        {entries.length === 0 ? (
          <span className="json-empty">空</span>
        ) : (
          entries.map(([key, item]) => (
            <JsonNode key={key} name={key} value={item} depth={depth + 1} />
          ))
        )}
      </div>
    </details>
  );
}

/** Renders JSON objects and arrays as nested key/value rows. */
export function JsonView({
  value,
  className,
}: {
  value: unknown;
  className?: string;
}) {
  return (
    <div className={`json-view${className ? ` ${className}` : ""}`}>
      <JsonNode name={null} value={value} depth={0} />
    </div>
  );
}
