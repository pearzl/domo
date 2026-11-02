/// 项目详情页的状态与动作：轮询快照、URL 视图同步、草稿清理与各类提交。
/// 组件只读取返回值并渲染；所有请求与本地状态都留在本 hook 内。
import { useEffect, useMemo, useRef, useState } from "react";
import type { FormEvent } from "react";
import type { ProjectArchiveRefs, ProjectDetail } from "../types";
import { api, errorMessage, subscribeEvents } from "../api";
import { useDraftJson, useDraftValue } from "../components/useDraft";
import { verificationSignature } from "./taskSummary";
import {
  filterTasks,
  nextTaskKey,
  validateTaskDraft,
  type TaskFilter,
} from "./taskList";
import { resolveProjectTab } from "./projectLayout";
import {
  readProjectView,
  updateProjectView,
  type ProjectViewState,
} from "./projectView";
import {
  EMPTY_TASK_DRAFT,
  isAddTaskDraft,
  type AddTaskDraft,
} from "./addTaskDraft";

function readWindowView(): ProjectViewState {
  if (typeof window === "undefined") return { tab: null, taskId: null };
  return readProjectView(window.location.search);
}

export function useProjectPageController({ id }: { id: string }) {
  const [detail, setDetail] = useState<ProjectDetail | null>(null);
  const [snapshotVersion, setSnapshotVersion] = useState(0);
  const verificationSignatureRef = useRef("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const analysisDraft = useDraftValue(
    { projectId: id, field: "analysis-message" },
    "",
  );
  const message = analysisDraft.value;
  const setMessage = analysisDraft.setValue;
  const feedbackDraft = useDraftValue(
    { projectId: id, field: "report-feedback" },
    "",
  );
  const feedback = feedbackDraft.value;
  const setFeedback = feedbackDraft.setValue;
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const initialView = readWindowView();
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(
    initialView.taskId,
  );
  const [activeTab, setActiveTab] = useState<string | null>(initialView.tab);
  const [replanOpen, setReplanOpen] = useState(false);
  const replanDraft = useDraftValue(
    { projectId: id, field: "replan-reason" },
    "",
  );
  const replanReason = replanDraft.value;
  const setReplanReason = replanDraft.setValue;
  const [addTaskOpen, setAddTaskOpen] = useState(false);
  const taskDraft = useDraftJson(
    { projectId: id, field: "add-task-form" },
    EMPTY_TASK_DRAFT,
    isAddTaskDraft,
  );
  const taskForm = taskDraft.value;
  /// 任务面板视图只在页面内保持，不写入任务数据。
  const [taskView, setTaskView] = useState<"list" | "dag">("list");
  /// 任务搜索与筛选同样只存在于页面状态，不改变任务数据。
  const [taskQuery, setTaskQuery] = useState("");
  const [taskFilter, setTaskFilter] = useState<TaskFilter>("all");
  const [merging, setMerging] = useState(false);
  const [mergeNotice, setMergeNotice] = useState<{
    tone: "ok" | "error";
    text: string;
  } | null>(null);
  const [archiveRefs, setArchiveRefs] = useState<ProjectArchiveRefs | null>(
    null,
  );

  function setTaskField<K extends keyof AddTaskDraft>(
    field: K,
    value: AddTaskDraft[K],
  ) {
    taskDraft.setValue((current) => ({ ...current, [field]: value }));
  }

  /// 视图状态写入 URL：用户切换选项卡或打开抽屉时新增历史，程序修正时替换当前历史。
  function applyProjectView(
    patch: Partial<ProjectViewState>,
    mode: "push" | "replace" = "push",
  ) {
    if (typeof window === "undefined") return;
    const nextSearch = updateProjectView(window.location.search, patch);
    const next = `${window.location.pathname}${nextSearch}`;
    if (`${window.location.pathname}${window.location.search}` === next) return;
    if (mode === "replace") {
      window.history.replaceState(window.history.state, "", next);
    } else {
      window.history.pushState({}, "", next);
    }
  }

  function selectTask(taskId: string | null) {
    setSelectedTaskId(taskId);
    applyProjectView({ taskId }, taskId ? "push" : "replace");
  }

  function selectTab(tab: string) {
    setActiveTab(tab);
    setSelectedTaskId(null);
    applyProjectView({ tab, taskId: null }, "push");
  }

  // 浏览器前进/后退时同步选项卡与抽屉；路径变化由 App 负责。
  useEffect(() => {
    function handlePop() {
      const view = readWindowView();
      setActiveTab(view.tab);
      setSelectedTaskId(view.taskId);
    }
    window.addEventListener("popstate", handlePop);
    return () => window.removeEventListener("popstate", handlePop);
  }, []);

  async function refresh(silent = false) {
    try {
      const next = await api.getProject(id);
      setDetail(next);
      // 验收相关数据变化时递增版本号，抽屉按版本号重新读取验收记录；
      // 仅轮询心跳而没有新数据时不重复请求。
      const signature = verificationSignature(next.tasks, next.jobs);
      if (signature !== verificationSignatureRef.current) {
        verificationSignatureRef.current = signature;
        setSnapshotVersion((current) => current + 1);
      }
      setError("");
    } catch (err) {
      if (!silent) setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    setLoading(true);
    void refresh();
    const timer = window.setInterval(() => void refresh(true), 2500);
    const unsubscribe = subscribeEvents(() => void refresh(true));
    return () => {
      window.clearInterval(timer);
      unsubscribe();
    };
  }, [id]);

  const visibleTasks = useMemo(
    () => filterTasks(detail?.tasks ?? [], taskQuery, taskFilter),
    [detail, taskQuery, taskFilter],
  );
  // 选中的任务被搜索或筛选隐藏时关闭抽屉，避免抽屉与列表状态不一致。
  useEffect(() => {
    if (
      selectedTaskId !== null &&
      detail !== null &&
      !visibleTasks.some((task) => task.id === selectedTaskId)
    ) {
      setSelectedTaskId(null);
      applyProjectView({ taskId: null }, "replace");
    }
  }, [detail, selectedTaskId, visibleTasks]);

  // 选项卡必须属于当前阶段；URL 中缺失或失效时回退到阶段默认值并写回地址栏。
  useEffect(() => {
    if (!detail) return;
    const next = resolveProjectTab(detail.project.stage, activeTab);
    if (next !== activeTab) setActiveTab(next);
    applyProjectView({ tab: next, taskId: selectedTaskId }, "replace");
  }, [detail, activeTab, selectedTaskId]);

  useEffect(() => {
    if (detail?.project.status !== "archived") {
      setArchiveRefs(null);
      return;
    }
    let active = true;
    void api
      .getProjectArchiveRefs(id)
      .then((next) => {
        if (active) setArchiveRefs(next);
      })
      .catch(() => {
        if (active) setArchiveRefs(null);
      });
    return () => {
      active = false;
    };
  }, [id, detail?.project.status]);

  async function run(action: () => Promise<unknown>): Promise<boolean> {
    setBusy(true);
    setError("");
    try {
      await action();
      await refresh();
      return true;
    } catch (err) {
      setError(errorMessage(err));
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function handleArchive() {
    setBusy(true);
    setError("");
    try {
      const preview = await api.getArchivePreview(id);
      const dirty = preview.dirty;
      let prompt = "归档后不会继续推进，也不能恢复。确认归档？";
      if (dirty.length > 0) {
        const shown = dirty
          .slice(0, 5)
          .map((item) => `${item.label}（${item.path}）`)
          .join("\n");
        const more =
          dirty.length > 5 ? `\n…等 ${dirty.length} 个工作区` : "";
        prompt = `以下工作区有未提交改动：\n${shown}${more}\n\n归档后会保留这些工作区，项目工作区可能无法完全清理。确认归档？`;
      }
      if (!window.confirm(prompt)) return;
      await api.archiveProject(id, dirty.length > 0);
      await refresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  // 打开添加任务表单：切到任务选项卡，key 为空时自动填入下一个可用的 T 编号。
  function openAddTaskForm() {
    selectTab("tasks");
    setAddTaskOpen(true);
    taskDraft.setValue((current) => ({
      ...current,
      key: current.key.trim() || nextTaskKey(detail?.tasks ?? []),
    }));
  }

  function toggleAddTaskForm() {
    if (addTaskOpen) {
      setAddTaskOpen(false);
      return;
    }
    openAddTaskForm();
  }

  async function handleMerge() {
    if (!detail) return;
    const project = detail.project;
    if (
      !window.confirm(
        `将项目分支 ${project.branch} rebase 到 ${project.base_branch}，并把 ${project.base_branch} 快进到项目分支提交。确认合入？`,
      )
    ) {
      return;
    }
    setMerging(true);
    setMergeNotice(null);
    try {
      const result = await api.mergeProject(project.id);
      setMergeNotice({ tone: "ok", text: result.message });
      await refresh();
    } catch (err) {
      setMergeNotice({ tone: "error", text: errorMessage(err) });
    } finally {
      setMerging(false);
    }
  }

  async function handleAddTask(event: FormEvent) {
    event.preventDefault();
    if (!detail) return;
    const activeTasks = detail.tasks.filter((task) => task.status !== "canceled");
    const draftCheck = validateTaskDraft(
      {
        key: taskForm.key,
        title: taskForm.title,
        description: taskForm.description,
        deps: taskForm.deps,
      },
      activeTasks,
    );
    if (!draftCheck.valid) return;
    const key = taskForm.key.trim();
    const ok = await run(() =>
      api.createTask(detail.project.id, {
        key,
        title: taskForm.title.trim(),
        description: taskForm.description.trim(),
        acceptance: taskForm.acceptance
          .split("\n")
          .map((item) => item.trim())
          .filter(Boolean),
        deps: taskForm.deps,
        milestone: taskForm.milestone,
      }),
    );
    if (!ok) return;
    taskDraft.clear(EMPTY_TASK_DRAFT);
    setAddTaskOpen(false);
  }

  function resetTaskFilter() {
    setTaskQuery("");
    setTaskFilter("all");
  }

  async function sendAnalysis() {
    if (!detail) return;
    const ok = await run(() =>
      api.postAnalysisMessage(detail.project.id, message, detail.project.confirm_mode),
    );
    if (ok) analysisDraft.clear();
  }

  async function confirmProposal() {
    if (!detail) return;
    setConfirming(true);
    const ok = await run(() => api.confirmAnalysis(detail.project.id));
    setConfirming(false);
    if (ok) analysisDraft.clear();
  }

  async function submitReport(
    action: "accept" | "comment" | "reject" | "replan",
  ) {
    const ok = await run(() => api.submitReport(id, action, feedback));
    if (ok) feedbackDraft.clear();
  }

  async function submitReplan() {
    const ok = await run(() => api.replanProject(id, replanReason));
    if (!ok) return;
    replanDraft.clear();
    setReplanOpen(false);
  }

  function toggleReplan() {
    setReplanOpen((value) => !value);
  }

  function cancelReplan() {
    setReplanOpen(false);
    replanDraft.clear();
  }

  return {
    detail,
    snapshotVersion,
    error,
    loading,
    busy,
    confirming,
    message,
    setMessage,
    analysisDraftKey: analysisDraft.key,
    feedback,
    setFeedback,
    feedbackDraftKey: feedbackDraft.key,
    selectedTaskId,
    activeTab,
    replanOpen,
    replanReason,
    setReplanReason,
    replanDraftKey: replanDraft.key,
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
    submitReplan,
    toggleReplan,
    cancelReplan,
  };
}

export type ProjectPageController = ReturnType<typeof useProjectPageController>;
