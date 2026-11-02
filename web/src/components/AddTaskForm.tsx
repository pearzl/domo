import type { FormEvent } from "react";
import { InputBox } from "./InputBox";
import { TaskFormCard } from "./TaskBoard";
import type { AddTaskDraft } from "../presentation/addTaskDraft";
import type {
  DependencyOption,
  TaskDraftCheck,
} from "../presentation/taskList";

/// 添加任务表单：草稿字段由页面持久化，校验结果也由页面派生后传入。
export function AddTaskForm({
  draft,
  draftCheck,
  depOptions,
  busy,
  onField,
  onSubmit,
}: {
  draft: AddTaskDraft;
  draftCheck: TaskDraftCheck;
  depOptions: DependencyOption[];
  busy: boolean;
  onField: <K extends keyof AddTaskDraft>(
    field: K,
    value: AddTaskDraft[K],
  ) => void;
  onSubmit: (event: FormEvent) => void;
}) {
  return (
    <TaskFormCard>
      <form className="form add-task-form" onSubmit={onSubmit}>
        <div className="form-grid">
          <label>
            <span>任务 key</span>
            <input
              value={draft.key}
              onChange={(event) => onField("key", event.target.value)}
              placeholder="例如：T3"
              aria-invalid={draftCheck.errors.key ? true : undefined}
              required
            />
            {draftCheck.errors.key ? (
              <small className="field-error">{draftCheck.errors.key}</small>
            ) : null}
          </label>
          <label>
            <span>标题</span>
            <input
              value={draft.title}
              onChange={(event) => onField("title", event.target.value)}
              placeholder="一句话说明这个任务做什么"
              required
            />
          </label>
        </div>
        <div className="field">
          <span className="field-label">描述</span>
          <InputBox
            value={draft.description}
            onChange={(next) => onField("description", next)}
            rows={3}
            placeholder="任务目标、边界和实现要点。"
          />
        </div>
        <div className="field">
          <span className="field-label">验收标准（每行一条，留空使用默认标准）</span>
          <InputBox
            value={draft.acceptance}
            onChange={(next) => onField("acceptance", next)}
            rows={3}
            placeholder={"例如：\ndomo_mock/T3.txt 存在且包含 T3"}
          />
        </div>
        <div className="field">
          <span className="field-label">前置任务（依赖）</span>
          <small className="muted">
            只引用当前活动任务；已完成、执行中的任务都能作为依赖，已取消任务不在此列出。
          </small>
          {depOptions.length === 0 ? (
            <span className="muted">项目还没有可依赖的任务。</span>
          ) : (
            <div className="checkbox-row">
              {depOptions.map((option) => (
                <label className="checkbox-label" key={option.key}>
                  <input
                    type="checkbox"
                    checked={draft.deps.includes(option.key)}
                    onChange={(event) => {
                      onField(
                        "deps",
                        event.target.checked
                          ? [...draft.deps, option.key]
                          : draft.deps.filter((dep) => dep !== option.key),
                      );
                    }}
                  />
                  <span>
                    {option.key} {option.title}
                    <span className="dep-stage muted"> · {option.stageLabel}</span>
                  </span>
                </label>
              ))}
            </div>
          )}
          {draftCheck.errors.deps ? (
            <small className="field-error">{draftCheck.errors.deps}</small>
          ) : null}
        </div>
        <label className="checkbox-label">
          <input
            type="checkbox"
            checked={draft.milestone}
            onChange={(event) => onField("milestone", event.target.checked)}
          />
          <span>标记为里程碑（验收通过后需人工确认）</span>
        </label>
        <div className="form-actions">
          <button
            className="primary"
            type="submit"
            disabled={busy || !draftCheck.valid}
            title={draftCheck.valid ? undefined : "请先修正表单中的提示"}
          >
            {busy ? "提交中…" : "添加任务"}
          </button>
          <span className="muted">
            项目在总结或完成阶段时会回到执行阶段，原报告作废待重写。
          </span>
        </div>
      </form>
    </TaskFormCard>
  );
}
