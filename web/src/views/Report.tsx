import { useEffect, useMemo, type ReactNode } from 'react';
import { useDomo } from '../store';
import type { ReportStatus } from '../types';
import { Badge, Modal, fmtTime } from '../ui';

const STATUS_META: Record<ReportStatus, { label: string; tone: string }> = {
  pending: { label: '排队中', tone: 'gray' },
  generating: { label: '生成中', tone: 'amber' },
  ready: { label: '已生成', tone: 'green' },
  failed: { label: '生成失败', tone: 'red' },
};

/** 行内格式：**加粗** 与 `代码`（报告由后端拼装，无需完整 Markdown 语法） */
function renderInline(text: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  const re = /(\*\*[^*]+\*\*|`[^`]+`)/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let k = 0;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) nodes.push(text.slice(last, m.index));
    const tok = m[0];
    if (tok.startsWith('**')) nodes.push(<strong key={k++}>{tok.slice(2, -2)}</strong>);
    else nodes.push(<code key={k++}>{tok.slice(1, -1)}</code>);
    last = m.index + tok.length;
  }
  if (last < text.length) nodes.push(text.slice(last));
  return nodes;
}

function isSeparatorRow(cells: string[]): boolean {
  return cells.length > 0 && cells.every(c => /^:?-{2,}:?$/.test(c.trim()));
}

/** 极简 Markdown 渲染：标题 / 列表 / 表格 / 引用 / 分隔线 + 行内加粗与代码 */
function renderMarkdown(md: string): ReactNode {
  const lines = md.replace(/\r\n/g, '\n').split('\n');
  const out: ReactNode[] = [];
  let list: ReactNode[] = [];
  let table: string[][] = [];
  let quote: string[] = [];
  let key = 0;

  const flushList = () => {
    if (list.length) {
      out.push(
        <ul key={`ul-${key++}`} className="report-list">
          {list}
        </ul>,
      );
      list = [];
    }
  };
  const flushTable = () => {
    if (table.length) {
      const rows = table.filter(r => !isSeparatorRow(r));
      const [head, ...body] = rows;
      out.push(
        <table key={`tb-${key++}`} className="report-table">
          {head && (
            <thead>
              <tr>
                {head.map((c, i) => (
                  <th key={i}>{renderInline(c)}</th>
                ))}
              </tr>
            </thead>
          )}
          <tbody>
            {body.map((r, ri) => (
              <tr key={ri}>
                {r.map((c, ci) => (
                  <td key={ci}>{renderInline(c)}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>,
      );
      table = [];
    }
  };
  const flushQuote = () => {
    if (quote.length) {
      out.push(
        <blockquote key={`bq-${key++}`} className="report-quote">
          {quote.map((q, i) => (
            <p key={i}>{renderInline(q)}</p>
          ))}
        </blockquote>,
      );
      quote = [];
    }
  };
  const flushAll = () => {
    flushList();
    flushTable();
    flushQuote();
  };

  for (const raw of lines) {
    const line = raw.replace(/\s+$/, '');
    const trimmed = line.trim();
    if (!trimmed) {
      flushAll();
      continue;
    }
    if (trimmed.startsWith('|')) {
      flushList();
      flushQuote();
      table.push(
        trimmed
          .replace(/^\|/, '')
          .replace(/\|$/, '')
          .split('|')
          .map(c => c.trim()),
      );
      continue;
    }
    if (trimmed.startsWith('>')) {
      flushList();
      flushTable();
      quote.push(trimmed.replace(/^>\s?/, ''));
      continue;
    }
    const heading = /^(#{1,6})\s+(.*)$/.exec(trimmed);
    if (heading) {
      flushAll();
      const level = heading[1].length;
      const text = renderInline(heading[2]);
      if (level <= 1) out.push(<h1 key={`h-${key++}`} className="report-h1">{text}</h1>);
      else if (level === 2) out.push(<h2 key={`h-${key++}`} className="report-h2">{text}</h2>);
      else out.push(<h3 key={`h-${key++}`} className="report-h3">{text}</h3>);
      continue;
    }
    if (/^---+$/.test(trimmed)) {
      flushAll();
      out.push(<hr key={`hr-${key++}`} className="report-hr" />);
      continue;
    }
    const item = /^(\s*)-\s+(.*)$/.exec(raw);
    if (item) {
      flushTable();
      flushQuote();
      const depth = Math.floor(item[1].length / 2);
      list.push(
        <li key={`li-${key++}`} className={depth > 0 ? 'report-li lvl' : 'report-li'}>
          {renderInline(item[2])}
        </li>,
      );
      continue;
    }
    // 普通段落
    flushAll();
    out.push(
      <p key={`p-${key++}`} className="report-p">
        {renderInline(trimmed)}
      </p>,
    );
  }
  flushAll();
  return <>{out}</>;
}

/**
 * M3 项目总结报告查看弹窗。
 *
 * - 打开时按需拉取详情（GET /projects/:id/report）；
 * - pending/generating 显示生成中；failed 显示错误 + 「重试生成」（POST）；
 * - ready 渲染 Markdown 正文；SSE `report` 事件实时刷新状态。
 */
export function ReportModal({
  projectId,
  projectName,
  onClose,
}: {
  projectId: string;
  projectName: string;
  onClose: () => void;
}) {
  const { reports, loadReport, generateReport } = useDomo();
  const report = reports[projectId];

  useEffect(() => {
    void loadReport(projectId);
  }, [projectId, loadReport]);

  const body = useMemo(() => {
    if (!report) {
      return (
        <div className="report-state">
          <span className="spinner" /> 正在加载报告…
        </div>
      );
    }
    const meta = STATUS_META[report.status] ?? STATUS_META.pending;
    if (report.status === 'pending' || report.status === 'generating') {
      return (
        <div className="report-state">
          <span className="spinner" />
          <span>
            报告{meta.label}…（planner 正在汇总事件与里程碑，通常会话可能需要数十秒）
          </span>
        </div>
      );
    }
    if (report.status === 'failed') {
      return (
        <div className="report-state report-failed">
          <p className="report-error-title">⚠ 报告生成失败（已记录到 data/domo.log）</p>
          <p className="report-error-msg">{report.error || '未知错误'}</p>
          <p className="muted">
            已尝试 {report.attempts} 次。修复 planner agent 配置后点击「重试生成」，或在后端 API{' '}
            <code>POST /api/projects/{projectId}/report</code> 重试。
          </p>
          <button className="btn btn-primary" onClick={() => void generateReport(projectId)}>
            ↻ 重试生成
          </button>
        </div>
      );
    }
    // ready
    return (
      <div className="report-doc">{renderMarkdown(report.content ?? '（报告正文为空）')}</div>
    );
  }, [report, projectId, generateReport]);

  const statusMeta = report ? STATUS_META[report.status] ?? STATUS_META.pending : null;

  return (
    <Modal wide className="modal-report" title={`项目总结报告 · ${projectName}`} onClose={onClose}>
      <div className="report-toolbar">
        {statusMeta && <Badge tone={statusMeta.tone}>{statusMeta.label}</Badge>}
        {report?.generated_at ? (
          <span className="muted">生成于 {fmtTime(report.generated_at)}</span>
        ) : null}
        {report?.attempts ? <span className="muted">尝试 {report.attempts} 次</span> : null}
        {report?.path ? (
          <span className="muted report-path" title={report.path}>
            {report.path}
          </span>
        ) : null}
        <span className="report-toolbar-spacer" />
        <button
          className="btn btn-sm"
          onClick={() => void generateReport(projectId)}
          disabled={report?.status === 'pending' || report?.status === 'generating'}
          title="重新汇总并生成报告（可用于失败重试）"
        >
          {report?.status === 'failed' ? '↻ 重试生成' : '↻ 重新生成'}
        </button>
      </div>
      {body}
    </Modal>
  );
}
