import type { UserAction } from "../types";
import { RichContent } from "./RichContent";

/** User actions stay visible; agent instructions and evidence are opt-in. */
export function UserActionNotice({ action, details, guidance, fallback }: {
  action?: UserAction | null;
  details?: string | null;
  guidance?: string | null;
  fallback?: string;
}) {
  return <div className="user-action-notice">
    {action ? <>
      <p><strong>{action.summary}</strong></p>
      <p>{action.why}</p>
      <p><strong>处理方：</strong>{action.owner}</p>
      <p><strong>需要做什么</strong></p>
      <ol>{action.steps.map((step, index) => <li key={index}><RichContent content={step} format="markdown" /></li>)}</ol>
      <p><strong>完成之后：</strong>{action.after}</p>
    </> : <p className="muted">{fallback ?? "这条记录没有单独的用户处理说明。可展开技术详情查看原始原因；如仍不明确，可记录指导，请 AI 先说明需要谁做什么。"}</p>}
    {guidance ? <details className="reference-fold"><summary>Agent 执行指导</summary><RichContent content={guidance} /></details> : null}
    {details ? <details className="raw-error"><summary>技术详情与判断依据</summary><RichContent content={details} /></details> : null}
  </div>;
}
