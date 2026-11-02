import type { ProjectUsage } from "../types";
import { usagePath } from "../router";
import { AnimatedNumber } from "./AnimatedNumber";
import {
  usageAllUnknown,
  usageCoverageLabel,
  usageCoverageNote,
  usageMetricValue,
  usagePartial,
} from "../presentation/usage";

/// 概览里的用量摘要：作业数、token、费用、时长与用量记录覆盖各以一张小卡片呈现，
/// 数值与用量页同口径（无法合计时为“—”），并用一句话说明覆盖范围。
/// 明细仍由用量页按项目过滤查看，这里只保留跳转入口。
/// 卡片数值按项目会话逐位滚动，刷新后首次展示保持静态。
export function UsageSummary({
  projectId,
  usage,
}: {
  projectId: string;
  usage: ProjectUsage;
}) {
  const totals = usage.totals;
  const session = `project:${projectId}`;
  const coverage = usageCoverageLabel(totals);
  // 只有部分作业有用量记录时，合计数值后标注已记录作业数。
  const partial = usagePartial(totals) ? coverage : null;
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
          session={session}
          slot="usage:jobs"
          label="作业数"
          value={`${totals.jobs} 个`}
        />
        <SummaryStat
          session={session}
          slot="usage:input-tokens"
          label="输入 token"
          value={usageMetricValue(totals.input_tokens, totals, "tokens")}
          partial={partial}
        />
        <SummaryStat
          session={session}
          slot="usage:output-tokens"
          label="输出 token"
          value={usageMetricValue(totals.output_tokens, totals, "tokens")}
          partial={partial}
        />
        <SummaryStat
          session={session}
          slot="usage:cost"
          label="费用"
          value={usageMetricValue(totals.cost, totals, "cost")}
          partial={partial}
        />
        <SummaryStat
          session={session}
          slot="usage:duration"
          label="时长"
          value={usageMetricValue(totals.duration_ms, totals, "duration")}
        />
      </div>
      <p
        className={`usage-summary-note${usageAllUnknown(totals) ? " usage-summary-note-warn" : ""}`}
      >
        {note}
      </p>
      <div className="usage-summary-actions">
        <a className="button-link" href={usagePath(projectId)}>
          查看用量明细
        </a>
      </div>
    </section>
  );
}

function SummaryStat({
  session,
  slot,
  label,
  value,
  partial,
}: {
  /// 项目会话键，数字滚动按它记忆上一次数值。
  session: string;
  /// 同一会话内各指标的槽位名。
  slot: string;
  label: string;
  value: string;
  /// 部分覆盖时在数值后标注已记录作业数，避免把合计误读成全量。
  partial?: string | null;
}) {
  return (
    <div className="usage-summary-stat">
      <span className="meta-label">{label}</span>
      <strong className="usage-summary-value">
        <AnimatedNumber session={session} slot={slot} value={value} />
      </strong>
      {partial ? (
        <span className="usage-summary-partial muted">（{partial} 个作业）</span>
      ) : null}
    </div>
  );
}
