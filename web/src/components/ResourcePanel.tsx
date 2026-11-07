import { useEffect, useRef, useState } from "react";
import type { ProjectDetail } from "../types";
import { errorMessage } from "../api";
import { Badge, ErrorBox } from "../ui";
import { RichContent } from "./RichContent";

export async function resourceRequest<T>(url: string, method = "GET", body?: unknown): Promise<T> {
  const response = await fetch(url, { method, headers: { "Content-Type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error ?? `请求失败 ${response.status}`);
  return result as T;
}
interface SharedRecord { id: string; no: string; text: string; supersedes: string; invalidated_by: string | null; task_id: string | null; job_id: string | null; created_at: string }
interface Shared { version: string; repository: string; records: SharedRecord[]; superseded_ids: string[] }
interface Artifact { id: string; path: string; format: string; verification: string; task_id: string | null; commit: string | null }
interface Artifacts { root: string; items: Artifact[] }
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
  const [version, setVersion] = useState("");
  const [selectedVersion, setSelectedVersion] = useState("");
  const [taskFilter, setTaskFilter] = useState("");
  const [preview, setPreview] = useState<(Preview & { artifactId: string }) | null>(null);
  const previewRequestRef = useRef(0);
  const [agent, setAgent] = useState("");
  const readonly = detail.project.status === "archived";
  const filteredArtifacts = (artifacts?.items ?? []).filter(item => !taskFilter || item.task_id === taskFilter);
  useEffect(() => {
    setPreview(null);
    return () => { previewRequestRef.current += 1; };
  }, [base]);
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
    const request = ++previewRequestRef.current;
    setPreview(null);
    try {
      const next = await resourceRequest<Preview>(`${base}/artifacts/${id}${commit ? `?commit=${encodeURIComponent(commit)}` : ""}`);
      if (request !== previewRequestRef.current) return;
      setPreview({ ...next, artifactId: id });
      setError("");
    } catch (err) {
      if (request === previewRequestRef.current) setError(errorMessage(err));
    }
  }
  return <>
    <details className="panel resource-panel"><summary>项目共享信息 <span className="section-count">{shared ? `${shared.records.length} 条记录` : "加载中…"}</span></summary>
      <div className="resource-body">
      <p className="muted">独立本地 Git 仓库；只展示执行者提交且仍有效的项目记忆。修正用 supersedes 指向被替代的 M 编号，旧记录失效但保留在历史中；验收者不提交。</p>
      <p>共享版本：<code>{shared?.version ?? "读取中"}</code></p>
      <div className="form-grid"><label>历史共享提交<input value={version} onChange={e => setVersion(e.target.value)} placeholder="完整提交哈希；留空查看当前版本" /></label>
      </div><div><button className="secondary" disabled={busy} onClick={() => { setBusy(true); setError(""); void resourceRequest<Shared>(`${base}/shared${version ? `?version=${encodeURIComponent(version)}` : ""}`).then(value => { setShared(value); setSelectedVersion(version); }).catch(err => setError(errorMessage(err))).finally(() => setBusy(false)); }}>读取共享版本</button></div>
      {shared?.records.map(record => <details key={record.id}>
        <summary>[{record.no}]{record.supersedes ? ` 替代 ${record.supersedes}` : ""} · 来源 {record.task_id ?? "项目"}</summary>
        <RichContent content={record.text} />
        <p>编号 {record.no} · 来源任务 {record.task_id ?? "项目"} / 作业 {record.job_id ?? "—"}</p>
        {record.supersedes ? <p>替代：{record.supersedes}</p> : null}
      </details>)}
      {shared?.records.length === 0 ? <p>暂无项目记忆。执行者可在结果 shared_updates 中提交 {"{text, supersedes}"}。</p> : null}
      </div>
    </details>
    <details className="panel resource-panel"><summary>成果 <span className="section-count">{artifacts ? `${artifacts.items.length} 项登记` : "加载中…"}</span></summary><div className="resource-body"><p>工作区 / 项目输入目录：<span className="mono">{artifacts?.root}</span></p>
      <p className="muted">目录及其内容就是项目输入，无需逐文件登记。这里仅展示显式登记的重要成果与参考资料，登记不表示验收通过，也不决定文件修改权限。</p>
      <p className="muted">{detail.project.workspace_type === "plain" ? "任务串行使用当前目录；普通工作区不保留文件历史，初始内容请自行备份。" : "任务在隔离 worktree 中执行；已登记成果可查看对应的 Git 提交版本。"}建议暂停后再手工编辑；中断保留文件现场，后续执行先检查再接续。</p>
      <div className="form-grid"><label>按任务过滤<select value={taskFilter} onChange={e => setTaskFilter(e.target.value)}><option value="">全部任务</option>{detail.tasks.map(t => <option key={t.id} value={t.id}>{t.key} {t.title}</option>)}</select></label>
      </div>
      {artifacts && filteredArtifacts.length === 0 ? <p className="muted">当前筛选下没有登记的重要成果；工作区文件仍可按任务需要直接使用。</p> : null}
      <ul className="artifact-list">{filteredArtifacts.map(item => <li key={item.id}>
        <div className="artifact-title"><strong>{item.path}</strong><Badge>{detail.tasks.find(task => task.id === item.task_id)?.key ?? "项目"}</Badge></div>
        <p className="muted">{item.format || "格式未声明"}{item.verification ? ` · ${item.verification}` : ""}</p>
        <div className="button-row"><button className="secondary" onClick={() => void viewArtifact(item.id)}>查看 / 版本</button><button className="ghost" onClick={() => void action(() => resourceRequest(`${base}/artifacts/${item.id}/reveal`, "POST", {}))}>打开所在目录</button></div>
      </li>)}</ul>
      {preview ? <details open><summary>{preview.path} · {preview.commit?.slice(0, 12) ?? "当前文件"}</summary>
        {preview.versions.length ? <select aria-label="产物版本" value={preview.commit ?? ""} onChange={e => void viewArtifact(preview.artifactId, e.target.value)}>{preview.versions.map(v => <option key={v} value={v}>{v}</option>)}</select> : null}
        <pre>{preview.text}</pre>
      </details> : null}
    </div></details>
    {detail.project.workspace_type === "git" && detail.project.stage === "completed" && !readonly && !detail.merged_into_base ? <section className="panel"><h2>合入冲突处理</h2>
      <p>由人工指定 Agent，在隔离 worktree 中解决冲突并直接继续线性合入；不创建普通任务，不调用独立验收者，不改变项目已完成阶段。</p>
      <p className="muted">风险：本次调用可能额外收费，并绕过角色、启停、冷却和可用时段；仍受项目费用预算及并发限制。</p>
      <select aria-label="冲突处理 Agent" value={agent} onChange={e => setAgent(e.target.value)}><option value="">选择 Agent</option>{detail.agents.map(a => <option key={a.id} value={a.id}>{a.name} · {a.model}</option>)}</select>
      <button className="primary" disabled={busy || !agent || merge?.request?.state === "running" || merge?.request?.state === "waiting"} onClick={() => {
        if (window.confirm("确认由所选 Agent 处理合入冲突？可能产生额外费用，解决后将直接继续合入。")) void action(() => resourceRequest(`${base}/merge/assist`, "POST", { agent_id: agent, risk_confirmed: true }));
      }}>确认风险并处理冲突</button>
      {merge?.request ? <p>状态：{merge.request.state} · {merge.request.error ?? ""} · 作业 {merge.request.job_id ?? "等待派发"}</p> : null}
    </section> : null}
    {error ? <ErrorBox message={error} onRetry={() => void refresh().then(() => setError("")).catch(err => setError(errorMessage(err)))} /> : null}
  </>;
}
