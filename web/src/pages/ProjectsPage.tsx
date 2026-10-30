import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";
import type { ProjectSummary } from "../types";
import { api, errorMessage } from "../api";
import {
  Badge,
  Empty,
  ErrorBox,
  Progress,
  StageBadge,
  StatusBadge,
  formatTime,
  STAGE_LABELS,
  STATUS_LABELS,
} from "../ui";
import {
  EMPTY_PROJECT_FILTERS,
  PROJECT_STAGES,
  PROJECT_STATUSES,
  countProjectStage,
  countProjectStatus,
  filterProjects,
  isProjectFilterActive,
  resetProjectFilters,
  toggleProjectStage,
  toggleProjectStatus,
} from "../projectFilter";
import type { ProjectFilters, ProjectStageFilter, ProjectStatusFilter } from "../projectFilter";

export function ProjectsPage({ onOpen }: { onOpen: (id: string) => void }) {
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [showForm, setShowForm] = useState(false);
  const [name, setName] = useState("");
  const [repoPath, setRepoPath] = useState("");
  const [baseBranch, setBaseBranch] = useState("");
  const [creating, setCreating] = useState(false);
  const [filters, setFilters] = useState<ProjectFilters>(EMPTY_PROJECT_FILTERS);

  async function refresh() {
    try {
      setProjects(await api.listProjects());
      setError("");
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 5000);
    return () => window.clearInterval(timer);
  }, []);

  const visibleProjects = useMemo(
    () => filterProjects(projects, filters),
    [projects, filters],
  );

  async function handleCreate(event: FormEvent) {
    event.preventDefault();
    setCreating(true);
    setError("");
    try {
      const detail = await api.createProject({
        name: name.trim(),
        repo_path: repoPath.trim(),
        base_branch: baseBranch.trim() || undefined,
      });
      setName("");
      setRepoPath("");
      setBaseBranch("");
      setShowForm(false);
      onOpen(detail.project.id);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setCreating(false);
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>项目</h1>
          <p className="subtitle">每个项目在一个独立的 Git worktree 中推进，交付分支不会直接改动 base。</p>
        </div>
        <button className="primary" onClick={() => setShowForm((value) => !value)}>
          {showForm ? "取消" : "新建项目"}
        </button>
      </header>

      {error ? <ErrorBox message={error} /> : null}

      {loading ? null : projects.length > 0 ? (
        <FilterBar
          projects={projects}
          filters={filters}
          onReset={() => setFilters(resetProjectFilters())}
          onToggleStage={(stage) =>
            setFilters((current) => toggleProjectStage(current, stage))
          }
          onToggleStatus={(status) =>
            setFilters((current) => toggleProjectStatus(current, status))
          }
        />
      ) : null}

      {showForm ? (
        <form className="panel form" onSubmit={handleCreate}>
          <div className="form-grid">
            <label>
              <span>项目名称</span>
              <input
                value={name}
                onChange={(event) => setName(event.target.value)}
                placeholder="例如：完善用户登录"
                required
              />
            </label>
            <label>
              <span>Git 仓库路径</span>
              <input
                value={repoPath}
                onChange={(event) => setRepoPath(event.target.value)}
                placeholder="C:\repos\demo 或 ~/repos/demo"
                required
              />
            </label>
            <label>
              <span>base 分支（留空使用仓库当前分支）</span>
              <input
                value={baseBranch}
                onChange={(event) => setBaseBranch(event.target.value)}
                placeholder="main"
              />
            </label>
          </div>
          <div className="form-actions">
            <button className="primary" type="submit" disabled={creating}>
              {creating ? "创建中…" : "创建项目"}
            </button>
          </div>
        </form>
      ) : null}

      {loading ? (
        <div className="muted">加载中…</div>
      ) : projects.length === 0 ? (
        <Empty title="还没有项目" hint="新建项目并进入项目后，输入目标与规划者确认。" />
      ) : visibleProjects.length === 0 ? (
        <div className="empty">
          <div className="empty-title">没有符合筛选条件的项目</div>
          <div className="empty-hint">调整筛选标签组合后再试。</div>
          <div className="empty-actions">
            <button className="primary" onClick={() => setFilters(resetProjectFilters())}>
              重置筛选
            </button>
          </div>
        </div>
      ) : (
        <div className="project-list">{renderProjectCards(visibleProjects, onOpen)}</div>
      )}
    </div>
  );
}

function renderProjectCards(
  projects: ProjectSummary[],
  onOpen: (id: string) => void,
) {
  return projects.map((project) => (
    <article
      key={project.id}
      className="project-card"
      onClick={() => onOpen(project.id)}
      role="button"
      tabIndex={0}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          if (event.key === " ") event.preventDefault();
          onOpen(project.id);
        }
      }}
    >
      <div className="project-card-main">
        <div className="project-card-title">
          <h2>{project.name}</h2>
          <StageBadge stage={project.stage} />
          <StatusBadge status={project.status} />
          {project.merged_at ? (
            <Badge
              tone="success"
              title={`已合入 ${project.base_branch}：${formatTime(project.merged_at)}`}
            >
              已合入 {project.merged_commit?.slice(0, 7) ?? ""}
            </Badge>
          ) : null}
        </div>
        <p className="goal-line">{project.goal || "尚未输入项目目标"}</p>
        {project.blocked_reason ? (
          <p className="blocked-line">阻塞：{project.blocked_reason}</p>
        ) : null}
        <div className="meta-line">
          <span>{project.repo_path}</span>
          <span>
            {project.base_branch} → {project.branch}
          </span>
          <span>更新于 {formatTime(project.updated_at)}</span>
        </div>
      </div>
      <div className="project-card-side">
        <Progress total={project.task_total} completed={project.task_completed} />
      </div>
    </article>
  ));
}

interface FilterBarProps {
  projects: ProjectSummary[];
  filters: ProjectFilters;
  onToggleStage: (stage: ProjectStageFilter) => void;
  onToggleStatus: (status: ProjectStatusFilter) => void;
  onReset: () => void;
}

function FilterBar({ projects, filters, onToggleStage, onToggleStatus, onReset }: FilterBarProps) {
  const allActive = !isProjectFilterActive(filters);
  return (
    <div className="filter-bar panel" aria-label="项目筛选">
      <div className="filter-group">
        <span className="filter-group-label">阶段</span>
        {PROJECT_STAGES.map((stage) => (
          <FilterTag
            key={stage}
            label={STAGE_LABELS[stage] ?? stage}
            count={countProjectStage(projects, stage)}
            active={filters.stages.includes(stage)}
            onClick={() => onToggleStage(stage)}
          />
        ))}
      </div>
      <div className="filter-group">
        <span className="filter-group-label">状态</span>
        {PROJECT_STATUSES.map((status) => (
          <FilterTag
            key={status}
            label={STATUS_LABELS[status] ?? status}
            count={countProjectStatus(projects, status)}
            active={filters.statuses.includes(status)}
            onClick={() => onToggleStatus(status)}
          />
        ))}
      </div>
      {allActive ? null : (
        <button className="filter-reset" onClick={onReset}>
          重置筛选
        </button>
      )}
    </div>
  );
}

function FilterTag({
  label,
  count,
  active,
  onClick,
}: {
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      className={`filter-tag${active ? " active" : ""}`}
      aria-pressed={active}
      onClick={onClick}
    >
      {label}
      <span className="filter-tag-count">{count}</span>
    </button>
  );
}
