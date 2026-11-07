import { useEffect, useState } from "react";
import { resourceRequest } from "./ResourcePanel";
import { api, errorMessage } from "../api";
import type { Agent, ProjectDetail, ProjectRolePool } from "../types";
import { Badge, ErrorBox, ROLE_LABELS, formatTime } from "../ui";

interface Advice { limitations: string; items: { agent_id: string; agent_name: string; calls: number; known_cost: number; unknown_cost_calls: number; completed_tasks: number; rejections: number; failed_jobs: number; associated_replans: number; advice: string[] }[] }
export function AgentAdvice({ roles }: { roles?: ProjectRolePool[] }) {
  const [data, setData] = useState<Advice | null>(null);
  const [error, setError] = useState("");
  useEffect(() => { let active = true; resourceRequest<Advice>("/api/agents/recommendations").then(value => { if (active) setData(value); }).catch(err => { if (active) setError(errorMessage(err)); }); return () => { active = false; }; }, []);
  const members = roles ? new Set(roles.flatMap(role => role.entries.map(entry => entry.agent_id))) : null;
  return <details className="agent-advice"><summary>Agent 配置建议与依据（仅供用户决策，不自动修改池）</summary>
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
export function GlobalQueue({ agents = [] }: { agents?: Agent[] }) {
  const [data, setData] = useState<Queue | null>(null);
  const [error, setError] = useState("");
  const [loadError, setLoadError] = useState("");
  const [busy, setBusy] = useState(false);
  const [projects, setProjects] = useState<ProjectDetail[]>([]);
  const [loading, setLoading] = useState(true);
  async function refresh() { setData(await resourceRequest<Queue>("/api/queue")); }
  useEffect(() => { let active = true; const load = () => resourceRequest<Queue>("/api/queue").then(value => { if (active) { setData(value); setLoadError(""); } }).catch(err => { if (active) setLoadError(errorMessage(err)); }).finally(() => { if (active) setLoading(false); }); void load(); const timer = window.setInterval(load, 3000); return () => { active = false; window.clearInterval(timer); }; }, []);
  const projectIds = [...new Set([...(data?.queued ?? []), ...(data?.running ?? [])].map(item => item.project_id))].sort().join(",");
  useEffect(() => {
    let active = true;
    if (projectIds) void Promise.all(projectIds.split(",").map(id => api.getProject(id))).then(value => { if (active) setProjects(value); }).catch(() => { /* 队列可独立读取；名称解析失败不阻断已有操作。 */ });
    return () => { active = false; };
  }, [projectIds]);
  async function action(operation: () => Promise<unknown>) { setBusy(true); setError(""); try { await operation(); await refresh(); } catch (err) { setError(errorMessage(err)); } finally { setBusy(false); } }
  const groups = new Set([...(data?.queued.map(item => item.agent_id) ?? []), ...(data?.running.map(item => item.agent_id) ?? [])]);
  function contextLabel(item: Running | QueueEntry) {
    const detail = projects.find(project => project.project.id === item.project_id);
    const task = detail?.tasks.find(task => task.id === item.task_id);
    return `${detail?.project.name ?? ("project_name" in item ? item.project_name : "项目作业")} · ${task ? `${task.key} ${task.title}` : ROLE_LABELS[item.role] ?? item.role}`;
  }
  return <section className="panel global-queue"><details className="queue-details"><summary><h2>Agent 全局队列</h2><span className="badge-row"><Badge tone="info">运行 {data?.running.length ?? "—"}</Badge><Badge>排队 {data?.queued.length ?? "—"}</Badge></span></summary><p className="muted">所有项目共享队列；按人工顺序派发，不自动按关键路径重排。暂停的项目或任务保留顺序但不占用派发资格。</p>
    {[...groups].map(agent => <section className="queue-agent" key={agent ?? "unassigned"}><h3 title={agent ?? undefined}>{agents.find(item => item.id === agent)?.name ?? (agent ? "Agent（名称暂不可用）" : "等待候选 Agent")}</h3>
      {data?.running.filter(item => item.agent_id === agent).map(item => <div className="queue-entry" key={item.id}>
        <div className="queue-entry-title"><Badge tone="info">运行中</Badge><a title={item.id} href={`/projects/${item.project_id}${item.task_id ? `?tab=tasks&task=${item.task_id}` : `?tab=info&job=${item.id}`}`}>{contextLabel(item)}</a><span className="muted">{ROLE_LABELS[item.role] ?? (item.role === "merge_conflict" ? "合入冲突处理" : item.role)}</span></div>
        <div className="button-row">{item.role === "merge_conflict" ? <button className="danger" disabled={busy} onClick={() => {
          if (window.confirm("停止冲突处理将终止进程并保留文件现场。继续？")) void action(() => resourceRequest(`/api/projects/${item.project_id}/merge/assist`, "DELETE"));
        }}>停止冲突处理</button> : <>
          <button className="secondary" disabled={busy} onClick={() => void action(() => resourceRequest(item.task_id ? `/api/projects/${item.project_id}/tasks/${item.task_id}/pause` : `/api/projects/${item.project_id}/pause`, "POST", {}))}>暂停后续派发</button>
          <button className="danger" disabled={busy} onClick={() => { if (window.confirm("强制暂停会终止在途进程，不撤销已经修改的文件。继续？")) void action(() => resourceRequest(item.task_id ? `/api/projects/${item.project_id}/tasks/${item.task_id}/pause` : `/api/projects/${item.project_id}/pause`, "POST", { force: true })); }}>强制暂停</button>
        </>}</div>
      </div>)}
      {data?.queued.filter(item => item.agent_id === agent).map((item, index, all) => <div className="queue-entry" key={item.id}>
        <div className="queue-entry-title"><Badge tone="warn">排队 {index + 1}</Badge><a href={`/projects/${item.project_id}${item.task_id ? `?tab=tasks&task=${item.task_id}` : ""}`}>{contextLabel(item)}</a><span className="muted">{ROLE_LABELS[item.role] ?? item.role} · {item.forced ? "由人工指定" : "按角色优先级"}</span></div>
        <p className="muted">{item.reason ?? "等待派发"} · 影响后继 {item.affected_tasks} · {item.blocks_milestone ? "阻挡里程碑" : "影响项目总结"} · 入队 {formatTime(item.created_at)}</p>
        <div className="button-row"><button className="secondary" disabled={busy || index === 0} onClick={() => void action(() => resourceRequest(`/api/queue/${item.id}`, "PATCH", { position: index - 1 }))}>上移</button>
        <button className="secondary" disabled={busy || index === all.length - 1} onClick={() => void action(() => resourceRequest(`/api/queue/${item.id}`, "PATCH", { position: index + 1 }))}>下移</button></div>
      </div>)}
    </section>)}
    {!groups.size ? <p className="muted">{loading ? "正在读取队列…" : loadError ? "队列暂不可用。" : "暂无运行或排队作业。"}</p> : null}</details>{loadError ? <ErrorBox message={loadError} onRetry={() => void refresh().then(() => setLoadError("")).catch(err => setLoadError(errorMessage(err)))} /> : null}{error ? <ErrorBox message={error} /> : null}
  </section>;
}
