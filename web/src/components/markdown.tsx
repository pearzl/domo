import { Fragment, useMemo } from "react";
import type { ReactNode } from "react";

/**
 * A deliberately small Markdown renderer. Every piece of source text becomes a
 * React text node, so raw HTML in the source is shown as text rather than
 * interpreted. Links are limited to safe schemes and open in a new context.
 */

const INLINE_PATTERN =
  /\[([^\]\n]+)\]\(([^)\s]+)(?:\s+"[^"]*")?\)|`([^`\n]+)`|\*\*([^*\n]+)\*\*|__([^_\n]+)__|~~([^~\n]+)~~|(?<![\w*])\*([^*\n]+)\*(?!\*)|(?<![\w])_([^_\n]+)_(?![\w])|(https?:\/\/[^\s<>()]+)/g;

const LIST_ITEM = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;
const FENCE = /^\s*(```+|~~~+)\s*([\w#+.-]*)\s*$/;
const FENCE_CLOSE = /^\s*(`{3,}|~{3,})\s*$/;
const HEADING = /^\s*(#{1,6})\s+(.*?)\s*#*\s*$/;
const BLOCKQUOTE = /^\s*>\s?(.*)$/;
const HR = /^\s*(?:(\*\s*){3,}|(-\s*){3,}|(_\s*){3,})$/;
const TABLE_SEPARATOR = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)+\|?\s*$/;

/** Returns a URL only when it cannot run script in the current document. */
export function safeHref(raw: string): string | null {
  const value = raw.trim();
  if (!value) return null;
  if (value.startsWith("//")) return null;
  if (/^(https?:|mailto:)/i.test(value)) return value;
  if (/^[a-z][a-z0-9+.-]*:/i.test(value)) return null;
  return value;
}

function renderInline(source: string, prefix: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let cursor = 0;
  let index = 0;

  function pushText(text: string) {
    if (text) nodes.push(text);
  }

  for (const match of source.matchAll(INLINE_PATTERN)) {
    const start = match.index ?? 0;
    pushText(source.slice(cursor, start));
    cursor = start + match[0].length;
    const key = `${prefix}-inline-${index}`;
    index += 1;

    if (match[1] !== undefined && match[2] !== undefined) {
      const href = safeHref(match[2]);
      if (href) {
        nodes.push(
          <a
            key={key}
            href={href}
            target="_blank"
            rel="noopener noreferrer"
            className="rich-link"
          >
            {renderInline(match[1], key)}
          </a>,
        );
      } else {
        nodes.push(match[0]);
      }
    } else if (match[3] !== undefined) {
      nodes.push(
        <code key={key} className="rich-inline-code">
          {match[3]}
        </code>,
      );
    } else if (match[4] !== undefined) {
      nodes.push(<strong key={key}>{renderInline(match[4], key)}</strong>);
    } else if (match[5] !== undefined) {
      nodes.push(<strong key={key}>{renderInline(match[5], key)}</strong>);
    } else if (match[6] !== undefined) {
      nodes.push(<del key={key}>{renderInline(match[6], key)}</del>);
    } else if (match[7] !== undefined) {
      nodes.push(<em key={key}>{renderInline(match[7], key)}</em>);
    } else if (match[8] !== undefined) {
      nodes.push(<em key={key}>{renderInline(match[8], key)}</em>);
    } else if (match[9] !== undefined) {
      nodes.push(
        <a
          key={key}
          href={match[9]}
          target="_blank"
          rel="noopener noreferrer"
          className="rich-link"
        >
          {match[9]}
        </a>,
      );
    }
  }
  pushText(source.slice(cursor));
  return nodes;
}

function renderParagraph(lines: string[], prefix: string): ReactNode {
  return (
    <p className="rich-paragraph" key={prefix}>
      {lines.map((line, lineIndex) => (
        <Fragment key={`${prefix}-line-${lineIndex}`}>
          {lineIndex > 0 ? <br /> : null}
          {renderInline(line, `${prefix}-line-${lineIndex}`)}
        </Fragment>
      ))}
    </p>
  );
}

function isTableStart(lines: string[], index: number): boolean {
  return (
    lines[index].includes("|") &&
    index + 1 < lines.length &&
    TABLE_SEPARATOR.test(lines[index + 1])
  );
}

function startsBlock(lines: string[], index: number): boolean {
  if (index >= lines.length) return true;
  const line = lines[index];
  if (!line.trim()) return true;
  if (FENCE.test(line) || HEADING.test(line) || HR.test(line)) return true;
  if (BLOCKQUOTE.test(line) || LIST_ITEM.test(line)) return true;
  return isTableStart(lines, index);
}

function splitTableRow(line: string): string[] {
  let text = line.trim();
  if (text.startsWith("|")) text = text.slice(1);
  if (text.endsWith("|")) text = text.slice(0, -1);
  return text.split("|").map((cell) => cell.trim());
}

function renderTable(
  lines: string[],
  start: number,
  prefix: string,
): { node: ReactNode; next: number } {
  const header = splitTableRow(lines[start]);
  const rows: string[][] = [];
  let index = start + 2;
  while (index < lines.length && lines[index].trim() && lines[index].includes("|")) {
    rows.push(splitTableRow(lines[index]));
    index += 1;
  }
  return {
    node: (
      <div className="rich-table-wrap" key={prefix}>
        <table className="rich-table">
          <thead>
            <tr>
              {header.map((cell, cellIndex) => (
                <th key={`${prefix}-th-${cellIndex}`}>
                  {renderInline(cell, `${prefix}-th-${cellIndex}`)}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, rowIndex) => (
              <tr key={`${prefix}-tr-${rowIndex}`}>
                {header.map((_, cellIndex) => (
                  <td key={`${prefix}-td-${rowIndex}-${cellIndex}`}>
                    {renderInline(row[cellIndex] ?? "", `${prefix}-td-${rowIndex}-${cellIndex}`)}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    ),
    next: index,
  };
}

interface ListItemData {
  text: string[];
  children: string[];
}

function renderList(
  lines: string[],
  start: number,
  prefix: string,
): { node: ReactNode; next: number } | null {
  const first = LIST_ITEM.exec(lines[start]);
  if (!first) return null;
  const baseIndent = first[1].length;
  const ordered = /^\d/.test(first[2]);
  const items: ListItemData[] = [];
  let current: ListItemData | null = null;
  let index = start;

  while (index < lines.length) {
    const line = lines[index];
    if (!line.trim()) {
      let lookahead = index + 1;
      while (lookahead < lines.length && !lines[lookahead].trim()) lookahead += 1;
      if (lookahead >= lines.length) break;
      const nextIndent = lines[lookahead].match(/^\s*/)?.[0].length ?? 0;
      if (nextIndent > baseIndent) {
        index = lookahead;
        continue;
      }
      break;
    }
    const indent = line.match(/^\s*/)?.[0].length ?? 0;
    const item = LIST_ITEM.exec(line);
    if (item && indent === baseIndent) {
      if (/^\d/.test(item[2]) !== ordered) break;
      current = { text: [item[3]], children: [] };
      items.push(current);
      index += 1;
      continue;
    }
    if (indent > baseIndent && current) {
      current.children.push(line.slice(baseIndent + 2));
      index += 1;
      continue;
    }
    break;
  }

  if (items.length === 0) return null;

  const rendered = items.map((item, itemIndex) => {
    const key = `${prefix}-li-${itemIndex}`;
    const children = item.children.length
      ? parseBlocks(item.children, `${key}-children`)
      : [];
    return (
      <li key={key}>
        {renderInline(item.text.join(" "), key)}
        {children.length ? <div className="rich-list-children">{children}</div> : null}
      </li>
    );
  });

  return {
    node: ordered ? (
      <ol className="rich-list" key={prefix}>
        {rendered}
      </ol>
    ) : (
      <ul className="rich-list" key={prefix}>
        {rendered}
      </ul>
    ),
    next: index,
  };
}

function parseBlocks(lines: string[], prefix: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let index = 0;
  let block = 0;

  while (index < lines.length) {
    const line = lines[index];
    if (!line.trim()) {
      index += 1;
      continue;
    }
    const key = `${prefix}-block-${block}`;
    block += 1;

    const fence = FENCE.exec(line);
    if (fence) {
      const marker = fence[1][0];
      const language = fence[2];
      const code: string[] = [];
      index += 1;
      while (index < lines.length) {
        const closing = FENCE_CLOSE.exec(lines[index]);
        if (closing && closing[1][0] === marker) {
          index += 1;
          break;
        }
        code.push(lines[index]);
        index += 1;
      }
      nodes.push(
        <div className="rich-code-block" key={key}>
          {language ? <div className="rich-code-lang">{language}</div> : null}
          <pre className="rich-code">
            <code>{code.join("\n")}</code>
          </pre>
        </div>,
      );
      continue;
    }

    const heading = HEADING.exec(line);
    if (heading) {
      const level = heading[1].length;
      const Tag = `h${level}` as "h1" | "h2" | "h3" | "h4" | "h5" | "h6";
      nodes.push(
        <Tag className={`rich-heading rich-heading-${level}`} key={key}>
          {renderInline(heading[2], key)}
        </Tag>,
      );
      index += 1;
      continue;
    }

    if (HR.test(line)) {
      nodes.push(<hr className="rich-hr" key={key} />);
      index += 1;
      continue;
    }

    if (BLOCKQUOTE.test(line)) {
      const quoteLines: string[] = [];
      while (index < lines.length) {
        const quote = BLOCKQUOTE.exec(lines[index]);
        if (!quote) break;
        quoteLines.push(quote[1]);
        index += 1;
      }
      nodes.push(
        <blockquote className="rich-quote" key={key}>
          {parseBlocks(quoteLines, `${key}-quote`)}
        </blockquote>,
      );
      continue;
    }

    if (isTableStart(lines, index)) {
      const table = renderTable(lines, index, key);
      nodes.push(table.node);
      index = table.next;
      continue;
    }

    if (LIST_ITEM.test(line)) {
      const list = renderList(lines, index, key);
      if (list) {
        nodes.push(list.node);
        index = list.next;
        continue;
      }
    }

    const paragraph: string[] = [line];
    index += 1;
    while (index < lines.length && !startsBlock(lines, index)) {
      paragraph.push(lines[index]);
      index += 1;
    }
    nodes.push(renderParagraph(paragraph, key));
  }

  return nodes;
}

/** True when the source clearly uses Markdown block or inline syntax. */
export function looksLikeMarkdown(source: string): boolean {
  if (
    /^(#{1,6}\s|```|~~~)/m.test(source) ||
    /^\s*([-*+]|\d+[.)])\s+/m.test(source) ||
    /^\s*>\s?/m.test(source) ||
    /^\s*\|.+\|\s*$/m.test(source)
  ) {
    return true;
  }
  return (
    /\*\*[^*\n]+\*\*/.test(source) ||
    /(?<!`)[`][^`\n]+[`](?!`)/.test(source) ||
    /\[[^\]\n]+\]\([^)\s]+\)/.test(source) ||
    /~~[^~\n]+~~/.test(source)
  );
}

export function Markdown({
  source,
  className,
}: {
  source: string;
  className?: string;
}) {
  const nodes = useMemo(
    () => parseBlocks(source.replace(/\r\n?/g, "\n").split("\n"), "md"),
    [source],
  );
  return <div className={`rich-markdown${className ? ` ${className}` : ""}`}>{nodes}</div>;
}
