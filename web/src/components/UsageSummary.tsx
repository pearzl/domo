import type { ProjectUsage } from "../types";
import { usagePath } from "../router";
import {
  usageAllUnknown,
  usageCoverageLabel,
  usageCoverageNote,
  usageMetricValue,
  usageMetricPartial,
} from "../presentation/usage";

/// 概览里的用量摘要：作业数、token、费用、时长与用量记录覆盖各以一张小卡片呈现，
/// 数值与用量页同口径（无法合计时为“—”），并用一句话说明覆盖范围。
/// 明细仍由用量页按项目过滤查看，这里只保留跳转入口。
export function UsageSummary({
  projectId,
  usage,
}: {
  projectId: string;
  usage: ProjectUsage;
}) {
  const totals = usage.totals;
  const coverage = usageCoverageLabel(totals);
  const note = usageCoverageNote(totals);
  return (
    <section
      className="usage-summary"
      aria-label="用量摘要"
      data-block="usage-summary"
    >
      <div className="usage-summary-head">
        <span className="meta-label">用量</span>
        <span
          className="usage-coverage-note"
          title="已记录 token 或费用的作业数 / 作业总数；未知作业不参与 token 与费用合计"
        >
          用量记录覆盖 {coverage ?? "—"}
        </span>
      </div>
      <div className="usage-summary-cards">
        <SummaryStat
          label="作业数"
          value={`${totals.jobs} 个`}
        />
        <SummaryStat
          label="输入 token"
          value={usageMetricValue(totals.input_tokens, totals, "input_tokens")}
          partial={usageMetricPartial(totals, "input_tokens")}
        />
        <SummaryStat
          label="输出 token"
          value={usageMetricValue(totals.output_tokens, totals, "output_tokens")}
          partial={usageMetricPartial(totals, "output_tokens")}
        />
        <SummaryStat
           label="已记录费用"
          value={usageMetricValue(totals.cost, totals, "cost")}
          partial={usageMetricPartial(totals, "cost")}
        />
        <SummaryStat
          label="时长"
          value={usageMetricValue(totals.duration_ms, totals, "duration_ms")}
        />
      </div>
      <p
        className={`usage-summary-note${usageAllUnknown(totals) ? " usage-summary-note-warn" : ""}`}
      >
        {note}
      </p>
      <p className="usage-summary-note">＊ 为部分合计，覆盖范围可聚焦标记查看。费用为 Agent 上报值，不代表账单或免费使用。</p>
      <div className="usage-summary-actions">
        <a className="button-link" href={usagePath(projectId)}>
          查看用量明细
        </a>
      </div>
    </section>
  );
}

function SummaryStat({
  label,
  value,
  partial,
}: {
  label: string;
  value: string;
  /// 部分覆盖时在数值后标注已记录作业数，避免把合计误读成全量。
  partial?: string | null;
}) {
  return (
    <div className="usage-summary-stat">
      <span className="meta-label">{label}</span>
      <strong className="usage-summary-value">
        {value}
      </strong>
      {partial ? (
        <span className="usage-partial-mark" tabIndex={0} title={`部分合计：已记录 ${partial} 个作业`} aria-label={`部分合计：已记录 ${partial} 个作业`}>＊</span>
      ) : null}
    </div>
  );
}
