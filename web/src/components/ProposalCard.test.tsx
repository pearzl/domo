import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { AnalysisProposal } from "../types";
import { AutoConfirmRow, ProposalCard } from "./ProposalCard";
import {
  PROPOSAL_LONG_CHARS,
  PROPOSAL_SECTION_LABELS,
  autoConfirmLocked,
  confirmImpact,
  confirmModeRow,
  confirmModeRuntimeNote,
  proposalSections,
  proposalTaskSectionLabel,
} from "../presentation/proposalView";

function proposal(overrides: Partial<AnalysisProposal> = {}): AnalysisProposal {
  return {
    kind: "plan",
    summary: "分析结论：现状与取舍。",
    goal: "完成项目页重构。",
    assumptions: ["后端接口保持不变"],
    acceptance: ["npm run build 通过"],
    tasks: [
      {
        key: "T1",
        title: "改造分析区块",
        description: "把确认策略改为开关行",
        acceptance: ["渲染断言通过"],
        deps: [],
      },
      {
        key: "T2",
        title: "组织方案小节",
        description: "六节可折叠",
        acceptance: ["小节标题齐全"],
        deps: ["T1"],
      },
    ],
    plan: ["先改展示", "再补测试"],
    verification: ["npm run test"],
    skip_confirmation: false,
    ...overrides,
  };
}

function renderRow(mode: "manual" | "auto", note?: string) {
  return renderToStaticMarkup(
    <AutoConfirmRow
      mode={mode}
      busy={false}
      disabled={false}
      note={note}
      onChange={() => {}}
    />,
  );
}

describe("自动确认开关行", () => {
  it("auto 时渲染“自动确认已开启”的开关元素", () => {
    const markup = renderRow("auto");
    expect(markup).toContain("自动确认已开启");
    expect(markup).not.toContain("自动确认已关闭");
    expect(markup).toContain('role="switch"');
    expect(markup).toContain('aria-checked="true"');
    expect(markup).toContain("policy-row-on");
    expect(markup).toContain('data-confirm-mode="auto"');
    // 开关元素本身就是可操作控件，不再使用复选框。
    expect(markup).toContain("<button");
    expect(markup).not.toContain('type="checkbox"');
  });

  it("manual 时文案相反且不带提示色", () => {
    const markup = renderRow("manual");
    expect(markup).toContain("自动确认已关闭");
    expect(markup).not.toContain("自动确认已开启");
    expect(markup).toContain('aria-checked="false"');
    expect(markup).not.toContain("policy-row-on");
  });

  it("状态文字与说明成对给出", () => {
    expect(confirmModeRow("auto")).toMatchObject({
      enabled: true,
      title: "自动确认已开启",
    });
    expect(confirmModeRow("manual")).toMatchObject({
      enabled: false,
      title: "自动确认已关闭",
    });
    expect(confirmModeRow("auto").description).not.toBe("");
    expect(confirmModeRow("manual").description).not.toBe("");
  });

  it("运行中切换给出下一轮生效说明且开关保持可操作", () => {
    const markup = renderRow("auto", confirmModeRuntimeNote(true) ?? undefined);
    expect(markup).toContain("将在下一轮分析生效");
    expect(markup).toContain("policy-row-note");
    expect(markup).not.toContain("disabled");
  });

  it("只有归档与暂停会锁住开关", () => {
    expect(autoConfirmLocked("archived")).toBe(true);
    expect(autoConfirmLocked("paused")).toBe(true);
    expect(autoConfirmLocked("pausing")).toBe(true);
    expect(autoConfirmLocked("running")).toBe(false);
    expect(autoConfirmLocked("blocked")).toBe(false);
    expect(autoConfirmLocked(null)).toBe(false);
    expect(confirmModeRuntimeNote(false)).toBeNull();
  });
});

describe("方案卡片小节", () => {
  it("六个小节标题齐全且任务方案带数量", () => {
    const markup = renderToStaticMarkup(
      <ProposalCard
        proposal={proposal()}
        activeTasks={[]}
        mode="manual"
        busy={false}
        confirming={false}
        disabled={false}
        confirmDisabledReason={null}
        onConfirm={() => {}}
      />,
    );
    for (const label of Object.values(PROPOSAL_SECTION_LABELS)) {
      expect(markup).toContain(label);
    }
    expect(markup).toContain(proposalTaskSectionLabel(2));
    expect(markup).toContain("任务方案（2 个任务）");
    expect((markup.match(/<details/g) ?? []).length).toBe(6);
    expect((markup.match(/<summary>/g) ?? []).length).toBe(6);
  });

  it("目标、假设、验收口径、执行步骤与自检方式按固定顺序出现", () => {
    const markup = renderToStaticMarkup(
      <ProposalCard
        proposal={proposal()}
        activeTasks={[]}
        mode="auto"
        busy={false}
        confirming={false}
        disabled={false}
        confirmDisabledReason={null}
        onConfirm={() => {}}
      />,
    );
    const order = [
      ">目标<",
      ">假设<",
      ">验收口径<",
      ">任务方案（2 个任务）<",
      ">执行步骤<",
      ">自检方式<",
    ].map((label) => markup.indexOf(label));
    expect(order.every((index) => index > 0)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
  });

  it("确认按钮与确认后影响摘要位于同一卡片底部", () => {
    const markup = renderToStaticMarkup(
      <ProposalCard
        proposal={proposal()}
        activeTasks={[]}
        mode="manual"
        busy={false}
        confirming={false}
        disabled={false}
        confirmDisabledReason={null}
        onConfirm={() => {}}
      />,
    );
    const footer = markup.slice(markup.indexOf("proposal-card-footer"));
    expect(footer).toContain("确认后");
    expect(footer).toContain("按方案创建 2 个任务（T1、T2）");
    expect(footer).toContain("确认方案，开始执行");
    expect(footer).toContain("<button");
    // 按钮在影响摘要之后，并处在卡片内。
    expect(markup.indexOf("确认方案，开始执行")).toBeGreaterThan(
      markup.indexOf("proposal-card-footer"),
    );
  });

  it("长方案默认折叠超出部分", () => {
    const tasks = Array.from({ length: 12 }, (_, index) => ({
      key: `T${index + 1}`,
      title: `任务 ${index + 1}`,
      description: "x".repeat(PROPOSAL_LONG_CHARS),
      acceptance: ["ok"],
      deps: [],
    }));
    const long = proposal({ tasks });
    const sections = proposalSections(long);
    expect(sections.filter((section) => section.expanded).map((s) => s.key)).toEqual([
      "tasks",
    ]);
    expect(sections.every((section) => section.count >= 0)).toBe(true);
    expect(sections.find((s) => s.key === "tasks")?.label).toBe(
      "任务方案（12 个任务）",
    );
    const markup = renderToStaticMarkup(
      <ProposalCard
        proposal={long}
        activeTasks={[]}
        mode="manual"
        busy={false}
        confirming={false}
        disabled={false}
        confirmDisabledReason={null}
        onConfirm={() => {}}
      />,
    );
    expect((markup.match(/ open=""/g) ?? []).length).toBe(1);
    // 长方案的小节标题显示条数，便于在折叠状态下判断内容量。
    expect(markup).toContain('<span class="proposal-section-count muted">12 条</span>');
  });

  it("确认禁用原因显示在卡片底部", () => {
    const markup = renderToStaticMarkup(
      <ProposalCard
        proposal={proposal()}
        activeTasks={[]}
        mode="auto"
        busy={false}
        confirming={false}
        disabled={false}
        confirmDisabledReason="输入框里有未发送的修改意见，发送或清空后再确认。"
        onConfirm={() => {}}
      />,
    );
    expect(markup).toContain("输入框里有未发送的修改意见");
    expect(markup).toMatch(/disabled=""/);
  });
});

describe("确认后影响摘要", () => {
  it("任务方案说明任务数量与依赖", () => {
    const impact = confirmImpact(proposal(), [], "manual");
    expect(impact.items[0]).toBe("按方案创建 2 个任务（T1、T2）");
    expect(impact.items[1]).toContain("依赖");
    expect(impact.note).toContain("必须由上方的确认按钮开始执行");
  });

  it("已有任务时说明作废数量，自动确认提示不代替确认", () => {
    const impact = confirmImpact(proposal(), [{ key: "T9" }], "auto");
    expect(impact.items[0]).toContain("现有 1 个任务会被新的任务方案作废");
    expect(impact.note).toContain("自动确认");
  });

  it("直接完成模式给出报告验收预期", () => {
    const impact = confirmImpact(
      proposal({ kind: "direct", tasks: [] }),
      [],
      "manual",
    );
    expect(impact.items.join(" ")).toContain("报告");
  });
});
