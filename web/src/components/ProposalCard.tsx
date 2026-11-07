import { useId, useLayoutEffect, useRef, useState } from "react";
import type { AnalysisProposal, Task } from "../types";
import { RichContent } from "./RichContent";
import {
  confirmModeRow,
  confirmImpact,
  proposalSections,
  sectionSummary,
  type ProposalSection,
  type ProposalSectionKey,
} from "../presentation/proposalView";

/// 自动确认策略行：状态文字、说明与复选框；开启时整行带提示色。
export function AutoConfirmRow({
  mode,
  busy,
  disabled,
  note,
  onChange,
}: {
  mode: "manual" | "auto";
  busy: boolean;
  disabled: boolean;
  note?: string | null;
  onChange: (next: "manual" | "auto") => void;
}) {
  const label = confirmModeRow(mode);
  const descriptionId = useId();
  return (
    <label
      className={`policy-row${label.enabled ? " policy-row-on" : ""}`}
      data-confirm-mode={mode}
    >
      <input
        type="checkbox"
        className="policy-checkbox"
        checked={label.enabled}
        aria-label="自动确认方案"
        aria-describedby={descriptionId}
        disabled={busy || disabled}
        onChange={() => onChange(label.enabled ? "manual" : "auto")}
      />
      <span className="policy-row-text">
        <strong className="policy-row-title">{label.title}</strong>
        <span className="policy-row-desc" id={descriptionId}>
          {label.description}
        </span>
        {note ? <span className="policy-row-note muted">{note}</span> : null}
      </span>
    </label>
  );
}

/// 方案卡片：六节可折叠小节 + 底部确认与影响摘要。
export function ProposalCard({
  proposal,
  activeTasks,
  mode,
  busy,
  disabled,
  confirmDisabledReason,
  confirming,
  onConfirm,
}: {
  proposal: AnalysisProposal;
  activeTasks: Pick<Task, "key">[];
  mode: "manual" | "auto";
  busy: boolean;
  disabled: boolean;
  confirmDisabledReason: string | null;
  confirming: boolean;
  onConfirm: () => void;
}) {
  const sections = proposalSections(proposal);
  const impact = confirmImpact(proposal, activeTasks, mode);
  const signature = proposal.tasks.map((task) => task.key).join(",");
  const [seenSignature, setSeenSignature] = useState(signature);
  const listEndRef = useRef<HTMLDivElement | null>(null);
  // 首次渲染不滚动；任务方案出现新条目时自动跟到最新一条。
  useLayoutEffect(() => {
    if (signature === seenSignature) return;
    setSeenSignature(signature);
    const end = listEndRef.current;
    end?.scrollIntoView?.({ block: "nearest" });
  }, [signature, seenSignature]);

  function sectionBody(key: ProposalSectionKey) {
    switch (key) {
      case "goal":
        return proposal.goal ? (
          <RichContent content={proposal.goal} />
        ) : (
          <p className="muted">方案未给出目标改写，沿用你提交的项目目标。</p>
        );
      case "assumptions":
        return (
          <ul>
            {proposal.assumptions.map((item) => (
              <li key={item}>{item}</li>
            ))}
          </ul>
        );
      case "acceptance":
        return (
          <ul>
            {proposal.acceptance.map((item) => (
              <li key={item}>{item}</li>
            ))}
          </ul>
        );
      case "tasks":
        return (
          <ol className="proposal-task-list">
            {proposal.tasks.map((task) => (
              <li key={task.key}>
                <div className="proposal-task-head">
                  <span className="task-key">{task.key}</span>
                  <strong>{task.title}</strong>
                  {task.deps.length ? (
                    <span className="muted">依赖 {task.deps.join("、")}</span>
                  ) : (
                    <span className="muted">无依赖</span>
                  )}
                </div>
                <RichContent content={task.description} />
                {task.acceptance.length ? (
                  <ul>
                    {task.acceptance.map((item) => (
                      <li key={item}>验收：{item}</li>
                    ))}
                  </ul>
                ) : null}
              </li>
            ))}
            <div ref={listEndRef} />
          </ol>
        );
    }
  }

  function renderSection(section: ProposalSection) {
    const body = sectionBody(section.key);
    const empty =
      section.key === "tasks" ? proposal.tasks.length === 0 : null;
    return (
      <details
        key={section.key}
        className="proposal-section"
        data-section={section.key}
        open={section.expanded}
      >
        <summary>
          <span className="proposal-section-label">{section.label}</span>
          {empty === false ? (
            <span className="proposal-section-count muted">
              {sectionSummary(section)}
            </span>
          ) : null}
        </summary>
        <div className="proposal-section-body">
          {empty ? <p className="muted">方案没有拆分任务。</p> : body}
        </div>
      </details>
    );
  }

  const isReplan = Boolean(proposal.replan_impact);
  const confirmLabel = isReplan
    ? "确认草稿，应用重规划"
    : "确认方案，开始执行";

  return (
    <article className="proposal-card" data-proposal-kind={proposal.kind}>
      <header className="proposal-card-head">
        <span className="proposal-kind">
          {isReplan
            ? proposal.replan_impact?.high_impact
              ? "高影响重规划草稿"
              : "低影响重规划草稿"
            : "项目方案"}
        </span>
        <span className="proposal-kind-scope muted">
          {proposal.tasks.length} 个任务
        </span>
      </header>
      <div className="proposal-card-summary">
        <RichContent content={proposal.summary} />
      </div>
      <div className="proposal-sections">
        {sections.map(renderSection)}
      </div>
      <footer className="proposal-card-footer">
        <div className="proposal-impact">
          <strong>{isReplan ? "应用后" : "确认后"}</strong>
          <ul>
            {impact.items.map((item) => (
              <li key={item}>{item}</li>
            ))}
          </ul>
          <span className="muted">{impact.note}</span>
        </div>
        <div className="proposal-actions">
          <button
            type="button"
            className="primary"
            disabled={busy || disabled || confirmDisabledReason !== null}
            title={confirmDisabledReason ?? undefined}
            onClick={onConfirm}
          >
            {confirming
              ? isReplan
                ? "应用中…"
                : "确认中…"
              : confirmLabel}
          </button>
          {confirmDisabledReason ? (
            <span className="muted">{confirmDisabledReason}</span>
          ) : null}
        </div>
      </footer>
    </article>
  );
}
