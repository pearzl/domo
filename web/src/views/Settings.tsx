import { useCallback, useEffect, useState } from 'react';
import { api } from '../api';
import { useDomo } from '../store';
import type { BackupInfo, SystemInfo } from '../types';
import { Badge, Empty, fmtTime } from '../ui';

type Theme = 'dark' | 'light';
function applyTheme(t: Theme) {
  document.documentElement.dataset.theme = t;
  localStorage.setItem('domo_theme', t);
}

export function SettingsView() {
  const { engineerings, setError } = useDomo();
  const [theme, setTheme] = useState<Theme>(() => (localStorage.getItem('domo_theme') as Theme) || 'dark');
  const [sys, setSys] = useState<SystemInfo | null>(null);
  const [backups, setBackups] = useState<BackupInfo[]>([]);
  const [dir, setDir] = useState('');
  const [engineeringId, setEngineeringId] = useState('');
  const [includeWs, setIncludeWs] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');

  const load = useCallback(async () => {
    try {
      const [b, s] = await Promise.all([api.listBackups(), api.systemInfo()]);
      setBackups(b.backups);
      setDir(b.dir);
      setSys(s);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [setError]);

  useEffect(() => {
    applyTheme(theme);
  }, [theme]);
  useEffect(() => { void load(); }, [load]);

  async function create() {
    setBusy(true);
    setMessage('');
    try {
      const info = await api.createBackup({ engineering_id: engineeringId || null, include_workspace: includeWs });
      setMessage(`已创建备份 ${info.name}`);
      await load();
    } catch (e) {
      setMessage(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function restore(name: string) {
    if (!window.confirm(`确定请求从备份 ${name} 恢复吗？\n恢复将在下次启动服务时执行；当前数据库会先做一次安全备份。`)) return;
    setBusy(true);
    setMessage('');
    try {
      await api.restoreBackup(name);
      setMessage('已登记恢复请求：请重启 domo-server 后生效。');
    } catch (e) {
      setMessage(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function remove(name: string) {
    if (!window.confirm(`删除备份 ${name}？`)) return;
    setBusy(true);
    try {
      await api.deleteBackup(name);
      await load();
    } catch (e) {
      setMessage(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="stack">
      <section className="panel">
        <h2 className="panel-title">外观与平台</h2>
        <div className="settings-row">
          <span>主题</span>
          <div className="seg">
            <button className={`seg-btn ${theme === 'dark' ? 'active' : ''}`} onClick={() => setTheme('dark')}>深色</button>
            <button className={`seg-btn ${theme === 'light' ? 'active' : ''}`} onClick={() => setTheme('light')}>浅色</button>
          </div>
        </div>
        {sys && (
          <div className="meta-grid" style={{ marginTop: 10 }}>
            <span className="muted">平台</span><span>{sys.os} / {sys.arch}{sys.wsl ? ' · WSL' : ''}</span>
            <span className="muted">数据目录</span><span className="mono">{sys.data_dir}</span>
            <span className="muted">备份目录</span><span className="mono">{sys.backup_dir}</span>
            <span className="muted">版本</span><span>{sys.version}</span>
          </div>
        )}
      </section>

      <section className="panel">
        <h2 className="panel-title">备份与恢复</h2>
        <div className="settings-row" style={{ flexWrap: 'wrap' }}>
          <select value={engineeringId} onChange={e => setEngineeringId(e.target.value)} className="select">
            <option value="">仅数据库</option>
            {engineerings.map(e => <option key={e.id} value={e.id}>含工程工作区：{e.name}</option>)}
          </select>
          <label className="muted" style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <input type="checkbox" checked={includeWs} onChange={e => setIncludeWs(e.target.checked)} disabled={!engineeringId} />
            打包工作区（tar.gz，可能较大）
          </label>
          <button className="btn btn-primary" disabled={busy} onClick={() => void create()}>创建备份</button>
          <button className="btn" disabled={busy} onClick={() => void load()}>刷新</button>
        </div>
        <p className="muted" style={{ margin: '8px 0 0' }}>
          备份目录：<code>{dir}</code>。恢复不会在运行中替换数据库：登记后需重启服务，启动时先备份当前库再恢复；可选解包工作区。
        </p>
        {message && <div className="risk-line" style={{ marginTop: 8 }}>{message}</div>}
        {backups.length === 0 ? <Empty text="还没有备份。" /> : (
          <div className="table-wrap" style={{ marginTop: 10 }}>
            <table className="table">
              <thead><tr><th>名称</th><th>时间</th><th>工程</th><th>工作区</th><th>大小</th><th>操作</th></tr></thead>
              <tbody>
                {backups.map(b => (
                  <tr key={b.name}>
                    <td className="mono">{b.name}</td>
                    <td>{fmtTime(b.created_at)}</td>
                    <td>{b.engineering_name ?? '—'}</td>
                    <td>{b.workspace ? <Badge tone="purple">含工作区</Badge> : <span className="muted">数据库</span>}</td>
                    <td>{Math.max(1, Math.round(b.size / 1024))} KB</td>
                    <td>
                      <button className="btn btn-sm" disabled={busy} onClick={() => void restore(b.name)}>恢复</button>{' '}
                      <button className="btn btn-sm btn-danger-ghost" disabled={busy} onClick={() => void remove(b.name)}>删除</button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}
