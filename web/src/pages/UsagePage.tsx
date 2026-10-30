import { useEffect, useState } from "react";
import type { Agent, ProjectSummary, UsageReport, UsageTotals } from "../types";
import { api, errorMessage, subscribeEvents } from "../api";
import { UsageTable } from "../components/UsageGroupsTable";
import { readUsageProjectId, usagePath } from "../router";
import {
  EMPTY_USAGE_FILTERS,
  USAGE_DIMENSIONS,
  USAGE_DIMENSION_LABELS,
  USAGE_ROLE_FILTERS,
  type UsageDimension,
  type UsageFilters,
  restrictUsageToProject,
  usageProjectOptions,
  usageRequestParams,
} from "../presentation/usage";
import {
  Badge,
  ErrorBox,
  ROLE_LABELS,
  formatCost,
  formatMillis,
  formatTokens,
} from "../ui";

// 初始项目过滤：地址栏带 project_id 时预选该项目，否则不限制。
function initialProjectId(): string {
  if (typeof window === "undefined") return "";
  return readUsageProjectId(window.location.search) ?? "";
}

// 卡片数值：全部未知时显示占位，部分缺失时标注覆盖比例。
function MetricValue({
  value,
  totals,
  format,
}: {
  value: number;
  totals: UsageTotals;
  format: (value: number) => string;
}) {
  if (totals.jobs > 0 && totals.known_jobs === 0) {
    return (
      <span className="muted" title="全部作业未记录用量">
        —
      </span>
    );
  }
  const partial = totals.known_jobs > 0 && totals.known_jobs < totals.jobs;
  return (
    <>
      {format(value)}
      {partial ? (
        <span className="muted usage-coverage-note">
          （{totals.known_jobs}/{totals.jobs} 个作业）
        </span>
      ) : null}
    </>
  );
}

// 顶部覆盖徽标：作业全部未记录用量时提示，其余情况说明未知作业的计数方式。
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

/// 汇总卡片：展示当前筛选范围内的作业数与用量合计。
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
  const [dimension, setDimension] = useState<UsageDimension>("project");
  const [filters, setFilters] = useState<UsageFilters>(() => ({
    project_id: initialProjectId(),
    agent_id: "",
    role: "",
    from: "",
    to: "",
  }));
  const [reloadKey, setReloadKey] = useState(0);
  const [report, setReport] = useState<UsageReport | null>(null);
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
    setLoading(true);
    api
      .getUsage(usageRequestParams(filters, dimension))
      .then((next) => {
        if (!active) return;
        setReport(next);
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
  }, [dimension, filters, reloadKey]);

  // 地址栏与项目筛选项保持同步，便于复制链接或在项目页直接跳到本项目明细。
  useEffect(() => {
    if (typeof window === "undefined") return;
    const next = usagePath(filters.project_id);
    if (`${window.location.pathname}${window.location.search}` === next) return;
    window.history.replaceState(window.history.state, "", next);
  }, [filters.project_id]);

  // 汇总与表格都来自当前筛选条件下、限定到所选项目的同一份报告。
  const scopedReport = restrictUsageToProject(report, filters.project_id);
  const totals = scopedReport?.totals ?? null;
  const projectOptions = usageProjectOptions(filters.project_id, projects);
  const hasFilter = Boolean(
    filters.project_id || filters.agent_id || filters.role || filters.from || filters.to,
  );
  const invalidRange =
    Boolean(filters.from) &&
    Boolean(filters.to) &&
    new Date(filters.from).getTime() > new Date(filters.to).getTime();
  const setField = <K extends keyof UsageFilters>(key: K, value: UsageFilters[K]) =>
    setFilters((current) => ({ ...current, [key]: value }));

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
            <select
              value={filters.project_id}
              onChange={(event) => setField("project_id", event.target.value)}
            >
              <option value="">全部项目</option>
              {projectOptions.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span>Agent</span>
            <select
              value={filters.agent_id}
              onChange={(event) => setField("agent_id", event.target.value)}
            >
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
            <select
              value={filters.role}
              onChange={(event) => setField("role", event.target.value)}
            >
              <option value="">全部角色</option>
              {USAGE_ROLE_FILTERS.map((value) => (
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
              value={filters.from}
              onChange={(event) => setField("from", event.target.value)}
            />
          </label>
          <label>
            <span>结束时间</span>
            <input
              type="datetime-local"
              value={filters.to}
              onChange={(event) => setField("to", event.target.value)}
            />
          </label>
        </div>
        <div className="button-row usage-filter-actions">
          <button
            className="ghost"
            disabled={!hasFilter}
            onClick={() => setFilters(EMPTY_USAGE_FILTERS)}
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

      <div
        className="segmented usage-dimensions"
        role="tablist"
        aria-label="用量维度"
      >
        {USAGE_DIMENSIONS.map((value) => (
          <button
            key={value}
            type="button"
            role="tab"
            aria-selected={dimension === value}
            className={dimension === value ? "segmented-item active" : "segmented-item"}
            onClick={() => setDimension(value)}
          >
            {USAGE_DIMENSION_LABELS[value]}
          </button>
        ))}
      </div>

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

      <UsageTable
        dimension={dimension}
        report={scopedReport}
        projects={projects}
        agents={agents}
        loading={loading}
      />
    </div>
  );
}
