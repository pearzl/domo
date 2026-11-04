import type { UsageAggregateMetric, UsageTotals } from "../types";
import { usageMetricPartial, usageMetricValue } from "../presentation/usage";

/** Missing values and partial sums use the selected metric's own coverage. */
export function UsageMetricValue({ totals, metric }: { totals: UsageTotals; metric: UsageAggregateMetric }) {
  const partial = usageMetricPartial(totals, metric);
  return (
    <>
      <span title={totals.known_metrics[metric] === 0 ? "该指标未记录，不能视为零" : undefined}>
        {usageMetricValue(totals[metric], totals, metric)}
      </span>
      {partial ? <span className="usage-partial-mark" tabIndex={0} title={`部分合计：已记录 ${partial} 个作业`} aria-label={`部分合计：已记录 ${partial} 个作业`}>＊</span> : null}
    </>
  );
}
