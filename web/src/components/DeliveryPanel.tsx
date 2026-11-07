import { useState } from "react";
import { api, errorMessage } from "../api";
import type { ProjectDetail } from "../types";
import { ErrorBox, formatTime } from "../ui";
import { RichContent } from "./RichContent";

/** Contract versions and explicit consumption authorization share the delivery overview. */
export function DeliveryPanel({ detail, onSaved }: { detail: ProjectDetail; onSaved: () => void }) {
  const delivery = detail.delivery;
  const [editing, setEditing] = useState(false);
  const [budget, setBudget] = useState("");
  const [attempts, setAttempts] = useState("");
  const [replans, setReplans] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  if (!delivery) return null;
  const current = delivery.contracts.find(item => item.id === delivery.current_contract_id);
  const readOnly = detail.project.status === "archived";
  function edit() {
    setBudget(delivery!.limits.cost_budget?.toString() ?? "");
    setAttempts(delivery!.limits.task_attempt_limit?.toString() ?? "");
    setReplans(delivery!.limits.replan_limit?.toString() ?? "");
    setError(""); setEditing(true);
  }
  async function save() {
    const value = (input: string) => input.trim() ? Number(input) : null;
    const limits = { cost_budget: value(budget), task_attempt_limit: value(attempts), replan_limit: value(replans) };
    if (Object.values(limits).some(item => item !== null && !Number.isFinite(item))) {
      setError("限制必须是有效数字"); return;
    }
    setBusy(true); setError("");
    try { await api.setProjectLimits(detail.project.id, limits); setEditing(false); onSaved(); }
    catch (err) { setError(errorMessage(err)); }
    finally { setBusy(false); }
  }
  return <section className="panel delivery-panel">
    <h2>交付契约与消耗边界</h2>
    <p className="subtitle">契约 #{current?.id ?? "—"} · 已记录费用 {delivery.known_cost.toFixed(4)} · 费用未知 {delivery.unknown_cost_jobs} 次</p>
    <details className="reference-fold"><summary>预算计量说明</summary><p className="muted">费用为 Agent 上报值，不代表实际账单或免费使用。预算按已知费用控制新派发，在途作业允许结算；未知或延迟上报的费用使预算不能作为账单硬上限。重试、换人和重规划不会重置累计消耗。</p></details>
    {detail.project.report ? <details className="reference-fold"><summary>最新可审查结果</summary><RichContent content={detail.project.report} /></details> : null}
    {delivery.acceptance ? <details className="reference-fold"><summary>项目集成验收 · 契约 #{delivery.acceptance.contract_id ?? "历史未记录"}</summary>
      <p>已检查版本：{delivery.acceptance.artifact_version ?? "普通工作区当前产物"}</p>
      <ul>{delivery.acceptance.criteria.map((item, index) => <li key={index}>
        <strong>{item.verdict} · {item.criterion}</strong><p>{item.evidence.join("；")}</p>{item.gap ? <p>风险 / 缺口：{item.gap}</p> : null}
      </li>)}</ul>
    </details> : null}
    {detail.project.stage === "completed" ? <details className="reference-fold"><summary>最终交付记录</summary>
      <p>被验收版本：{detail.project.accepted_commit ?? "未记录"}；合入版本：{detail.project.merged_commit ?? "未合入"}</p>
      <p>接受时间：{detail.project.merged_at ?? "见验收记录"}；接受理由：{detail.project.accept_reason ?? "无"}</p>
      <p>合入时 base 已前进：{detail.project.merge_rebased ? "是" : "否"}；冲突由 Agent 解决：{detail.project.merge_agent_resolved ? "是" : "否"}</p>
      {detail.project.merge_conflicts?.length ? <p>冲突文件：{detail.project.merge_conflicts.join("、")}</p> : null}
    </details> : null}
    {delivery.merge_operation?.error ? <div className="error-box">
      合入操作 {delivery.merge_operation.phase}：{delivery.merge_operation.error}
      <p>目标操作前引用：{delivery.merge_operation.base_before}；项目操作前引用：{delivery.merge_operation.project_before}</p>
    </div> : null}
    {current?.proposal?.acceptance.length ? <details className="reference-fold"><summary>当前交付验收标准（{current.proposal.acceptance.length} 项）</summary><ul>{current.proposal.acceptance.map((item, index) => <li key={index}>{item}</li>)}</ul></details> : null}
    <details><summary>契约版本与方案差异（{delivery.contracts.length}）</summary>
      {delivery.contracts.map((item, index) => {
        const before = delivery.contracts[index - 1];
        const criteria = item.proposal?.acceptance ?? [];
        const oldCriteria = before?.proposal?.acceptance ?? [];
        return <details key={item.id}><summary>#{item.id} · {formatTime(item.created_at)} · {item.source}</summary>
          <RichContent content={item.goal} />
          {before && before.goal !== item.goal ? <p>目标发生变化：{before.goal} → {item.goal}</p> : null}
          <p>验收变化：{JSON.stringify(criteria) === JSON.stringify(oldCriteria) ? "无" : "有"}</p>
          <p>取消任务：{item.proposal?.replan_impact?.canceled_task_keys.join("、") || "无"}；保留任务：{item.proposal?.replan_impact?.retained_task_keys.join("、") || "无"}</p>
          <pre>{JSON.stringify(item.proposal, null, 2)}</pre>
          {item.source === "import" ? <p className="muted">历史数据导入快照；不代表能够还原导入前的全部契约版本。</p> : null}
        </details>;
      })}
    </details>
    {delivery.replans?.length ? <details><summary>历史重规划草稿（{delivery.replans.length}）</summary>
      {delivery.replans.map(item => <details key={item.generation}><summary>第 {item.generation} 轮 · {item.state}</summary>
        <p>{item.reason}</p><p>{item.goal}</p><pre>{JSON.stringify(item.draft, null, 2)}</pre>
      </details>)}
    </details> : null}
    <p>费用预算：{delivery.limits.cost_budget ?? "不限"}；任务自动执行尝试：{delivery.limits.task_attempt_limit ?? "不限"}；重规划次数：{delivery.limits.replan_limit ?? "不限"}</p>
    {!readOnly && !editing ? <button className="secondary" onClick={edit}>设置限制 / 授权继续</button> : null}
    {editing ? <form className="form" onSubmit={event => { event.preventDefault(); void save(); }}>
      <label>费用预算（留空不限）<input type="number" min="0" step="any" value={budget} onChange={e => setBudget(e.target.value)} /></label>
      <label>每任务累计执行尝试上限<input type="number" min="1" step="1" value={attempts} onChange={e => setAttempts(e.target.value)} /></label>
      <label>项目累计重规划次数上限<input type="number" min="0" step="1" value={replans} onChange={e => setReplans(e.target.value)} /></label>
      <div className="button-row"><button className="primary" disabled={busy}>保存限制</button><button className="secondary" type="button" disabled={busy} onClick={() => setEditing(false)}>取消</button></div>
    </form> : null}
    {error ? <ErrorBox message={error} /> : null}
  </section>;
}
