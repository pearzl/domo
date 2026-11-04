import { useEffect, useState } from "react";
import { resourceRequest } from "./ResourcePanel";
import { errorMessage } from "../api";
import type { ProjectRolePool } from "../types";

interface Advice { limitations: string; items: { agent_id: string; agent_name: string; calls: number; known_cost: number; unknown_cost_calls: number; completed_tasks: number; rejections: number; failed_jobs: number; associated_replans: number; advice: string[] }[] }
export function AgentAdvice({ roles }: { roles?: ProjectRolePool[] }) {
  const [data, setData] = useState<Advice | null>(null);
  const [error, setError] = useState("");
  useEffect(() => { let active = true; resourceRequest<Advice>("/api/agents/recommendations").then(value => { if (active) setData(value); }).catch(err => { if (active) setError(errorMessage(err)); }); return () => { active = false; }; }, []);
  const members = roles ? new Set(roles.flatMap(role => role.entries.map(entry => entry.agent_id))) : null;
  return <details className="panel"><summary>Agent 配置建议与依据（仅供用户决策，不自动修改池）</summary>
    <p>{data?.limitations}</p>
    {data?.items.filter(item => !members || members.has(item.agent_id)).map(item => <details key={item.agent_id}>
      <summary>{item.agent_name} · 完成关联任务 {item.completed_tasks} · 验收拒绝 {item.rejections}</summary>
      <p>全部调用 {item.calls} · 已知费用 {item.known_cost.toFixed(4)} · 未知费用 {item.unknown_cost_calls} · 失败作业 {item.failed_jobs} · 关联重规划 {item.associated_replans}</p>
      <ul>{item.advice.map((text, index) => <li key={index}>{text}</li>)}</ul>
    </details>)}{error ? <p className="error-box">{error}</p> : null}
  </details>;
}

interface QueueEntry { id: string; project_id: string; project_name: string; task_id: string | null; role: string; agent_id: string | null; reason: string | null; forced: boolean; affected_tasks: number; blocks_milestone: boolean; created_at: string }
interface Running { id: string; project_id: string; task_id: string | null; role: string; agent_id: string | null }
interface Queue { queued: QueueEntry[]; running: Running[] }
export function GlobalQueue() {
  const [data, setData] = useState<Queue | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  async function refresh() { setData(await resourceRequest<Queue>("/api/queue")); }
  useEffect(() => { let active = true; const load = () => resourceRequest<Queue>("/api/queue").then(value => { if (active) setData(value); }).catch(err => { if (active) setError(errorMessage(err)); }); void load(); const timer = window.setInterval(load, 3000); return () => { active = false; window.clearInterval(timer); }; }, []);
  async function action(operation: () => Promise<unknown>) { setBusy(true); setError(""); try { await operation(); await refresh(); } catch (err) { setError(errorMessage(err)); } finally { setBusy(false); } }
  const groups = new Set([...(data?.queued.map(item => item.agent_id) ?? []), ...(data?.running.map(item => item.agent_id) ?? [])]);
  return <section className="panel"><h2>Agent 全局队列</h2><p className="muted">所有项目共享队列；按人工顺序派发，不自动按关键路径重排。暂停的项目或任务保留顺序但不占用派发资格。</p>
    {[...groups].map(agent => <details key={agent ?? "unassigned"} open><summary>{agent ?? "等待候选 Agent"}</summary>
      {data?.running.filter(item => item.agent_id === agent).map(item => <div key={item.id}>
        运行中 <a href={`/projects/${item.project_id}?tab=info&job=${item.id}`}>{item.id}</a> · {item.role}
        {item.role === "merge_conflict" ? <button disabled={busy} onClick={() => {
          if (window.confirm("停止冲突处理将终止进程并保留文件现场。继续？")) void action(() => resourceRequest(`/api/projects/${item.project_id}/merge/assist`, "DELETE"));
        }}>停止冲突处理</button> : <>
          <button disabled={busy} onClick={() => void action(() => resourceRequest(item.task_id ? `/api/projects/${item.project_id}/tasks/${item.task_id}/pause` : `/api/projects/${item.project_id}/pause`, "POST", {}))}>暂停后续派发</button>
          <button disabled={busy} onClick={() => { if (window.confirm("强制暂停会终止在途进程，不撤销已经修改的文件。继续？")) void action(() => resourceRequest(item.task_id ? `/api/projects/${item.project_id}/tasks/${item.task_id}/pause` : `/api/projects/${item.project_id}/pause`, "POST", { force: true })); }}>强制暂停</button>
        </>}
      </div>)}
      {data?.queued.filter(item => item.agent_id === agent).map((item, index, all) => <div key={item.id}>
        <a href={`/projects/${item.project_id}${item.task_id ? `?tab=tasks&task=${item.task_id}` : ""}`}>{item.project_name}</a> · {item.role} · {item.forced ? "由人工指定" : "按角色优先级"}
        <p>{item.reason ?? "等待派发"} · 影响后继 {item.affected_tasks} · {item.blocks_milestone ? "阻挡里程碑" : "影响项目总结"} · 入队 {item.created_at}</p>
        <button disabled={busy || index === 0} onClick={() => void action(() => resourceRequest(`/api/queue/${item.id}`, "PATCH", { position: index - 1 }))}>上移</button>
        <button disabled={busy || index === all.length - 1} onClick={() => void action(() => resourceRequest(`/api/queue/${item.id}`, "PATCH", { position: index + 1 }))}>下移</button>
      </div>)}
    </details>)}
    {!groups.size ? <p>暂无运行或排队作业。</p> : null}{error ? <p className="error-box">{error}</p> : null}
  </section>;
}
