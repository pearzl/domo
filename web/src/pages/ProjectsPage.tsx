import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";
import type { ProjectSummary } from "../types";
import { api, errorMessage } from "../api";
import { SkeletonLines } from "../components/Skeleton";
import {
  Badge,
  EmptySteps,
  ErrorBox,
  Progress,
  StageBadge,
  StatusBadge,
  formatTime,
  projectStatusLabel,
  stageLabel,
} from "../ui";
import {
  EMPTY_PROJECT_FILTERS,
  PROJECT_ATTENTION_LABELS,
  PROJECT_STAGES,
  PROJECT_STATUSES,
  countProjectAttention,
  countProjectStage,
  countProjectStatus,
  filterProjects,
  isProjectFilterActive,
  resetProjectFilters,
  toggleProjectAttention,
  toggleProjectStage,
  toggleProjectStatus,
} from "../projectFilter";
import type { ProjectFilters, ProjectStageFilter, ProjectStatusFilter } from "../projectFilter";

export function ProjectsPage({ onOpen }: { onOpen: (id: string) => void }) {
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [formError, setFormError] = useState("");
  const [showForm, setShowForm] = useState(false);
  const [name, setName] = useState("");
  const [goal, setGoal] = useState("");
  const [repoPath, setRepoPath] = useState("");
  const [baseBranch, setBaseBranch] = useState("");
  const [workspaceType, setWorkspaceType] = useState<"git" | "plain">("git");
  const [confirmMode, setConfirmMode] = useState<"manual" | "auto">("manual");
  const [creating, setCreating] = useState(false);
  const [filters, setFilters] = useState<ProjectFilters>(EMPTY_PROJECT_FILTERS);

  async function refresh(showLoading = false) {
    if (showLoading) setLoading(true);
    try {
      setProjects(await api.listProjects());
      setLoadError("");
    } catch (err) {
      setLoadError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void refresh(true);
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
    setFormError("");
    try {
      const detail = await api.createProject({
        name: name.trim(),
        repo_path: repoPath.trim(),
        goal: goal.trim() || undefined,
        workspace_type: workspaceType,
        confirm_mode: confirmMode,
        base_branch:
          workspaceType === "git" ? baseBranch.trim() || undefined : undefined,
      });
      setName("");
      setGoal("");
      setRepoPath("");
      setBaseBranch("");
      setShowForm(false);
      onOpen(detail.project.id);
    } catch (err) {
      setFormError(errorMessage(err));
    } finally {
      setCreating(false);
    }
  }

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>项目</h1>
          <p className="subtitle">每个项目在独立工作区中推进。Git 工作区使用 worktree 与交付分支，普通工作区直接在所选本地目录中按顺序推进。任务是可独立验收的工作单元；“作业”指任务每次调用 Agent 产生的一次执行记录。</p>
        </div>
        <button className="primary" onClick={() => setShowForm((value) => !value)}>
          {showForm ? "取消" : "新建项目"}
        </button>
      </header>

      {loadError ? (
        <ErrorBox
          message={loadError}
          onRetry={() => void refresh(true)}
          retrying={loading}
        />
      ) : null}
      {formError ? <ErrorBox message={formError} /> : null}

      {loading ? null : projects.length > 0 ? (
        <FilterBar
          projects={projects}
          filters={filters}
          onReset={() => setFilters(resetProjectFilters())}
          onToggleAttention={() =>
            setFilters((current) => toggleProjectAttention(current))
          }
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
              <span>工作区类型</span>
              <select
                value={workspaceType}
                onChange={(event) =>
                  setWorkspaceType(event.target.value as "git" | "plain")
                }
              >
                <option value="git">Git 工作区</option>
                <option value="plain">普通工作区</option>
              </select>
            </label>
            <label>
              <span>{workspaceType === "git" ? "Git 仓库路径" : "本地目录"}</span>
              <input
                value={repoPath}
                onChange={(event) => setRepoPath(event.target.value)}
                placeholder={
                  workspaceType === "git"
                    ? "C:\\repos\\demo 或 ~/repos/demo"
                    : "C:\\docs\\delivery 或 ~/delivery（不存在时自动创建）"
                }
                required
              />
            </label>
            {workspaceType === "git" ? (
              <label>
                <span>base 分支（留空使用仓库当前分支）</span>
                <input
                  value={baseBranch}
                  onChange={(event) => setBaseBranch(event.target.value)}
                  placeholder="main"
                />
              </label>
            ) : null}
            <label>
              <span>方案确认方式</span>
              <select
                value={confirmMode}
                onChange={(event) =>
                  setConfirmMode(event.target.value as "manual" | "auto")
                }
              >
                <option value="manual">人工确认（默认）</option>
                <option value="auto">自动确认</option>
              </select>
            </label>
            <label className="form-wide">
              <span>项目目标（可稍后补充）</span>
              <textarea
                value={goal}
                onChange={(event) => setGoal(event.target.value)}
                rows={3}
                placeholder="说明要达成的结果、范围和验收预期；留空时项目保持阻塞等待输入。"
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
        <div className="panel">
          <SkeletonLines label="加载中…" />
        </div>
      ) : projects.length === 0 && !loadError ? (
        <EmptySteps
          title="还没有项目"
          hint="按三步开始第一个项目。"
          steps={[
            "新建项目：填写项目名称与工作区（Git 仓库或普通目录）。",
            "输入目标并确认方案：在项目分析里描述目标、边界和验收预期，确认规划者给出的方案。",
            "跟踪执行与验收：任务按依赖自动推进，可在概览、任务与事件时间线查看进展，报告生成后验收并合入。",
          ]}
          action={{ label: "新建项目", onClick: () => setShowForm(true) }}
        />
      ) : projects.length === 0 ? null : visibleProjects.length === 0 ? (
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
          <Badge tone="info">
            {project.workspace_type === "plain" ? "普通工作区" : "Git 工作区"}
          </Badge>
          <StageBadge stage={project.stage} />
          <StatusBadge status={project.status} />
          {project.attention ? (
            <Badge
              tone={
                project.attention === "failed" || project.attention === "blocked"
                  ? "danger"
                  : "success"
              }
            >
              {PROJECT_ATTENTION_LABELS[project.attention]}
            </Badge>
          ) : null}
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
          {project.workspace_type === "plain" ? (
            <span>普通工作区 · 串行推进</span>
          ) : (
            <span>
              {project.base_branch} → {project.branch}
            </span>
          )}
          <span>更新于 {formatTime(project.updated_at)}</span>
        </div>
      </div>
      <div className="project-card-side">
        <Progress
          total={project.task_total}
          completed={project.task_completed}
          session={`project:${project.id}`}
        />
      </div>
    </article>
  ));
}

interface FilterBarProps {
  projects: ProjectSummary[];
  filters: ProjectFilters;
  onToggleAttention: () => void;
  onToggleStage: (stage: ProjectStageFilter) => void;
  onToggleStatus: (status: ProjectStatusFilter) => void;
  onReset: () => void;
}

function FilterBar({ projects, filters, onToggleAttention, onToggleStage, onToggleStatus, onReset }: FilterBarProps) {
  const allActive = !isProjectFilterActive(filters);
  return (
    <div className="filter-bar panel" aria-label="项目筛选">
      <div className="filter-group">
        <span className="filter-group-label">视角</span>
        <FilterTag
          label="待我处理"
          count={countProjectAttention(projects)}
          active={filters.attentionOnly}
          onClick={onToggleAttention}
        />
      </div>
      <div className="filter-group">
        <span className="filter-group-label">阶段</span>
        {PROJECT_STAGES.map((stage) => (
          <FilterTag
            key={stage}
            label={stageLabel(stage)}
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
            label={projectStatusLabel(status) ?? status}
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
