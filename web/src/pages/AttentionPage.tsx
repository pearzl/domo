import { useEffect, useMemo, useState } from "react";
import type { ProjectSummary } from "../types";
import { api, errorMessage } from "../api";
import { PROJECT_ATTENTION_LABELS } from "../projectFilter";
import { attentionItems } from "../presentation/attention";
import { formatElapsed } from "../presentation/projectStatus";
import { Empty, ErrorBox, Icon, PageHeader, formatTime } from "../ui";
import { SkeletonLines } from "../components/Skeleton";

const REFRESH_MS = 5000;

/// 待我处理：集中展示所有需要人工驱动的项目，默认按等待时长倒序排列。
/// 数据来自项目列表的待办口径，处理完成后项目离开列表，导航计数同步减少。
export function AttentionPage({ onOpen }: { onOpen: (id: string, taskId?: string) => void }) {
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [now, setNow] = useState(() => Date.now());
  const [reloadKey, setReloadKey] = useState(0);

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
  }, [reloadKey]);

  const items = useMemo(
    () => attentionItems(projects, now),
    [projects, now],
  );

  return (
    <div className="page">
      <PageHeader icon="attention" eyebrow="工作空间 / 需要你的决策" title="待我处理"
        description="集中处理回答、确认、验收与裁决，按等待时间优先查看。"
        actions={<span className="page-count" role="status">{loading ? "加载中…" : loadError && items.length === 0 ? "待办暂不可用" : `${items.length} 项待办`}</span>}
      />

      {loadError ? (
        <ErrorBox message={loadError} onRetry={() => setReloadKey((value) => value + 1)} retrying={loading} />
      ) : null}

      {loading ? (
        <div className="panel">
          <SkeletonLines label="加载中…" />
        </div>
      ) : items.length === 0 && loadError ? null : items.length === 0 ? (
        <Empty title="没有待处理事项" hint="需要你驱动的项目会出现在这里。" />
      ) : (
        <div className="attention-list">
          {items.map(({ project, action, waitingMs, id, affectedTasks, blocksMilestone, projectAdvancing }) => {
            const attention = action.kind;
            return (
              <article
                key={id ?? project.id}
                className="panel attention-item"
                data-tone={attention === "failed" || attention === "blocked" ? "danger" : "warn"}
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
                  <span className="attention-waiting">
                    已等待 {formatElapsed(waitingMs)}
                  </span>
                  <span className="attention-open">前往处理 <Icon name="arrow" /></span>
                </div>
                <p className="goal-line">
                  {project.goal || "尚未输入项目目标"}
                </p>
                {action.reason ? <p className="attention-reason">{firstLine(action.reason)}</p> : null}
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
