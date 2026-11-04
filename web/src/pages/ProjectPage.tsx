/**
 * 项目详情页。主体选项卡、任务抽屉与作业定位同步到 URL 查询参数，
 * 刷新、复制链接与浏览器前进后退都回到同一视图。
 *
 * 人工核对清单（输入持久化）：
 * 1. 在“项目分析”输入框输入文字，刷新页面，内容仍在。
 * 2. 输入文字后离开项目页，再后退/前进回到项目页，内容仍在。
 * 3. 成功发送后输入框清空，刷新后仍为空。
 * 4. 打开任务抽屉或切换主体选项卡，地址栏出现 tab/task 参数；刷新与后退后视图一致。
 * 5. 方向键在选项卡内左右切换，Home/End 跳到首尾；每项徽标随任务、方案与报告状态更新。
 * 6. 概览里的用量摘要显示作业数、token、费用、时长与覆盖；没有记录的指标显示“—”，
 *    “查看用量明细”带着项目过滤跳到用量页；项目页不再有独立的用量面板。
 * 7. 自动确认开关行显示当前状态；方案卡片的各小节可折叠，确认按钮与“确认后”影响摘要同处卡片底部。
 *
 * 人工核对清单（动效）：
 * 1. 依次切换全部选项卡：面板淡入上移约 4px，上方选项卡与下方 Agent 池位置不动，无布局跳动。
 * 2. 切换过程中重复点击、用方向键快速切换：动画不叠加、不闪白，页面不出现滚动条抖动。
 * 3. 展开活动与审计时间线：条目淡入，列表自动跟随底部，“回到最新”出现时不推动页面。
 * 4. 概览用量摘要与页头进度在数值变化时逐位滚动，刷新后首次展示保持静态。
 * 5. 开启系统“减少动效”：以上动画全部只淡入不位移，状态点与骨架不再循环。
 */
import { api } from "../api";
import { DeliveryPanel } from "../components/DeliveryPanel";
import { ResourcePanel } from "../components/ResourcePanel";
import { SkeletonLines } from "../components/Skeleton";
import { InputBox } from "../components/InputBox";
import { EventTimeline } from "../components/EventTimeline";
import { JobDetailView, JobRecordList } from "../components/JobDetailView";
import { ProjectOverview } from "../components/ProjectOverview";
import { ProjectTopBlocks } from "../components/ProjectTopBlocks";
import { ProjectHeader } from "../components/ProjectHeader";
import { ProjectTabs, tabButtonId, tabPanelId } from "../components/ProjectTabs";
import { AutoConfirmRow, ProposalCard } from "../components/ProposalCard";
import {
  autoConfirmLocked,
  confirmModeRuntimeNote,
} from "../presentation/proposalView";
import { RichContent } from "../components/RichContent";
import { ProjectTasksSection } from "../components/ProjectTasksSection";
import { ProjectReportPanel } from "../components/ProjectReportPanel";
import { ReplanPanel } from "../components/ReplanPanel";
import { GoalEditPanel } from "../components/GoalEditPanel";
import {
  AnalysisLiveActivity,
} from "../components/LiveActivitySummary";
import { TaskDrawer } from "../components/TaskDrawer";
import { useProjectPageController } from "../presentation/projectPageController";
import { projectPageView } from "../presentation/projectPageView";
import { projectBlockOrder, projectTabBadge } from "../presentation/projectLayout";
import type { GuidanceTab } from "../presentation/projectStatus";
import {
  Badge,
  ErrorBox,
  formatTime,
} from "../ui";

export { LiveActivitySummary } from "../components/LiveActivitySummary";
export { TaskDrawer } from "../components/TaskDrawer";

function replanSourceLabel(source: string | null): string {
  return source === "executor" ? "执行者" : source === "human" ? "人工" : "未知";
}

export function ProjectPage({
  id,
  onBack,
}: {
  id: string;
  onBack: () => void;
}) {
  const page = useProjectPageController({ id });
  const {
    detail,
    loading,
    error,
    busy,
    confirming,
    message,
    setMessage,
    analysisDraftKey,
    feedback,
    setFeedback,
    feedbackDraftKey,
    selectedTaskId,
    selectedJobId,
    replanOpen,
    replanReason,
    setReplanReason,
    replanDraftKey,
    goalEditOpen,
    goalValue,
    setGoalValue,
    goalDraftKey,
    addTaskOpen,
    taskForm,
    setTaskField,
    taskView,
    setTaskView,
    taskQuery,
    setTaskQuery,
    taskFilter,
    setTaskFilter,
    merging,
    mergeNotice,
    archiveRefs,
    snapshotVersion,
    refresh,
    run,
    selectTask,
    selectTab,
    openAddTaskForm,
    toggleAddTaskForm,
    handleArchive,
    handleMerge,
    handleAddTask,
    resetTaskFilter,
    sendAnalysis,
    confirmProposal,
    submitReport,
    requestReportReplan,
    abandonReplan,
    submitReplan,
    toggleReplan,
    cancelReplan,
    toggleGoalEdit,
    cancelGoalEdit,
    submitGoalEdit,
  } = page;

  if (loading)
    return (
      <div className="page">
        <SkeletonLines label="加载项目…" />
      </div>
    );
  if (!detail) {
    return (
      <div className="page">
        <button className="ghost" onClick={onBack}>
          ← 返回项目
        </button>
        <ErrorBox message={error || "项目不存在"} />
      </div>
    );
  }

  const view = projectPageView(detail, {
    activeTab: page.activeTab,
    selectedTaskId,
    taskQuery,
    taskFilter,
    taskDraft: taskForm,
    message,
    busy,
    confirming,
  });
  const {
    project,
    tasks,
    jobs,
    proposal,
    activeTasks,
    archived,
    paused,
    goalMessages,
    reviewMessages,
    analysisConversation,
    canReplan,
    replanEvents,
    agentName,
    projectJobs,
    analysisJob,
    directJob,
    directMode,
    retryableProjectJobId,
    queueIds,
    taskCounts,
    searchActive,
    draftCheck,
    depOptions,
    replanPreview,
    activeBlock,
    badgeInput,
    selectedTask,
    visibleTasks,
    confirmBlockedReason,
    confirmBusy,
  } = view;

  // 待办跳转保留在 URL 中；失败作业在信息页展开并定位。
  function openGuidanceTab(tab: GuidanceTab, jobId?: string) {
    selectTab(tab, jobId ?? null);
  }

  const blockOrder = projectBlockOrder(project.stage);

  const analysisBlock = project.stage === "analysis" ? (
    <section className="panel">
      <h2>项目分析</h2>
      <p className="subtitle">
        规划者会理解目标与现状，必要时澄清，给出可确认的方案：任务拆分、依赖和验收标准。确认方案后才开始执行。
      </p>
      {project.replan_active === 1 ? (
        <div className="confirm-row">
          <div className="button-row">
            <button
              className="secondary"
              disabled={busy || !!analysisJob || archived}
              title={analysisJob ? "规划者正在分析，完成后可放弃重规划" : undefined}
              onClick={() => void abandonReplan()}
            >
              放弃重规划并恢复旧目标与旧计划
            </button>
          </div>
          <span className="muted">旧目标、任务、报告和验收口径仍保留；恢复后继续原计划派发。</span>
        </div>
      ) : null}
      {project.replan_active === 1 ? null : (
        <AutoConfirmRow
          mode={project.confirm_mode}
          busy={busy}
          disabled={autoConfirmLocked(project.status)}
          note={confirmModeRuntimeNote(!!analysisJob)}
          onChange={(mode) =>
            void run(() => api.updateAnalysisPolicy(project.id, mode))
          }
        />
      )}
      <div className="messages">
        {analysisConversation.messages.map((item) =>
          item.folded ? (
            <details
              key={item.message.id}
              className="message message-planner message-folded"
            >
              <summary>
                <span className="message-role">规划者</span>
                <span className="muted">上一条分析结果 · 已折叠，点击展开</span>
              </summary>
              <RichContent
                content={item.message.content}
                className="message-content"
              />
            </details>
          ) : (
            <div
              key={item.message.id}
              className={`message message-${item.message.role}`}
            >
              <div className="message-role">
                {item.message.role === "user" ? "你" : "规划者"}
              </div>
              <RichContent
                content={item.message.content}
                className="message-content"
              />
            </div>
          ),
        )}
        {analysisConversation.waiting && project.status !== "blocked" ? (
          <div className="message-waiting" role="status">
            <span className="message-role">规划者</span>
            <span className="muted">
              {analysisConversation.waiting === "start"
                ? "等待规划者开始分析…"
                : "已收到你的反馈，等待规划者给出下一次分析结果…"}
            </span>
          </div>
        ) : null}
        {goalMessages.length === 0 ? (
          <div className="muted">还没有输入项目目标。</div>
        ) : null}
      </div>
      {analysisJob ? (
        <AnalysisLiveActivity jobId={analysisJob.id} />
      ) : null}
      {analysisJob ? (
        <div className="goal-running">
          <Badge tone="info">规划者正在分析</Badge>
          <span className="muted">
            {analysisJob.agent_name ?? "规划 Agent"} ·{" "}
            {formatTime(analysisJob.started_at)}
          </span>
        </div>
      ) : null}
      {proposal ? (
        <ProposalCard
          proposal={proposal}
          activeTasks={activeTasks}
          mode={project.confirm_mode}
          busy={confirmBusy}
          confirming={confirming}
          disabled={paused || archived}
          confirmDisabledReason={confirmBlockedReason}
          onConfirm={confirmProposal}
        />
      ) : null}
      {!analysisJob ? (
        proposal ? (
          <div className="confirm-row proposal-follow-up">
            <InputBox
              value={message}
              onChange={setMessage}
              draftKey={analysisDraftKey}
              disabled={busy || paused || archived}
              rows={3}
              maxAutoGrowChars={520}
              placeholder="对方案有不同意见时在这里补充，规划者会给出新的方案。"
              actions={
                <div className="button-row">
                  <button
                    className="secondary"
                    disabled={busy || paused || archived || !message.trim()}
                    onClick={sendAnalysis}
                  >
                    发送修改意见
                  </button>
                </div>
              }
            />
          </div>
        ) : (
          <div className="confirm-row">
            <InputBox
              value={message}
              onChange={setMessage}
              draftKey={analysisDraftKey}
              disabled={busy || paused || archived}
              rows={3}
              maxAutoGrowChars={520}
              placeholder={
                goalMessages.length === 0
                  ? "描述项目目标、边界和验收预期。"
                  : "补充信息、回答规划者的问题，或对方案提出修改意见。"
              }
              actions={
                <div className="button-row">
                  <button
                    className="secondary"
                    disabled={busy || paused || archived || !message.trim()}
                    onClick={sendAnalysis}
                  >
                    {goalMessages.length === 0 ? "开始分析" : "发送"}
                  </button>
                </div>
              }
            />
          </div>
        )
      ) : null}
    </section>
  ) : goalMessages.length > 1 ? (
    <details className="panel">
      <summary>项目分析记录</summary>
      <div className="messages compact">
        {goalMessages.map((item) => (
          <div key={item.id} className={`message message-${item.role}`}>
            <div className="message-role">{item.role === "user" ? "你" : "规划者"}</div>
            <RichContent content={item.content} className="message-content" />
          </div>
        ))}
      </div>
    </details>
  ) : null;

  // 信息页：参考类信息（重规划记录、用量、项目作业、归档）集中在信息选项卡。
  const infoBlock = (
    <div className="project-info">
      {project.replan_reason || replanEvents.length > 0 ? (
        <section className="panel">
          <h2>重规划记录</h2>
          {project.replan_reason ? (
            <div className="blocked-banner">
              <strong>
                最近一次重规划（来源：{replanSourceLabel(project.replan_source)}）
              </strong>
              <RichContent content={project.replan_reason} />
              {project.replan_requested_at ? (
                <div className="muted">{formatTime(project.replan_requested_at)}</div>
              ) : null}
            </div>
          ) : null}
          <EventTimeline
            events={replanEvents}
            tasks={tasks}
            jobs={jobs}
            defaultFilter="all"
            showFilters={false}
            emptyText="暂无重规划事件"
          />
        </section>
      ) : null}

      {projectJobs.length > 0 ? (
        <section className="panel">
          <h2>项目分析 / 直接完成 / 总结作业</h2>
          <JobRecordList
            jobs={projectJobs}
            revealJobId={selectedJobId}
            renderJob={(job) => (
              <JobDetailView
                key={job.id}
                job={job}
                retryable={job.id === retryableProjectJobId}
                readOnly={archived}
                focusNonce={job.id === selectedJobId ? 1 : 0}
                onRetry={() => run(() => api.retryJob(job.id))}
              />
            )}
          />
        </section>
      ) : null}

      {archived && archiveRefs && project.workspace_type !== "plain" ? (
        <details className="panel">
          <summary>归档历史与备份</summary>
          <p className="muted">
            任务与项目分支的提交保存在 refs/domo-archive。恢复前先确认同名分支不存在。
          </p>
          {archiveRefs.project ? (
            <>
              <div className="kv">
                <span>项目分支</span>
                <span className="mono">
                  {archiveRefs.project.reference} @{" "}
                  {archiveRefs.project.tip.slice(0, 7)}
                </span>
              </div>
              <div className="muted mono">{archiveRefs.project.recovery}</div>
              <div className="muted mono">{archiveRefs.project.log_command}</div>
            </>
          ) : null}
          {archiveRefs.tasks.length > 0 ? (
            <div className="derive-list">
              {archiveRefs.tasks.map((item) => (
                <div className="derive-child" key={item.task_id}>
                  <div className="derive-child-head">
                    <span className="task-key">{item.key}</span>
                    <span className="derive-child-title">{item.title}</span>
                    <Badge tone="muted">{item.tip.slice(0, 7)}</Badge>
                  </div>
                  <div className="muted mono">{item.reference}</div>
                  <div className="muted mono">{item.recovery}</div>
                  <div className="muted mono">{item.log_command}</div>
                </div>
              ))}
            </div>
          ) : (
            <div className="muted">没有任务分支归档记录。</div>
          )}
          <div className="kv">
            <span>备份命令</span>
            <span className="mono">{archiveRefs.backup_command}</span>
          </div>
        </details>
      ) : null}
    </div>
  );

  return (
    <div className="page">
      <div className="back-row">
        <button className="ghost" onClick={onBack}>
          ← 返回项目
        </button>
      </div>

      <ProjectHeader
        detail={detail}
        canReplan={canReplan}
        canEditGoal={!archived && project.stage !== "completed" && !!project.confirmed_at}
        busy={busy}
        merging={merging}
        mergeNotice={mergeNotice}
        onResume={() => void run(() => api.resumeProject(project.id))}
        onPause={() => void run(() => api.pauseProject(project.id))}
        onForcePause={() => {
          if (
            window.confirm(
              "强制暂停会立即终止所有在途作业，任务进入已暂停；之后点“恢复”继续推进。确认继续？",
            )
          ) {
            void run(() => api.pauseProject(project.id, true));
          }
        }}
        onToggleReplan={toggleReplan}
        onToggleGoalEdit={toggleGoalEdit}
        onArchive={() => void handleArchive()}
        onMerge={() => void handleMerge()}
      />

      {error ? <ErrorBox message={error} /> : null}

      {goalEditOpen ? (
        <GoalEditPanel
          goal={goalValue}
          goalDraftKey={goalDraftKey}
          busy={busy}
          onGoalChange={setGoalValue}
          onSubmit={() => void submitGoalEdit()}
          onCancel={cancelGoalEdit}
        />
      ) : null}

      {replanOpen && canReplan ? (
        <ReplanPanel
          preview={replanPreview}
          reason={replanReason}
          reasonDraftKey={replanDraftKey}
          busy={busy}
          onReasonChange={setReplanReason}
          onSubmit={() => void submitReplan()}
          onCancel={cancelReplan}
        />
      ) : null}

      <ProjectTabs
        order={blockOrder}
        active={activeBlock}
        badges={{
          analysis: projectTabBadge("analysis", badgeInput),
          tasks: projectTabBadge("tasks", badgeInput),
          report: projectTabBadge("report", badgeInput),
        }}
        onSelect={(block) => selectTab(block)}
      />

      {/* 以选项卡为 key：切换时面板淡入上移，面板本身始终占位，内容区高度不跳动。 */}
      <section
        key={activeBlock}
        className="project-panel"
        role="tabpanel"
        id={tabPanelId(activeBlock)}
        aria-labelledby={tabButtonId(activeBlock)}
        tabIndex={0}
      >
        {activeBlock === "overview" ? (
          <>
          <DeliveryPanel detail={detail} onSaved={() => void refresh()} />
          <ResourcePanel detail={detail} onSaved={() => void refresh()} />
          <ProjectOverview
            detail={detail}
            onOpenTab={openGuidanceTab}
            onRetryJob={(jobId) => run(() => api.retryJob(jobId))}
            onMerge={() => void handleMerge()}
            onResume={() => void run(() => api.resumeProject(project.id))}
            busy={busy}
            merging={merging}
          />
          </>
        ) : activeBlock === "analysis" ? (
          analysisBlock
        ) : activeBlock === "tasks" ? (
          <ProjectTasksSection
            project={project}
            tasks={tasks}
            activeTasks={activeTasks}
            visibleTasks={visibleTasks}
            directMode={directMode}
            directJob={directJob}
            archived={archived}
            busy={busy}
            proposal={proposal}
            taskView={taskView}
            onTaskViewChange={setTaskView}
            addTaskOpen={addTaskOpen}
            onToggleAddTask={toggleAddTaskForm}
            taskQuery={taskQuery}
            onTaskQueryChange={setTaskQuery}
            taskFilter={taskFilter}
            onTaskFilterChange={setTaskFilter}
            taskCounts={taskCounts}
            searchActive={searchActive}
            onResetTaskFilter={resetTaskFilter}
            draft={taskForm}
            draftCheck={draftCheck}
            depOptions={depOptions}
            onTaskField={setTaskField}
            onAddTask={handleAddTask}
            selectedTaskId={selectedTaskId}
            onSelectTask={selectTask}
            agentName={agentName}
            queueIds={queueIds}
            onReorder={(taskId, direction) =>
              void run(() => api.reorderTaskQueue(project.id, taskId, direction))
            }
          />
        ) : activeBlock === "report" ? (
          <ProjectReportPanel
            project={project}
            reviewMessages={reviewMessages}
            feedback={feedback}
            feedbackDraftKey={feedbackDraftKey}
            onFeedbackChange={setFeedback}
            busy={busy}
            archived={archived}
            onSubmitReport={(action) => void submitReport(action)}
            onRequestReplan={requestReportReplan}
            onAddTask={openAddTaskForm}
          />
        ) : (
          infoBlock
        )}
      </section>

      <ProjectTopBlocks detail={detail} onRefresh={() => refresh()} />

      {selectedTask ? (
        <TaskDrawer
          key={selectedTask.id}
          task={selectedTask}
          detail={detail}
          onClose={() => selectTask(null)}
          onRetry={() =>
            run(async () => {
              await api.retryTask(project.id, selectedTask.id);
            })
          }
          onRetryJob={(jobId) => run(() => api.retryJob(jobId))}
          onSaveGuidance={(guidance) =>
            run(async () => {
              await api.saveTaskGuidance(project.id, selectedTask.id, guidance);
            })
          }
          onToggleMilestone={() =>
            run(async () => {
              await api.setTaskMilestone(
                project.id,
                selectedTask.id,
                !selectedTask.milestone,
              );
            })
          }
          onRefresh={() => refresh()}
          archived={archived}
          busy={busy}
          agentName={agentName}
          snapshotVersion={snapshotVersion}
        />
      ) : null}
    </div>
  );
}
