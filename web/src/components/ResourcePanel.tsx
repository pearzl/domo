import { useEffect, useState } from "react";
import type { ProjectDetail } from "../types";
import { errorMessage } from "../api";
import { RichContent } from "./RichContent";

export async function resourceRequest<T>(url: string, method = "GET", body?: unknown): Promise<T> {
  const response = await fetch(url, { method, headers: { "Content-Type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error ?? `请求失败 ${response.status}`);
  return result as T;
}
interface SharedRecord { id: string; topic: string; body: string; scope: string[]; evidence: string[]; supersedes: string[]; conflicts: string[]; reason: string; task_id: string | null; job_id: string | null; created_at: string }
interface Shared { version: string; repository: string; records: SharedRecord[]; superseded_ids: string[] }
interface Artifact { id: string; path: string; kind: string; format: string; verification: string; task_id: string | null; commit: string | null }
interface Artifacts { root: string; items: Artifact[]; check: { interrupted: boolean; reason: string | null } | null; permissions: { task_id: string; path: string; reason: string }[] }
interface Preview { path: string; text: string; commit: string | null; versions: string[] }
interface MergeStatus { request: { agent_id: string; state: string; job_id: string | null; error: string | null } | null }

/** Project resources have independent versions and never masquerade as a contract edit. */
export function ResourcePanel({ detail, onSaved }: { detail: ProjectDetail; onSaved: () => void }) {
  const base = `/api/projects/${detail.project.id}`;
  const [shared, setShared] = useState<Shared | null>(null);
  const [artifacts, setArtifacts] = useState<Artifacts | null>(null);
  const [merge, setMerge] = useState<MergeStatus | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [topic, setTopic] = useState("");
  const [version, setVersion] = useState("");
  const [selectedVersion, setSelectedVersion] = useState("");
  const [taskFilter, setTaskFilter] = useState("");
  const [kindFilter, setKindFilter] = useState("");
  const [preview, setPreview] = useState<Preview | null>(null);
  const [selectedArtifact, setSelectedArtifact] = useState("");
  const [path, setPath] = useState("");
  const [kind, setKind] = useState("delivery");
  const [format, setFormat] = useState("");
  const [verification, setVerification] = useState("");
  const [task, setTask] = useState("");
  const [reason, setReason] = useState("");
  const [agent, setAgent] = useState("");
  const readonly = detail.project.status === "archived";
  async function refresh() {
    const [s, a, m] = await Promise.all([resourceRequest<Shared>(`${base}/shared${selectedVersion ? `?version=${encodeURIComponent(selectedVersion)}` : ""}`), resourceRequest<Artifacts>(`${base}/artifacts`), resourceRequest<MergeStatus>(`${base}/merge/assist`)]);
    setShared(s); setArtifacts(a); setMerge(m);
  }
  useEffect(() => { let active = true; void refresh().catch(err => { if (active) setError(errorMessage(err)); }); return () => { active = false; }; }, [base, detail.project.updated_at]);
  async function action(operation: () => Promise<unknown>) {
    setBusy(true); setError("");
    try { await operation(); await refresh(); onSaved(); } catch (err) { setError(errorMessage(err)); } finally { setBusy(false); }
  }
  async function viewArtifact(id: string, commit?: string) {
    setSelectedArtifact(id);
    try { setPreview(await resourceRequest<Preview>(`${base}/artifacts/${id}${commit ? `?commit=${encodeURIComponent(commit)}` : ""}`)); setError(""); }
    catch (err) { setError(errorMessage(err)); }
  }
  return <>
    <section className="panel"><h2>项目共享信息</h2>
      <p className="muted">独立本地 Git 仓库，不跟踪普通工作区交付文件；修正追加替代记录，分歧保留各方依据，不以最后提交者为准。</p>
      <p>共享版本：<code>{shared?.version ?? "读取中"}</code></p>
      <label>主题过滤<input value={topic} onChange={e => setTopic(e.target.value)} /></label>
      <label>历史共享提交<input value={version} onChange={e => setVersion(e.target.value)} placeholder="完整提交哈希；留空查看当前版本" /></label>
      <button disabled={busy} onClick={() => { setBusy(true); setError(""); void resourceRequest<Shared>(`${base}/shared${version ? `?version=${encodeURIComponent(version)}` : ""}`).then(value => { setShared(value); setSelectedVersion(version); }).catch(err => setError(errorMessage(err))).finally(() => setBusy(false)); }}>读取共享版本</button>
      {shared?.records.filter(record => record.topic.includes(topic)).map(record => <details key={record.id}>
        <summary>{record.topic} · {shared.superseded_ids?.includes(record.id) ? "已被替代" : "有效记录"}{record.conflicts.length ? " · 存在分歧，需局部核验" : ""}</summary>
        <code>{record.id}</code><RichContent content={record.body} />
        <p>适用任务：{record.scope.join("、") || "全项目"}；来源任务 {record.task_id ?? "人工"} / 作业 {record.job_id ?? "—"}</p>
        <p>证据：{record.evidence.join("；")}</p><p>替代：{record.supersedes.join("、") || "无"}；分歧：{record.conflicts.join("、") || "无"}</p><p>{record.reason}</p>
      </details>)}
      {shared?.records.length === 0 ? <p>暂无共享记录。Agent 可在作业结果的 shared_updates 中提交事实、接口与结论。</p> : null}
    </section>
    <section className="panel"><h2>输入与产物</h2><p className="mono">{artifacts?.root}</p>
      <p className="muted">原始输入、任务中间产物与最终交付物分别登记。普通工作区只管理当前文件，不承诺恢复历史内容；请暂停后再手工编辑。</p>
      {artifacts?.check?.interrupted ? <div className="error-box">需要检查现场：{artifacts.check.reason}。停止进程不代表撤销文件修改。</div> : null}
      <label>按任务过滤<select value={taskFilter} onChange={e => setTaskFilter(e.target.value)}><option value="">全部任务</option>{detail.tasks.map(t => <option key={t.id} value={t.id}>{t.key} {t.title}</option>)}</select></label>
      <label>按类型过滤<select value={kindFilter} onChange={e => setKindFilter(e.target.value)}><option value="">全部类型</option><option value="input">原始输入</option><option value="intermediate">中间产物</option><option value="delivery">最终交付</option></select></label>
      <ul>{artifacts?.items.filter(item => (!taskFilter || item.task_id === taskFilter) && (!kindFilter || item.kind === kindFilter)).map(item => <li key={item.id}>
        <strong>{item.path}</strong> · {item.kind} · {item.format || "格式未声明"}<p>{item.verification}</p>
        <button onClick={() => void viewArtifact(item.id)}>查看 / 版本</button>{" "}<button onClick={() => void action(() => resourceRequest(`${base}/artifacts/${item.id}/reveal`, "POST", {}))}>打开所在目录</button>
      </li>)}</ul>
      {preview ? <details open><summary>{preview.path} · {preview.commit?.slice(0, 12) ?? "当前文件"}</summary>
        {preview.versions.length ? <select value={preview.commit ?? ""} onChange={e => void viewArtifact(selectedArtifact, e.target.value)}>{preview.versions.map(v => <option key={v} value={v}>{v}</option>)}</select> : null}
        <pre>{preview.text}</pre>
      </details> : null}
      {!readonly ? <details><summary>登记产物、声明原地修改或确认现场检查</summary>
        <label>相对路径<input value={path} onChange={e => setPath(e.target.value)} placeholder="outputs/report.md" /></label>
        <label>类型<select value={kind} onChange={e => setKind(e.target.value)}><option value="input">原始输入</option><option value="intermediate">中间产物</option><option value="delivery">最终交付</option></select></label>
        <label>格式<input value={format} onChange={e => setFormat(e.target.value)} /></label><label>验收方式<input value={verification} onChange={e => setVerification(e.target.value)} /></label>
        <label>来源 / 授权任务<select value={task} onChange={e => setTask(e.target.value)}><option value="">项目直接完成</option>{detail.tasks.map(t => <option key={t.id} value={t.id}>{t.key} {t.title}</option>)}</select></label>
        <button disabled={busy || !path} onClick={() => void action(() => resourceRequest(`${base}/artifacts`, "POST", { path, kind, format, verification, task_id: task || null }))}>登记产物</button>
        <label>修改理由 / 文件现场检查结论<textarea value={reason} onChange={e => setReason(e.target.value)} /></label>
        <button disabled={busy || !path || !reason.trim()} onClick={() => void action(() => resourceRequest(`${base}/workspace/permissions`, "POST", { path, task_id: task || null, reason }))}>授权该任务修改指定路径</button>{" "}
        <button disabled={busy || !reason.trim()} onClick={() => void action(() => resourceRequest(`${base}/workspace/inspect`, "POST", { reason }))}>记录检查结果并允许后续重试</button>
        <ul>{artifacts?.permissions.map((p, index) => <li key={index}>{p.path} · {p.task_id || "项目直接完成"} · {p.reason}</li>)}</ul>
      </details> : null}
    </section>
    {detail.project.workspace_type === "git" && detail.project.stage === "completed" && !readonly && !detail.merged_into_base ? <section className="panel"><h2>合入冲突处理</h2>
      <p>由人工指定 Agent，在隔离 worktree 中解决冲突并直接继续线性合入；不创建普通任务，不调用独立验收者，不改变项目已完成阶段。</p>
      <p className="muted">风险：本次调用可能额外收费，并绕过角色、启停、冷却和可用时段；仍受项目费用预算及并发限制。</p>
      <select value={agent} onChange={e => setAgent(e.target.value)}><option value="">选择 Agent</option>{detail.agents.map(a => <option key={a.id} value={a.id}>{a.name} · {a.model}</option>)}</select>
      <button disabled={busy || !agent || merge?.request?.state === "running" || merge?.request?.state === "waiting"} onClick={() => {
        if (window.confirm("确认由所选 Agent 处理合入冲突？可能产生额外费用，解决后将直接继续合入。")) void action(() => resourceRequest(`${base}/merge/assist`, "POST", { agent_id: agent, risk_confirmed: true }));
      }}>确认风险并处理冲突</button>
      {merge?.request ? <p>状态：{merge.request.state} · {merge.request.error ?? ""} · 作业 {merge.request.job_id ?? "等待派发"}</p> : null}
    </section> : null}
    {error ? <div className="error-box">{error}</div> : null}
  </>;
}
