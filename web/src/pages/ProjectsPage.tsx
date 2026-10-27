import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import type { ProjectSummary } from "../types";
import { api, errorMessage } from "../api";
import {
  Empty,
  ErrorBox,
  Progress,
  StageBadge,
  StatusBadge,
  formatTime,
} from "../ui";

export function ProjectsPage({ onOpen }: { onOpen: (id: string) => void }) {
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [showForm, setShowForm] = useState(false);
  const [name, setName] = useState("");
  const [repoPath, setRepoPath] = useState("");
  const [baseBranch, setBaseBranch] = useState("");
  const [creating, setCreating] = useState(false);

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
                placeholder="/path/to/repository"
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
      ) : (
        <div className="project-list">
          {projects.map((project) => (
            <article
              key={project.id}
              className="project-card"
              onClick={() => onOpen(project.id)}
              role="button"
              tabIndex={0}
              onKeyDown={(event) => {
                if (event.key === "Enter") onOpen(project.id);
              }}
            >
              <div className="project-card-main">
                <div className="project-card-title">
                  <h2>{project.name}</h2>
                  <StageBadge stage={project.stage} />
                  <StatusBadge status={project.status} />
                </div>
                <p className="goal-line">
                  {project.goal || "尚未输入项目目标"}
                </p>
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
          ))}
        </div>
      )}
    </div>
  );
}
