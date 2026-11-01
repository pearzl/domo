import type { ProjectDetail } from "../types";
import { ProjectGoal } from "./ProjectOverview";
import { CopyButton } from "./CopyButton";
import { RichContent } from "./RichContent";
import { StageBadge, StatusBadge, formatTime } from "../ui";

export interface ProjectHeaderProps {
  detail: ProjectDetail;
  canReplan: boolean;
  busy: boolean;
  merging: boolean;
  mergeNotice: { tone: "ok" | "error"; text: string } | null;
  onResume: () => void;
  onPause: () => void;
  onForcePause: () => void;
  onToggleReplan: () => void;
  onArchive: () => void;
  onMerge: () => void;
}

/// 项目页头部：标题、阶段与状态条、目标、项目源信息与交付横幅。
/// 项目操作按钮只负责回传意图，确认与请求留在页面层，保持原有行为。
export function ProjectHeader({
  detail,
  canReplan,
  busy,
  merging,
  mergeNotice,
  onResume,
  onPause,
  onForcePause,
  onToggleReplan,
  onArchive,
  onMerge,
}: ProjectHeaderProps) {
  const { project, tasks, jobs } = detail;
  const mergedIntoBase = detail.merged_into_base;
  const archived = project.status === "archived";
  const paused = project.status === "paused" || project.status === "pausing";
  const activeTasks = tasks.filter((task) => task.status !== "canceled");
  const taskCompleted = activeTasks.filter(
    (task) => task.stage === "completed",
  ).length;
  const runningJobs = jobs.filter((job) => job.status === "running").length;
  return (
    <header className="project-header panel">
      <div className="project-heading">
        <div>
          <h1>{project.name}</h1>
          <ProjectStatusBar
            stage={project.stage}
            status={project.status}
            taskTotal={activeTasks.length}
            taskCompleted={taskCompleted}
            runningJobs={runningJobs}
          />
        </div>
        <div className="header-actions">
          {!archived && project.stage !== "completed" ? (
            paused ? (
              <button className="secondary" disabled={busy} onClick={onResume}>
                恢复
              </button>
            ) : (
              <>
                <button className="secondary" disabled={busy} onClick={onPause}>
                  暂停
                </button>
                <button className="danger" disabled={busy} onClick={onForcePause}>
                  强制暂停
                </button>
              </>
            )
          ) : null}
          {canReplan ? (
            <button className="secondary" disabled={busy} onClick={onToggleReplan}>
              重新规划
            </button>
          ) : null}
          {!archived ? (
            <button className="danger" disabled={busy} onClick={onArchive}>
              归档
            </button>
          ) : null}
        </div>
      </div>
      <ProjectGoal goal={project.goal} />
      <ProjectSourceInfo
        repoPath={project.repo_path}
        baseBranch={project.base_branch}
        branch={project.branch}
        workspace={project.workspace}
      />
      {project.blocked_reason ? (
        <div className="blocked-banner">
          <strong>{project.status === "blocked" ? "阻塞原因" : "提示"}</strong>
          <RichContent content={project.blocked_reason} />
        </div>
      ) : null}
      {project.stage === "completed" ? (
        <div className="success-banner merge-banner">
          <div>
            {project.merged_at && mergedIntoBase ? (
              <>
                已合入 <span className="mono">{project.base_branch}</span>（
                {formatTime(project.merged_at)}，提交{" "}
                <span className="mono">
                  {project.merged_commit?.slice(0, 7) ?? "—"}
                </span>
                ）。项目分支 <span className="mono">{project.branch}</span> 已 rebase
                到目标分支。
              </>
            ) : mergedIntoBase ? (
              <>
                目标分支 <span className="mono">{project.base_branch}</span> 已包含项目分支{" "}
                <span className="mono">{project.branch}</span> 的全部提交。
              </>
            ) : archived ? (
              <>
                未合入，项目分支 <span className="mono">{project.branch}</span>{" "}
                已归档。归档 ref：
                <span className="mono">
                  {`refs/domo-archive/heads/${project.branch}`}
                </span>
                <br />
                恢复命令：
                <span className="mono">
                  {`git branch ${project.branch} refs/domo-archive/heads/${project.branch}`}
                </span>
              </>
            ) : (
              <>
                项目已完成。改动位于 <span className="mono">{project.branch}</span>
                ，请审阅后合入 <span className="mono">{project.base_branch}</span>。
              </>
            )}
          </div>
          {!mergedIntoBase && !archived ? (
            <div className="button-row merge-actions">
              <button className="primary" disabled={busy || merging} onClick={onMerge}>
                {merging ? "合入中…" : `合入到 ${project.base_branch}`}
              </button>
            </div>
          ) : null}
          {mergeNotice ? (
            <div
              className={`merge-result ${
                mergeNotice.tone === "error" ? "merge-error" : "merge-ok"
              }`}
            >
              <RichContent content={mergeNotice.text} />
            </div>
          ) : null}
        </div>
      ) : null}
    </header>
  );
}

/// 标题下的状态条：阶段、状态、任务进度与当前运行作业数。
/// 阶段与状态徽标以取值为 key，刷新后状态变化即重挂载并淡入。
export function ProjectStatusBar({
  stage,
  status,
  taskTotal,
  taskCompleted,
  runningJobs,
}: {
  stage: string;
  status: string | null;
  taskTotal: number;
  taskCompleted: number;
  runningJobs: number;
}) {
  const percent = taskTotal > 0 ? Math.round((taskCompleted / taskTotal) * 100) : 0;
  return (
    <div className="project-status-bar" role="group" aria-label="项目状态">
      <span
        key={`stage-${stage}`}
        className="project-status-item project-status-fade"
      >
        <StageBadge stage={stage} />
      </span>
      {status ? (
        <span
          key={`status-${status}`}
          className="project-status-item project-status-fade"
        >
          <StatusBadge status={status} />
        </span>
      ) : null}
      <span className="project-status-item">
        <span className="meta-label">任务进度</span>
        <span className="project-status-value">
          {taskCompleted}/{taskTotal}
          <span className="muted"> · {percent}%</span>
        </span>
      </span>
      <span className="project-status-item">
        <span className="meta-label">运行作业</span>
        <span className="project-status-value">{runningJobs} 个</span>
      </span>
    </div>
  );
}

/// 项目源信息默认折叠，仓库、交付分支与工作区均以等宽字体展示并可单独复制。
export function ProjectSourceInfo({
  repoPath,
  baseBranch,
  branch,
  workspace,
}: {
  repoPath: string;
  baseBranch: string;
  branch: string;
  workspace: string;
}) {
  const entries: { label: string; value: string }[] = [
    { label: "仓库", value: repoPath },
    { label: "交付分支", value: `${baseBranch} → ${branch}` },
    { label: "工作区", value: workspace },
  ];
  return (
    <details className="project-source">
      <summary>项目源信息</summary>
      <div className="project-source-grid">
        {entries.map((entry) => (
          <div className="project-source-row" key={entry.label}>
            <span className="meta-label">{entry.label}</span>
            <span className="mono project-source-value">{entry.value}</span>
            <CopyButton value={entry.value} label={entry.label} />
          </div>
        ))}
      </div>
    </details>
  );
}
