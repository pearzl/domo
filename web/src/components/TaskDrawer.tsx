/**
 * 任务抽屉：控制、结果、验收、历史、依赖与配置的呈现顺序由布局模块决定。
 * 抽屉按任务重建，草稿与焦点还原都只作用于当前任务。
 */
import { useEffect, useId, useRef, useState } from "react";
import type { ProjectDetail, Task } from "../types";
import { api, errorMessage } from "../api";
import { InputBox } from "./InputBox";
import { useDraftValue } from "./useDraft";
import { JobDetailView, JobRecordList } from "./JobDetailView";
import { TaskHistoryBlock } from "./TaskHistory";
import { RichContent } from "./RichContent";
import {
  ResultSummary,
  VerificationRecapBlock,
  useVerificationHistory,
} from "./TaskVerification";
import { TaskAgentPoolPanel } from "./TaskAgentPoolPanel";
import { HumanAcceptancePanel } from "./HumanAcceptancePanel";
import { ArbitrationPanel } from "./ArbitrationPanel";
import { taskHistory } from "../presentation/taskHistory";
import { latestFailedJobId } from "../presentation/jobSelection";
import { isReordableQueueTask, reordableQueueIds } from "../taskQueue";
import { queueSummary } from "../presentation/taskList";
import { verificationRecap } from "../presentation/taskSummary";
import {
  DRAWER_ACCEPTANCE_TITLE,
  DRAWER_CONTENT_SECTION_CLASS,
  DRAWER_DESCRIPTION_TITLE,
  DRAWER_FEEDBACK_TITLE,
  DRAWER_LEAD_TITLES,
  DEPS_FIELD_LABELS,
  DEPS_PRIMARY_FIELDS,
  acceptanceMarkdown,
  depsFoldedFields,
  drawerLayout,
  hasVerifierFeedback,
  rawErrorEntry,
  verifierFeedback,
  type DepsField,
  type DrawerTailSection,
} from "../presentation/taskDrawer";
import {
  Badge,
  FAILURE_KIND_LABELS,
  TaskStateBadge,
} from "../ui";

/// 与后端在回收工作区前写入的标记保持一致。
const ARTIFACTS_RELEASING = "releasing";

export function TaskDrawer({
  task,
  detail,
  onClose,
  onRetry,
  onRetryJob,
  onSaveGuidance,
  onToggleMilestone,
  onRefresh,
  archived,
  busy,
  agentName,
  snapshotVersion = 0,
}: {
  task: Task;
  detail: ProjectDetail;
  onClose: () => void;
  onRetry: () => void;
  onRetryJob: (jobId: string) => void;
  onSaveGuidance: (guidance: string) => Promise<boolean>;
  onToggleMilestone: () => void;
  onRefresh: () => Promise<void>;
  archived: boolean;
  busy: boolean;
  agentName: (id: string | null) => string;
  /** 项目快照版本：验收记录随任务与作业一起刷新。 */
  snapshotVersion?: number;
}) {
  const isGit = detail.project.workspace_type !== "plain";
  const jobs = detail.jobs.filter((job) => job.task_id === task.id);
  // 验收摘要优先取逐轮验收记录；接口不可用时呈现层回退到任务字段并标注来源。
  const verificationHistory = useVerificationHistory(
    task.project_id,
    task.id,
    snapshotVersion,
  );
  // 执行与验收历史只按轮次对齐作业元信息，结果与原因长文本在作业视图中查看。
  const history = taskHistory(
    jobs,
    verificationHistory.history,
    verificationHistory.state,
  );
  // 跳转到作业详情：记录目标作业与跳转序号，作业记录列表据此展示并展开该作业。
  const [focusJob, setFocusJob] = useState<{ jobId: string; nonce: number }>({
    jobId: "",
    nonce: 0,
  });
  const jumpToJob = (jobId: string) =>
    setFocusJob((current) => ({ jobId, nonce: current.nonce + 1 }));

  const artifactsReleasing = task.artifacts_released_at === ARTIFACTS_RELEASING;
  const artifactsReleased = Boolean(task.artifacts_released_at);
  const archiveReference = task.branch
    ? `refs/domo-archive/heads/${task.branch}`
    : null;
  const archiveRecovery =
    task.branch && archiveReference
      ? `git branch ${task.branch} ${archiveReference}`
      : null;
  const retryableJobId =
    !archived && task.status === "blocked" && task.human_gate !== "milestone"
      ? latestFailedJobId(jobs)
      : null;
  const children = detail.tasks.filter((item) => item.parent_id === task.id);
  const parent = task.parent_id
    ? detail.tasks.find((item) => item.id === task.parent_id) ?? null
    : null;
  const childCompleted = children.filter(
    (item) => item.stage === "completed",
  ).length;
  const waitingChildren =
    children.length > 0 &&
    task.stage === "pending" &&
    childCompleted < children.length;
  const guidanceDraft = useDraftValue(
    { projectId: task.project_id, taskId: task.id, field: "guidance" },
    task.guidance,
  );
  const guidance = guidanceDraft.value;
  const setGuidance = guidanceDraft.setValue;
  const [controlBusy, setControlBusy] = useState(false);
  const [controlError, setControlError] = useState("");
  const guidanceLocked = archived || task.stage === "completed" || task.status === "canceled";
  const taskPaused = task.status === "paused" || task.status === "pausing";
  // 首区块按状态决定：blocked 与里程碑人工验收先裁决，已完成/已取消先给结果，其余进行中先给控制条。
  const layout = drawerLayout({ ...task, archived });
  // 裁决面板与原始错误入口已经呈现失败原因，验收摘要不再重复同一条原文。
  const verificationRecapValue = verificationRecap(
    {
      ...task,
      last_error: layout.gateCarriesError || rawErrorEntry(layout) === "collapsed"
        ? null
        : task.last_error,
    },
    verificationHistory.history,
    verificationHistory.state,
  );
  const resultFirst = layout.lead === "result";
  const canControl =
    !archived && task.stage !== "completed" && task.status !== "canceled";
  const canReorder = isReordableQueueTask(task, archived);
  const edge = queueSummary(reordableQueueIds(detail.tasks, archived), task.id);
  const titleId = useId();
  const drawerRef = useRef<HTMLElement | null>(null);
  const closeRef = useRef<HTMLButtonElement | null>(null);
  // 打开时记录焦点来源，关闭后把焦点还给打开前的任务行或卡片。
  const returnFocusRef = useRef<HTMLElement | null>(null);
  useEffect(() => {
    const active = document.activeElement;
    returnFocusRef.current = active instanceof HTMLElement ? active : null;
    closeRef.current?.focus();
    return () => {
      const target = returnFocusRef.current;
      if (target?.isConnected) target.focus();
      returnFocusRef.current = null;
    };
  }, []);
  // 抽屉内 Tab 循环，Esc 关闭；焦点不会被背后的页面接走。
  function handleDrawerKeyDown(event: React.KeyboardEvent<HTMLElement>) {
    if (event.key === "Escape") {
      event.stopPropagation();
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const drawer = drawerRef.current;
    if (!drawer) return;
    const focusable = Array.from(
      drawer.querySelectorAll<HTMLElement>(
        'a[href], button, input, select, textarea, [tabindex]:not([tabindex="-1"])',
      ),
    ).filter(
      (element) =>
        !element.hasAttribute("disabled") &&
        element.getAttribute("aria-hidden") !== "true" &&
        element.tabIndex !== -1 &&
        element.getClientRects().length > 0,
    );
    if (focusable.length === 0) {
      event.preventDefault();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const current = document.activeElement;
    if (event.shiftKey) {
      if (current === first || !drawer.contains(current)) {
        event.preventDefault();
        last.focus();
      }
    } else if (current === last || !drawer.contains(current)) {
      event.preventDefault();
      first.focus();
    }
  }
  async function control(action: () => Promise<unknown>) {
    setControlBusy(true);
    setControlError("");
    try {
      await action();
      await onRefresh();
    } catch (err) {
      setControlError(errorMessage(err));
    } finally {
      setControlBusy(false);
    }
  }
  // 结果摘要按 Markdown 排版，并标注来自哪一次执行作业；没有结果时给出空状态。
  const resultSection = (
    <ResultSummary taskId={task.id} summary={task.result_summary} jobs={jobs} />
  );
  const verificationSection = <>
    <VerificationRecapBlock recap={verificationRecapValue} />
    {verificationHistory.history?.verifications.filter(record => record.evidence).map(record => (
      <details key={record.id}><summary>第 {record.round} 轮验收证据 · 契约 #{record.evidence!.contract_id}</summary>
        <p>产物版本：{record.evidence!.artifact_version ?? "普通工作区当前产物，不提供历史内容恢复"}</p>
        <p>检查时间：{record.finished_at ?? "—"}</p>
        {record.evidence!.criteria.map((criterion, index) => <div key={index}>
          <strong>{criterion.passed ? "通过" : "拒绝"} · {criterion.criterion}</strong>
          <ul>{criterion.checks.map((check, i) => <li key={i}>{check.method} → {check.result}</li>)}</ul>
        </div>)}
        <p>已检查产物：{record.evidence!.artifacts.join("、") || "无文件产物"}</p>
        <p>未覆盖风险：{record.evidence!.risks.join("；") || "无声明"}</p>
        <p>人工检查项：{record.evidence!.human_checks.join("；") || "无声明"}</p>
      </details>
    ))}
  </>;
  // 作业记录：从执行历史跳转时，目标作业即使不在最近几条也保持可见并展开。
  const jobsSection = (
    <section>
      <h3>作业（{jobs.length}）</h3>
      <JobRecordList
        jobs={jobs}
        revealJobId={focusJob.jobId || null}
        renderJob={(job) => (
          <JobDetailView
            key={job.id}
            job={job}
            retryable={job.id === retryableJobId}
            readOnly={archived}
            onRetry={() => onRetryJob(job.id)}
            focusNonce={job.id === focusJob.jobId ? focusJob.nonce : 0}
          />
        )}
      />
    </section>
  );
  const historySection = (
    <TaskHistoryBlock history={history} onJumpToJob={jumpToJob} />
  );
  const feedbackSection =
    hasVerifierFeedback(task) ? (
      <section className="task-feedback" data-drawer-block="feedback">
        <h3>{DRAWER_FEEDBACK_TITLE}</h3>
        <ul>
          {verifierFeedback(task.verifier_issues).map((issue, index) => (
            <li key={`${index}-${issue}`}>
              <RichContent content={issue} />
            </li>
          ))}
        </ul>
      </section>
    ) : null;
  // 任务描述与验收标准同级并列：同一个容器类、同一级标题、同一套 Markdown 排版。
  const descriptionSection = (
    <section className={DRAWER_CONTENT_SECTION_CLASS} data-drawer-block="description">
      <h3>{DRAWER_DESCRIPTION_TITLE}</h3>
      <RichContent
        content={task.description}
        format="markdown"
        emptyText="没有记录任务描述。"
      />
    </section>
  );
  const acceptanceSection = (
    <section className={DRAWER_CONTENT_SECTION_CLASS} data-drawer-block="acceptance">
      <h3>{DRAWER_ACCEPTANCE_TITLE}</h3>
      <RichContent
        content={acceptanceMarkdown(task.acceptance)}
        format="markdown"
        emptyText="没有记录验收标准。"
      />
    </section>
  );
  const deriveSection =
    parent || children.length > 0 ? (
      <section>
        <h3>派生关系</h3>
        {parent ? (
          <>
            <div className="kv">
              <span>父任务</span>
              <span>
                {parent.key} {parent.title}
              </span>
            </div>
            <p className="muted">
              {isGit
                ? "派生任务跳过验收，执行成功后直接提交并合入父任务分支。"
                : "派生任务跳过验收，执行成功后产物直接保留在普通工作区。"}
            </p>
          </>
        ) : null}
        {children.length > 0 ? (
          <>
            <div className="kv">
              <span>子任务</span>
              <span>
                {childCompleted}/{children.length} 已完成
                {waitingChildren ? "，父任务等待中" : ""}
              </span>
            </div>
            <div className="derive-list">
              {children.map((child) => (
                <div className="derive-child" key={child.id}>
                  <div className="derive-child-head">
                    <span className="task-key">{child.key}</span>
                    <span className="derive-child-title">{child.title}</span>
                    <TaskStateBadge stage={child.stage} status={child.status} />
                  </div>
                  <div className="muted">
                    <RichContent
                      content={child.result_summary || child.last_error}
                      emptyText="尚无结果"
                    />
                  </div>
                </div>
              ))}
            </div>
          </>
        ) : null}
      </section>
    ) : null;
  // 依赖与位置分层：依赖、排队位置与等待原因常显，其余字段折叠在“更多位置信息”中。
  const depsRow = (field: DepsField) => {
    const label = DEPS_FIELD_LABELS[field];
    switch (field) {
      case "deps":
        return (
          <div className="kv" key={field}>
            <span>{label}</span>
            <span>{task.deps.length ? task.deps.join(", ") : "无"}</span>
          </div>
        );
      case "queue":
        return (
          <div className="kv" key={field}>
            <span>{label}</span>
            <span>
              {edge.position > 0
                ? `可调整队列第 ${edge.position}/${edge.total} 位`
                : task.queue_position !== null
                  ? `派发队列第 ${task.queue_position} 位`
                  : "未在队列中"}
            </span>
          </div>
        );
      case "waitingReason":
        return (
          <div className="kv" key={field}>
            <span>{label}</span>
            <span>{task.waiting_reason ?? "—"}</span>
          </div>
        );
      case "failureKind":
        return (
          <div className="kv" key={field}>
            <span>{label}</span>
            <span>
              {task.last_failure_kind
                ? FAILURE_KIND_LABELS[task.last_failure_kind] ?? task.last_failure_kind
                : "—"}
            </span>
          </div>
        );
      case "agent":
        return (
          <div className="kv" key={field}>
            <span>{label}</span>
            <span>{agentName(task.agent_id)}</span>
          </div>
        );
      case "branch":
        return (
          <div className="kv" key={field}>
            <span>{label}</span>
            <span className="mono">{task.branch ?? "—"}</span>
          </div>
        );
      case "worktree":
        return (
          <div className="kv" key={field}>
            <span>{label}</span>
            <span className="badge-row">
              <span className="mono">
                {task.worktree ??
                  (artifactsReleasing
                    ? "回收中"
                    : artifactsReleased
                      ? "已回收"
                      : "尚未创建")}
              </span>
              {task.worktree && artifactsReleased ? (
                <Badge
                  tone="muted"
                  title="工作区已回收，分支提交保留在 refs/domo-archive"
                >
                  {artifactsReleasing ? "回收中" : "已回收"}
                </Badge>
              ) : null}
            </span>
          </div>
        );
      case "archiveRef":
        return (
          <div className="kv" key={field}>
            <span>{label}</span>
            <span className="mono">{archiveReference}</span>
          </div>
        );
      case "archiveRecovery":
        return (
          <div className="kv" key={field}>
            <span>{label}</span>
            <span className="mono">{archiveRecovery}</span>
          </div>
        );
    }
  };
  const depsFolded = depsFoldedFields({
    archiveReference: Boolean(archiveReference && artifactsReleased && !artifactsReleasing),
    archiveRecovery: Boolean(artifactsReleased && !artifactsReleasing && archiveRecovery),
  }).filter(
    (field) =>
      isGit ||
      (field !== "branch" && field !== "worktree" && field !== "archiveRef" &&
        field !== "archiveRecovery"),
  );
  const depsSection = (
    <section className="task-deps">
      <h3>依赖与位置</h3>
      {DEPS_PRIMARY_FIELDS.map(depsRow)}
      <details className="drawer-fold deps-fold">
        <summary>更多位置信息</summary>
        {depsFolded.map(depsRow)}
      </details>
    </section>
  );
  const rawErrorSection =
    rawErrorEntry(layout) === "collapsed" && task.last_error ? (
      <details className="raw-error drawer-fold" key="rawError">
        <summary>查看原始错误</summary>
        <div className="error-box">
          <RichContent content={task.last_error} />
        </div>
      </details>
    ) : null;
  const saveGuidance = async () => {
    const ok = await onSaveGuidance(guidance);
    if (ok) guidanceDraft.clear(guidance);
  };
  const guidanceEditor = (
    <section>
      <h3>人工指导</h3>
      <InputBox
        value={guidance}
        onChange={setGuidance}
        draftKey={guidanceDraft.key}
        rows={4}
        maxAutoGrowChars={520}
        disabled={guidanceLocked || busy}
        placeholder="指导会在该任务下一次执行作业时生效；保存后，执行者会带着这段说明开始下一次作业。"
        actions={
          <div className="button-row guidance-actions">
            <button
              className="primary"
              disabled={
                guidanceLocked ||
                busy ||
                !guidance.trim() ||
                guidance.trim() === task.guidance.trim()
              }
              onClick={() => void saveGuidance()}
            >
              保存指导
            </button>
            {guidanceLocked ? (
              <span className="muted">
                {task.status === "canceled"
                  ? "任务已取消，不能再编辑指导。"
                  : task.stage === "completed"
                    ? "任务已完成，不能再编辑指导。"
                    : "项目已归档，不能再编辑指导。"}
              </span>
            ) : null}
          </div>
        }
      />
    </section>
  );
  const guidanceReadonly = (
    <details className="drawer-fold">
      <summary>人工指导</summary>
      <RichContent content={task.guidance} emptyText="没有人工指导。" />
    </details>
  );
  const agentPoolSection = (
    <TaskAgentPoolPanel
      task={task}
      activeRole="executor"
      onRefresh={onRefresh}
      disabled={archived || task.stage === "completed" || task.status === "canceled"}
    />
  );
  const controlSection = canControl ? (
    <div className="task-control-bar">
      {taskPaused ? (
        <button
          className="secondary"
          disabled={controlBusy}
          onClick={() => void control(() => api.resumeTask(task.project_id, task.id))}
        >
          继续
        </button>
      ) : (
        <>
          <button
            className="secondary"
            disabled={controlBusy}
            onClick={() => void control(() => api.pauseTask(task.project_id, task.id))}
          >
            暂停
          </button>
          <button
            className="danger"
            disabled={controlBusy}
            onClick={() => {
              if (window.confirm("强制暂停会终止当前作业；之后点“继续”重新派发。确认继续？")) {
                void control(() => api.pauseTask(task.project_id, task.id, true));
              }
            }}
          >
            强制暂停
          </button>
        </>
      )}
      {canReorder ? (
        <>
          <button
            className="ghost"
            disabled={controlBusy || !edge.canMoveUp}
            aria-label={`${task.key} 排队上移`}
            title={edge.canMoveUp ? "上移一位" : "已经在可调整队列首位"}
            onClick={() =>
              void control(() => api.reorderTaskQueue(task.project_id, task.id, "up"))
            }
          >
            上移
          </button>
          <button
            className="ghost"
            disabled={controlBusy || !edge.canMoveDown}
            aria-label={`${task.key} 排队下移`}
            title={edge.canMoveDown ? "下移一位" : "已经在可调整队列末位"}
            onClick={() =>
              void control(() => api.reorderTaskQueue(task.project_id, task.id, "down"))
            }
          >
            下移
          </button>
        </>
      ) : null}
    </div>
  ) : null;
  const milestoneSection = (
    <div className="milestone-bar">
      <span className="muted">
        {task.milestone
          ? "验收者通过后不会自动合并，需要人工确认。"
          : "把任务标记为里程碑，验收通过后交给人工确认。"}
      </span>
      <button
        className={task.milestone ? "secondary" : "ghost"}
        disabled={
          archived ||
          busy ||
          task.stage === "completed" ||
          task.status === "canceled" ||
          task.human_gate === "milestone"
        }
        onClick={onToggleMilestone}
      >
        {task.milestone ? "取消里程碑" : "标记为里程碑"}
      </button>
    </div>
  );
  const gateSection =
    task.human_gate === "milestone" ? (
      <HumanAcceptancePanel
        task={task}
        projectId={task.project_id}
        onRefresh={onRefresh}
        disabled={archived}
      />
    ) : task.status === "blocked" ? (
      <ArbitrationPanel
        projectId={task.project_id}
        task={task}
        onRefresh={onRefresh}
        disabled={archived}
        onLegacyRetry={onRetry}
      />
    ) : null;
  // 首区块：gate 由裁决/人工验收面板承载，result 由结果摘要承载，control 由控制条承载。
  const controlBlock = (
    <section className="task-control" aria-label={DRAWER_LEAD_TITLES.control}>
      <h3>控制</h3>
      {controlSection ?? <p className="muted">该任务当前没有可用的控制操作。</p>}
      {controlError ? <div className="error-box">{controlError}</div> : null}
    </section>
  );
  const guidanceSection = guidanceLocked ? guidanceReadonly : guidanceEditor;
  const tailBlock = (key: DrawerTailSection) => {
    switch (key) {
      case "control":
        return <div key={key}>{controlBlock}</div>;
      case "verification":
        return <div key={key}>{verificationSection}</div>;
      case "history":
        return <div key={key}>{historySection}</div>;
      case "milestone":
        return <div key={key}>{milestoneSection}</div>;
      case "result":
        return <div key={key}>{resultSection}</div>;
      case "description":
        return <div key={key}>{descriptionSection}</div>;
      case "acceptance":
        return <div key={key}>{acceptanceSection}</div>;
      case "feedback":
        return feedbackSection ? <div key={key}>{feedbackSection}</div> : null;
      case "derive":
        return deriveSection ? <div key={key}>{deriveSection}</div> : null;
      case "deps":
        return <div key={key}>{depsSection}</div>;
      case "rawError":
        return rawErrorSection;
      case "guidance":
        return <div key={key}>{guidanceSection}</div>;
      case "jobs":
        return <div key={key}>{jobsSection}</div>;
      case "agentPool":
        return (
          <section className="drawer-config" key={key}>
            <h3>配置</h3>
            {agentPoolSection}
          </section>
        );
    }
  };
  return (
    <div className="drawer-backdrop" onClick={onClose}>
      <aside
        className="drawer"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        data-drawer-lead={layout.lead}
        ref={drawerRef}
        onKeyDown={handleDrawerKeyDown}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="drawer-header">
          <div>
            <div className="muted">{task.key}</div>
            <h2 id={titleId}>{task.title}</h2>
          </div>
          <button className="ghost" ref={closeRef} onClick={onClose}>
            关闭
          </button>
        </div>
        <div className="drawer-body">
          <div className="badge-row">
            <TaskStateBadge stage={task.stage} status={task.status} />
            {task.attempts ? <Badge tone="warn">执行 {task.attempts} 次</Badge> : null}
            {task.rejections ? <Badge tone="warn">拒绝 {task.rejections} 次</Badge> : null}
            {!resultFirst && task.last_failure_kind ? (
              <Badge tone="danger">
                失败分类 ·{" "}
                {FAILURE_KIND_LABELS[task.last_failure_kind] ?? task.last_failure_kind}
              </Badge>
            ) : null}
            {!resultFirst && task.parent_id ? (
              <Badge tone="info">派生任务 · 跳过验收</Badge>
            ) : null}
            {!resultFirst && task.milestone ? (
              <Badge tone="warn">里程碑 · 需人工验收</Badge>
            ) : null}
            {!resultFirst && layout.gateCarriesError ? (
              <Badge tone="danger">等待人工验收</Badge>
            ) : null}
            {!resultFirst && waitingChildren ? (
              <Badge tone="warn">
                等待子任务 {childCompleted}/{children.length}
              </Badge>
            ) : null}
          </div>
          <div className="drawer-lead" data-lead={layout.lead}>
            {layout.lead === "gate" ? gateSection : null}
            {layout.lead === "result" ? resultSection : null}
            {layout.lead === "control" ? controlBlock : null}
          </div>
          {layout.tail.map((key) => tailBlock(key))}
        </div>
      </aside>
    </div>
  );
}
