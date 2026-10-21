import { useCallback, useEffect, useState } from 'react';
import { useDomo } from '../store';
import { api } from '../api';
import type { UsageBucket, UsageSummary } from '../types';
import { Empty } from '../ui';

function fmtInt(n: number | undefined | null): string {
  return (n ?? 0).toLocaleString('en-US');
}
function fmtCost(n: number | undefined | null): string {
  const v = n ?? 0;
  return v > 0 ? `$${v.toFixed(4)}` : '—';
}
function fmtDuration(ms: number | undefined | null): string {
  const s = Math.round((ms ?? 0) / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return `${m}m ${s % 60}s`;
}

function BucketTable({ title, rows }: { title: string; rows: UsageBucket[] }) {
  return (
    <section className="panel">
      <h2 className="panel-title">{title}</h2>
      {rows.length === 0 ? (
        <Empty text="暂无用量。" />
      ) : (
        <div className="table-wrap">
          <table className="table">
            <thead>
              <tr><th>名称</th><th>调用</th><th>输入 tokens</th><th>输出 tokens</th><th>费用</th><th>时长</th></tr>
            </thead>
            <tbody>
              {rows.map(r => (
                <tr key={r.key}>
                  <td>{r.key}</td>
                  <td>{fmtInt(r.calls)}</td>
                  <td>{fmtInt(r.input_tokens)}</td>
                  <td>{fmtInt(r.output_tokens)}</td>
                  <td>{fmtCost(r.cost_usd)}</td>
                  <td>{fmtDuration(r.duration_ms)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

export function UsageView() {
  const { selectedProjectId, projects } = useDomo();
  const [scope, setScope] = useState<string>(selectedProjectId ?? '');
  const [data, setData] = useState<UsageSummary | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    setBusy(true);
    setError('');
    try {
      setData(await api.getUsage(scope || undefined));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [scope]);

  useEffect(() => { void load(); }, [load]);

  return (
    <div className="stack">
      <div className="toolbar">
        <span className="muted">统计范围</span>
        <select value={scope} onChange={e => setScope(e.target.value)} className="select">
          <option value="">全部项目</option>
          {projects.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
        </select>
        <span className="spacer" />
        <button className="btn" onClick={() => void load()} disabled={busy}>{busy ? '刷新中…' : '刷新'}</button>
      </div>
      {error && <div className="form-error">{error}</div>}
      {!data ? (
        busy ? <div className="muted">加载中…</div> : <Empty text="暂无用量数据。" />
      ) : (
        <>
          <div className="usage-cards">
            <div className="usage-card"><span className="muted">调用次数</span><strong>{fmtInt(data.totals.calls)}</strong></div>
            <div className="usage-card"><span className="muted">输入 tokens</span><strong>{fmtInt(data.totals.input_tokens)}</strong></div>
            <div className="usage-card"><span className="muted">输出 tokens</span><strong>{fmtInt(data.totals.output_tokens)}</strong></div>
            <div className="usage-card"><span className="muted">费用</span><strong>{fmtCost(data.totals.cost_usd)}</strong></div>
            <div className="usage-card"><span className="muted">时长</span><strong>{fmtDuration(data.totals.duration_ms)}</strong></div>
          </div>
          <p className="muted" style={{ margin: 0 }}>
            {data.note}
            {data.budget_usd != null && (
              <> · 预算 ${data.budget_usd.toFixed(2)} {data.over_budget ? '已超限' : '未超限'}</>
            )}
          </p>
          {data.over_budget && <div className="form-error">成本已超过预算上限；请检查 agent 用量或调整 DOMO_BUDGET_USD。</div>}
          <BucketTable title="按项目" rows={data.by_project} />
          <BucketTable title="按 Agent" rows={data.by_agent} />
          <BucketTable title="按任务" rows={data.by_task} />
        </>
      )}
    </div>
  );
}
