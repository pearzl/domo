import { useEffect, useState } from 'react';
import { api } from '../api';
import type { PoolView } from '../types';
import { ROLE_LABEL } from '../ui';

const ROLES = ['executor', 'acceptor', 'planner'] as const;

/**
 * M4.5 项目 agent 池表格：
 * - 行 = agent，列 = 角色
 * - agent 未配置该角色 → `-`（不可更新）
 * - 右键单元格：切换该角色启用/禁用（禁用显示 ✕）
 * - 左键数字：优先级 +1（数字 −1），同列其余自动重排，数字恒为 0..n 连续
 * - 每次变更立即持久化到项目池（生成/重新规划时也随请求提交）
 */
export function PoolTable({ projectId }: { projectId: string }) {
  const [view, setView] = useState<PoolView | null>(null);
  const [err, setErr] = useState('');
  const [busy, setBusy] = useState(false);

  async function load() {
    try {
      setView(await api.getProjectPool(projectId));
    } catch (e) {
      setErr(String(e));
    }
  }
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  if (!view) return <p className="muted">加载 agent 池…</p>;

  /** 当前池（角色 → 有序 agent id）；未配置过的角色按 agent 登记顺序补全 */
  const pool: Record<string, string[]> = { ...view.pool };
  for (const role of ROLES) {
    if (!pool[role]) {
      pool[role] = view.agents
        .filter(a => a.roles[role])
        .map(a => a.id);
    }
  }

  async function save(next: Record<string, string[]>) {
    setBusy(true);
    try {
      setView(await api.putProjectPool(projectId, next));
      setErr('');
    } catch (e) {
      setErr(String(e));
    }
    setBusy(false);
  }

  function toggle(role: string, agentId: string) {
    const list = [...(pool[role] ?? [])];
    const i = list.indexOf(agentId);
    if (i >= 0) list.splice(i, 1);
    else list.push(agentId);
    void save({ ...pool, [role]: list });
  }

  function bump(role: string, agentId: string) {
    const list = [...(pool[role] ?? [])];
    const i = list.indexOf(agentId);
    if (i <= 0) return; // 已是最小优先级
    [list[i - 1], list[i]] = [list[i], list[i - 1]];
    void save({ ...pool, [role]: list });
  }

  const circuit = (agentId: string) =>
    view.circuits.find(c => c.agent_id === agentId);

  return (
    <div>
      <div className="label-text">
        项目 agent 池（按角色优先级：数字越小越优先；右键启用/禁用，左键点数字提升优先级）
      </div>
      <table className="table pool-table">
        <thead>
          <tr>
            <th>Agent</th>
            {ROLES.map(r => (
              <th key={r}>{ROLE_LABEL[r] ?? r}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {view.agents.map(a => (
            <tr key={a.id}>
              <td>
                <span>{a.name}</span>
                <span className="mono muted id-hint"> {a.adapter}</span>
                {a.status === 'busy' && <span className="badge badge-amber"> 忙碌</span>}
                {circuit(a.id) && (
                  <span className="badge badge-red" title={`限流/配置错误熔断中（剩余 ${Math.round((circuit(a.id)!.remaining_ms || 0) / 1000)}s）`}>
                    熔断 {Math.round((circuit(a.id)!.remaining_ms || 0) / 1000)}s
                  </span>
                )}
              </td>
              {ROLES.map(role => {
                const hasRole = !!a.roles[role];
                if (!hasRole) {
                  return (
                    <td key={role} className="muted pool-cell">
                      -
                    </td>
                  );
                }
                const idx = (pool[role] ?? []).indexOf(a.id);
                const enabled = idx >= 0;
                return (
                  <td
                    key={role}
                    className="pool-cell"
                    onContextMenu={e => {
                      e.preventDefault();
                      if (!busy) toggle(role, a.id);
                    }}
                    onClick={() => {
                      if (enabled && !busy) bump(role, a.id);
                    }}
                    title={enabled ? '左键：提升优先级（数字 −1）；右键：禁用' : '右键：启用'}
                  >
                    {enabled ? <span className="pool-pri">{idx}</span> : <span className="pool-off">✕</span>}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
      <p className="muted" style={{ fontSize: 12 }}>
        建议：执行者与验收者尽量使用不同实例（同一实例兼任多角色会降低独立性）；被限流/配置错误的 agent
        会自动临时熔断（全局生效），熔断期间任务按优先级使用下一个 agent。
      </p>
      {err && <div className="form-error">{err}</div>}
    </div>
  );
}
