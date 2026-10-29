import { useEffect, useState } from "react";
import type {
  Agent,
  ProjectSummary,
  UsageGroup,
  UsageReport,
  UsageTotals,
} from "../types";
import { api, errorMessage, subscribeEvents } from "../api";
import {
  Badge,
  Empty,
  ErrorBox,
  ROLE_LABELS,
  formatCost,
  formatMillis,
  formatTokens,
} from "../ui";

type Dimension = "project" | "agent" | "role";

const DIMENSIONS: Dimension[] = ["project", "agent", "role"];

const DIMENSION_LABELS: Record<Dimension, string> = {
  project: "按项目",
  agent: "按 Agent",
  role: "按角色",
};

const DIMENSION_HINTS: Record<Dimension, string> = {
  project: "每个项目内所有作业的用量合计。",
  agent: "每个 Agent 承担的作业用量；未记录 Agent 的作业单独归入未知分组。",
  role: "按作业角色汇总，便于对比项目分析、直接完成、执行、验收与总结的成本。",
};

const ROLE_FILTERS = ["analysis", "direct", "executor", "verifier", "summary"];

const EMPTY_REPORTS: Record<Dimension, UsageReport | null> = {
  project: null,
  agent: null,
  role: null,
};

function toIso(value: string): string | undefined {
  if (!value) return undefined;
  const time = new Date(value).getTime();
  if (Number.isNaN(time)) return undefined;
  return new Date(time).toISOString();
}

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

function groupName(
  dimension: Dimension,
  group: UsageGroup,
  projects: ProjectSummary[],
  agents: Agent[],
): string {
  if (dimension === "project") {
    return (
      projects.find((project) => project.id === group.project_id)?.name ??
      group.project_id ??
      "未记录项目"
    );
  }
  if (dimension === "agent") {
    if (!group.agent_id) return "未记录 Agent";
    return (
      group.agent_name ??
      agents.find((agent) => agent.id === group.agent_id)?.name ??
      group.agent_id
    );
  }
  return group.role ? ROLE_LABELS[group.role] ?? group.role : "未记录角色";
}

function GroupBadges({
  dimension,
  group,
}: {
  dimension: Dimension;
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
    badges.push(
      <UsageCoverageBadge key="usage-coverage" totals={group} />,
    );
  }
  return <>{badges}</>;
}

function UsageTable({
  dimension,
  report,
  projects,
  agents,
}: {
  dimension: Dimension;
  report: UsageReport;
  projects: ProjectSummary[];
  agents: Agent[];
}) {
  return (
    <section className="panel">
      <h2>{DIMENSION_LABELS[dimension]}</h2>
      <p className="subtitle">{DIMENSION_HINTS[dimension]}</p>
      <p className="subtitle">输入 token 含缓存读写，输出 token 含推理。</p>
      {report.groups.length === 0 ? (
        <Empty
          title="当前筛选范围内没有作业记录"
          hint="调整项目、Agent、角色或时间范围后再试。"
        />
      ) : (
        <div className="usage-table-wrap">
          <table className="usage-table">
            <thead>
              <tr>
                <th>名称</th>
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
              {report.groups.map((group) => (
                <tr key={group.key}>
                  <td>
                    <span className="usage-group-name">
                      {groupName(dimension, group, projects, agents)}
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

function TotalsCards({ totals }: { totals: UsageTotals }) {
  return (
    <>
      <div className="usage-cards">
        <div>
          <span className="meta-label">作业</span>
          <strong>{totals.jobs}</strong>
        </div>
        <div>
          <span className="meta-label">用量记录覆盖</span>
          <strong>
            {totals.jobs > 0 ? (
              <>
                {totals.known_jobs}/{totals.jobs} 个作业
              </>
            ) : (
              "—"
            )}
          </strong>
        </div>
        <div>
          <span className="meta-label">输入 token</span>
          <strong>
            <MetricValue
              value={totals.input_tokens}
              totals={totals}
              format={formatTokens}
            />
          </strong>
        </div>
        <div>
          <span className="meta-label">输出 token</span>
          <strong>
            <MetricValue
              value={totals.output_tokens}
              totals={totals}
              format={formatTokens}
            />
          </strong>
        </div>
        <div>
          <span className="meta-label">缓存读</span>
          <strong>
            <MetricValue
              value={totals.cache_read_tokens}
              totals={totals}
              format={formatTokens}
            />
          </strong>
        </div>
        <div>
          <span className="meta-label">缓存写</span>
          <strong>
            <MetricValue
              value={totals.cache_write_tokens}
              totals={totals}
              format={formatTokens}
            />
          </strong>
        </div>
        <div>
          <span className="meta-label">推理</span>
          <strong>
            <MetricValue
              value={totals.reasoning_tokens}
              totals={totals}
              format={formatTokens}
            />
          </strong>
        </div>
        <div>
          <span className="meta-label">费用</span>
          <strong>
            <MetricValue value={totals.cost} totals={totals} format={formatCost} />
          </strong>
        </div>
        <div>
          <span className="meta-label">时长</span>
          <strong>{formatMillis(totals.duration_ms)}</strong>
        </div>
        <div>
          <span className="meta-label">未知用量</span>
          <strong>
            {totals.unknown_jobs > 0 ? (
              <Badge tone="warn">{totals.unknown_jobs} 个作业</Badge>
            ) : (
              "无"
            )}
          </strong>
        </div>
      </div>
      {totals.jobs > 0 && totals.known_jobs === 0 ? (
        <div className="warn-banner">
          全部作业未记录用量：token 与费用无法合计，显示为“—”。作业数与时长仍包含这些作业。
        </div>
      ) : totals.unknown_jobs > 0 ? (
        <div className="warn-banner">
          已记录 {totals.known_jobs}/{totals.jobs} 个作业的用量；其余{" "}
          {totals.unknown_jobs} 个作业未提供 token 或费用，已单独计为未知。输入/输出
          token 与费用合计只包含已知用量；作业数与时长仍包含未知作业。输入 token
          含缓存读写，输出 token 含推理。
        </div>
      ) : null}
    </>
  );
}

export function UsagePage() {
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [projectId, setProjectId] = useState("");
  const [agentId, setAgentId] = useState("");
  const [role, setRole] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [reloadKey, setReloadKey] = useState(0);
  const [reports, setReports] =
    useState<Record<Dimension, UsageReport | null>>(EMPTY_REPORTS);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");

  useEffect(() => {
    let active = true;
    Promise.all([api.listProjects(), api.listAgents()])
      .then(([nextProjects, nextAgents]) => {
        if (!active) return;
        setProjects(nextProjects);
        setAgents(nextAgents);
      })
      .catch((err) => {
        if (active) setError(errorMessage(err));
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    let timer: number | undefined;
    const unsubscribe = subscribeEvents(() => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setReloadKey((value) => value + 1), 1200);
    });
    return () => {
      window.clearTimeout(timer);
      unsubscribe();
    };
  }, []);

  useEffect(() => {
    let active = true;
    const params = {
      project_id: projectId || undefined,
      agent_id: agentId || undefined,
      role: role || undefined,
      from: toIso(from),
      to: toIso(to),
    };
    setLoading(true);
    Promise.all(
      DIMENSIONS.map((dimension) =>
        api.getUsage({ ...params, group_by: dimension }),
      ),
    )
      .then(([project, agent, groupedByRole]) => {
        if (!active) return;
        setReports({ project, agent, role: groupedByRole });
        setError("");
      })
      .catch((err) => {
        if (active) setError(errorMessage(err));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [projectId, agentId, role, from, to, reloadKey]);

  const totals = reports.project?.totals ?? null;
  const hasFilter = Boolean(projectId || agentId || role || from || to);
  const invalidRange =
    Boolean(from) && Boolean(to) && new Date(from).getTime() > new Date(to).getTime();

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>用量</h1>
          <p className="subtitle">
            汇总每次 Agent 调用的输入/输出 token、费用与时长，可按项目、Agent、角色和时间范围筛选。
          </p>
        </div>
        <button
          className="secondary"
          onClick={() => setReloadKey((value) => value + 1)}
          disabled={loading}
        >
          {loading ? "刷新中…" : "刷新"}
        </button>
      </header>

      {error ? <ErrorBox message={error} /> : null}

      <section className="panel">
        <h2>筛选</h2>
        <div className="form-grid">
          <label>
            <span>项目</span>
            <select value={projectId} onChange={(event) => setProjectId(event.target.value)}>
              <option value="">全部项目</option>
              {projects.map((project) => (
                <option key={project.id} value={project.id}>
                  {project.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span>Agent</span>
            <select value={agentId} onChange={(event) => setAgentId(event.target.value)}>
              <option value="">全部 Agent</option>
              {agents.map((agent) => (
                <option key={agent.id} value={agent.id}>
                  {agent.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span>角色</span>
            <select value={role} onChange={(event) => setRole(event.target.value)}>
              <option value="">全部角色</option>
              {ROLE_FILTERS.map((value) => (
                <option key={value} value={value}>
                  {ROLE_LABELS[value] ?? value}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span>起始时间</span>
            <input
              type="datetime-local"
              value={from}
              onChange={(event) => setFrom(event.target.value)}
            />
          </label>
          <label>
            <span>结束时间</span>
            <input
              type="datetime-local"
              value={to}
              onChange={(event) => setTo(event.target.value)}
            />
          </label>
        </div>
        <div className="button-row usage-filter-actions">
          <button
            className="ghost"
            disabled={!hasFilter}
            onClick={() => {
              setProjectId("");
              setAgentId("");
              setRole("");
              setFrom("");
              setTo("");
            }}
          >
            清除筛选
          </button>
          <span className="muted">
            时间按本地时区解释；不选择时统计全部历史记录。
          </span>
        </div>
        {invalidRange ? (
          <div className="warn-banner usage-filter-warning">
            起始时间晚于结束时间，筛选结果将为空。
          </div>
        ) : null}
      </section>

      {loading && !totals ? (
        <section className="panel">
          <div className="muted">正在加载用量记录…</div>
        </section>
      ) : null}

      {totals ? (
        <section className="panel">
          <h2>汇总</h2>
          <div className="button-row usage-coverage-row">
            <UsageCoverageBadge totals={totals} />
            <span className="muted">未知作业不参与 token 与费用合计。</span>
          </div>
          <TotalsCards totals={totals} />
        </section>
      ) : null}

      {DIMENSIONS.map((dimension) => {
        const report = reports[dimension];
        if (!report) return null;
        return (
          <UsageTable
            key={dimension}
            dimension={dimension}
            report={report}
            projects={projects}
            agents={agents}
          />
        );
      })}
    </div>
  );
}
