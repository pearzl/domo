import type { Agent, ProjectSummary, UsageGroup, UsageReport, UsageTotals } from "../types";
import type { UsageDimension } from "../presentation/usage";
import {
  USAGE_DIMENSION_HINTS,
  USAGE_DIMENSION_LABELS,
  USAGE_NAME_HEADERS,
  currentUsageReport,
  usageGroupName,
} from "../presentation/usage";
import {
  Badge,
  Empty,
  formatCost,
  formatMillis,
  formatTokens,
} from "../ui";

function usageAllUnknown(totals: UsageTotals): boolean {
  return totals.jobs > 0 && totals.known_jobs === 0;
}

function usagePartial(totals: UsageTotals): boolean {
  return totals.known_jobs > 0 && totals.known_jobs < totals.jobs;
}

function MetricValue({
  value,
  totals,
  format,
}: {
  value: number;
  totals: UsageTotals;
  format: (value: number) => string;
}) {
  if (usageAllUnknown(totals)) {
    return (
      <span className="muted" title="全部作业未记录用量">
        —
      </span>
    );
  }
  return (
    <>
      {format(value)}
      {usagePartial(totals) ? (
        <span className="muted usage-coverage-note">
          （{totals.known_jobs}/{totals.jobs} 个作业）
        </span>
      ) : null}
    </>
  );
}

function UsageCoverageBadge({ totals }: { totals: UsageTotals }) {
  if (totals.jobs === 0) return null;
  const tone =
    totals.known_jobs === 0
      ? "warn"
      : totals.known_jobs < totals.jobs
        ? "info"
        : "muted";
  return (
    <Badge
      tone={tone}
      title="已记录 token 或费用的作业数 / 作业总数；未知作业不参与 token 与费用合计"
    >
      用量记录覆盖 {totals.known_jobs}/{totals.jobs} 个作业
    </Badge>
  );
}

function GroupBadges({
  dimension,
  group,
}: {
  dimension: UsageDimension;
  group: UsageGroup;
}) {
  const badges = [];
  if (dimension === "agent" && !group.agent_id) {
    badges.push(
      <Badge key="agent" tone="warn" title="作业未记录 Agent，无法归属到具体 Agent">
        未知 Agent
      </Badge>,
    );
  }
  if (group.jobs > 0 && group.known_jobs < group.jobs) {
    badges.push(<UsageCoverageBadge key="usage-coverage" totals={group} />);
  }
  return <>{badges}</>;
}

/// 用量分组明细表：一次只渲染一个维度的一张表，表头与行数据都随维度变化。
export function UsageTable({
  dimension,
  report,
  projects,
  agents,
  loading,
}: {
  dimension: UsageDimension;
  report: UsageReport | null;
  projects: ProjectSummary[];
  agents: Agent[];
  loading: boolean;
}) {
  const current = currentUsageReport(dimension, report);
  return (
    <section className="panel" data-dimension={dimension}>
      <h2>{USAGE_DIMENSION_LABELS[dimension]}</h2>
      <p className="subtitle">{USAGE_DIMENSION_HINTS[dimension]}</p>
      <p className="subtitle">输入 token 含缓存读写，输出 token 含推理。</p>
      {!current ? (
        <div className="muted">
          {loading ? "正在加载用量记录…" : "暂时没有可展示的用量数据。"}
        </div>
      ) : current.groups.length === 0 ? (
        <Empty
          title="当前筛选范围内没有作业记录"
          hint="调整项目、Agent、角色或时间范围后再试。"
        />
      ) : (
        <div className="usage-table-wrap">
          <table className="usage-table" data-dimension={dimension}>
            <thead>
              <tr>
                <th>{USAGE_NAME_HEADERS[dimension]}</th>
                <th className="num">作业</th>
                <th className="num">输入 token</th>
                <th className="num">输出 token</th>
                <th className="num">缓存读</th>
                <th className="num">缓存写</th>
                <th className="num">推理</th>
                <th className="num">费用</th>
                <th className="num">时长</th>
                <th className="num">未知</th>
              </tr>
            </thead>
            <tbody>
              {current.groups.map((group) => (
                <tr key={group.key}>
                  <td>
                    <span className="usage-group-name">
                      {usageGroupName(dimension, group, projects, agents)}
                      <GroupBadges dimension={dimension} group={group} />
                    </span>
                  </td>
                  <td className="num">{group.jobs}</td>
                  <td className="num">
                    <MetricValue
                      value={group.input_tokens}
                      totals={group}
                      format={formatTokens}
                    />
                  </td>
                  <td className="num">
                    <MetricValue
                      value={group.output_tokens}
                      totals={group}
                      format={formatTokens}
                    />
                  </td>
                  <td className="num">
                    <MetricValue
                      value={group.cache_read_tokens}
                      totals={group}
                      format={formatTokens}
                    />
                  </td>
                  <td className="num">
                    <MetricValue
                      value={group.cache_write_tokens}
                      totals={group}
                      format={formatTokens}
                    />
                  </td>
                  <td className="num">
                    <MetricValue
                      value={group.reasoning_tokens}
                      totals={group}
                      format={formatTokens}
                    />
                  </td>
                  <td className="num">
                    <MetricValue
                      value={group.cost}
                      totals={group}
                      format={formatCost}
                    />
                  </td>
                  <td className="num">{formatMillis(group.duration_ms)}</td>
                  <td className="num">
                    {group.unknown_jobs > 0 ? (
                      <Badge
                        tone="warn"
                        title="这些作业未提供 token 或费用，合计值只包含已知用量"
                      >
                        {group.unknown_jobs} 个未知
                      </Badge>
                    ) : (
                      <span className="muted">—</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
