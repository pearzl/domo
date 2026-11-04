import { useEffect, useMemo, useState } from "react";
import type { ProjectSummary } from "../types";
import { api, errorMessage } from "../api";
import { PROJECT_ATTENTION_LABELS } from "../projectFilter";
import { attentionItems } from "../presentation/attention";
import { formatElapsed } from "../presentation/projectStatus";
import { ErrorBox, formatTime } from "../ui";
import { SkeletonLines } from "../components/Skeleton";

const REFRESH_MS = 5000;

/// 待我处理：集中展示所有需要人工驱动的项目，默认按等待时长倒序排列。
/// 数据来自项目列表的待办口径，处理完成后项目离开列表，导航计数同步减少。
export function AttentionPage({ onOpen }: { onOpen: (id: string, taskId?: string) => void }) {
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    let active = true;
    async function refresh(showLoading = false) {
      if (showLoading) setLoading(true);
      try {
        const next = await api.listProjects();
        if (!active) return;
        setProjects(next);
        setLoadError("");
      } catch (err) {
        if (active) setLoadError(errorMessage(err));
      } finally {
        if (active) setLoading(false);
      }
    }
    void refresh(true);
    const timer = window.setInterval(() => void refresh(), REFRESH_MS);
    const clock = window.setInterval(() => setNow(Date.now()), 1000);
    return () => {
      active = false;
      window.clearInterval(timer);
      window.clearInterval(clock);
    };
  }, []);

  const items = useMemo(
    () => attentionItems(projects, now),
    [projects, now],
  );

  return (
    <div className="page">
      <header className="page-header">
        <div>
          <h1>待我处理</h1>
          <p className="subtitle">
            所有需要你回答、确认、验收或裁决的项目；处理完成后事项会离开此列表。
          </p>
        </div>
        <span className="muted">{items.length} 项待办</span>
      </header>

      {loadError ? (
        <ErrorBox message={loadError} />
      ) : null}

      {loading ? (
        <div className="panel">
          <SkeletonLines label="加载中…" />
        </div>
      ) : items.length === 0 ? (
        <div className="empty">
          <div className="empty-title">没有待处理事项</div>
          <div className="empty-hint">需要你驱动的项目会出现在这里。</div>
        </div>
      ) : (
        <div className="attention-list">
          {items.map(({ project, action, waitingMs, id, affectedTasks, blocksMilestone, projectAdvancing }) => {
            const attention = action.kind;
            return (
              <article
                key={id ?? project.id}
                className="panel attention-item"
                role="button"
                tabIndex={0}
                onClick={() => onOpen(project.id, action.task?.id)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    if (event.key === " ") event.preventDefault();
                    onOpen(project.id, action.task?.id);
                  }
                }}
              >
                <div className="attention-item-head">
                  <span className="attention-kind">
                    {attention ? PROJECT_ATTENTION_LABELS[attention] : "待处理"}
                  </span>
                  <h2>{project.name}</h2>
                  <span className="muted">
                    已等待 {formatElapsed(waitingMs)}
                  </span>
                </div>
                <p className="goal-line">
                  {project.goal || "尚未输入项目目标"}
                </p>
                <div className="meta-line">
                  {action.task ? (
                    <span>
                      任务 {action.task.key} ·{" "}
                      {action.task.title}
                    </span>
                  ) : null}
                  <span>
                    最近变化 {formatTime(project.updated_at)}
                  </span>
                  {affectedTasks !== undefined ? <span>影响后继 {affectedTasks} 项 · {blocksMilestone ? "阻挡里程碑" : "无里程碑阻挡"} · {projectAdvancing ? "项目仍可推进" : "等待人工处理"}</span> : null}
                  {action.reason ? (
                    <span>阻塞：{firstLine(action.reason)}</span>
                  ) : null}
                  <span>
                    {project.workspace_type === "plain"
                      ? "普通工作区"
                      : `${project.base_branch} → ${project.branch}`}
                  </span>
                </div>
              </article>
            );
          })}
        </div>
      )}
    </div>
  );
}

function firstLine(value: string, max = 160): string {
  const line =
    value
      .split(/\r?\n/)
      .map((item) => item.trim())
      .find((item) => item.length > 0) ?? "";
  const chars = Array.from(line);
  return chars.length <= max ? line : `${chars.slice(0, max - 1).join("")}…`;
}
