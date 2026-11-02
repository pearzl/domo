import { describe, expect, it } from "vitest";
import {
  DRAWER_ACCEPTANCE_TITLE,
  DRAWER_CONTENT_SECTION_CLASS,
  DRAWER_DESCRIPTION_TITLE,
  DRAWER_FEEDBACK_TITLE,
  DRAWER_LEAD_TITLES,
  DEPS_FOLDED_FIELDS,
  DEPS_PRIMARY_FIELDS,
  acceptanceMarkdown,
  canControlTask,
  depsFoldedFields,
  drawerLayout,
  drawerLead,
  hasVerifierFeedback,
  isTaskTerminal,
  rawErrorEntry,
  verifierFeedback,
} from "./taskDrawer";

const base = { stage: "running", status: null, human_gate: null };

const cases = [
  { name: "blocked", input: { ...base, status: "blocked" } },
  { name: "milestone", input: { ...base, human_gate: "milestone" } },
  { name: "completed", input: { ...base, stage: "completed" } },
  { name: "canceled", input: { ...base, status: "canceled" } },
  { name: "active", input: base },
];

describe("drawerLead 首区块选择", () => {
  it("blocked 首区块为 gate", () => {
    expect(drawerLead({ ...base, status: "blocked" })).toBe("gate");
    expect(drawerLayout({ ...base, status: "blocked" }).lead).toBe("gate");
  });

  it("human_gate=milestone 首区块为 gate", () => {
    expect(drawerLead({ ...base, human_gate: "milestone" })).toBe("gate");
    expect(
      drawerLayout({ stage: "completed", status: null, human_gate: "milestone" }).lead,
    ).toBe("gate");
  });

  it("completed 首区块为 result", () => {
    expect(drawerLead({ ...base, stage: "completed" })).toBe("result");
  });

  it("canceled 首区块为 result", () => {
    expect(drawerLead({ ...base, status: "canceled" })).toBe("result");
    expect(drawerLead({ ...base, stage: "completed", status: "canceled" })).toBe("result");
  });

  it("active 首区块为 control", () => {
    expect(drawerLead(base)).toBe("control");
    expect(drawerLead({ ...base, status: "paused" })).toBe("control");
    expect(drawerLead({ ...base, stage: "pending" })).toBe("control");
  });
});

describe("isTaskTerminal 终态判断", () => {
  it("只有 completed 与 canceled 是终态", () => {
    expect(isTaskTerminal({ ...base, stage: "completed" })).toBe(true);
    expect(isTaskTerminal({ ...base, status: "canceled" })).toBe(true);
    expect(isTaskTerminal({ ...base, stage: "completed", human_gate: "milestone" })).toBe(true);
    expect(isTaskTerminal(base)).toBe(false);
    expect(isTaskTerminal({ ...base, stage: "verifying" })).toBe(false);
    expect(isTaskTerminal({ ...base, status: "blocked" })).toBe(false);
    expect(isTaskTerminal({ ...base, stage: "pending" })).toBe(false);
  });
});

describe("canControlTask", () => {
  it("只有可操作的任务显示控制条", () => {
    expect(canControlTask(base)).toBe(true);
    expect(canControlTask({ ...base, stage: "completed" })).toBe(false);
    expect(canControlTask({ ...base, status: "canceled" })).toBe(false);
    expect(canControlTask({ ...base, archived: true })).toBe(false);
  });
});

describe("DRAWER_LEAD_TITLES", () => {
  it("覆盖三类首区块", () => {
    expect(DRAWER_LEAD_TITLES).toEqual({ gate: "裁决", result: "结果", control: "控制" });
  });
});

describe("描述与验收标准的同级区块", () => {
  it("两者共用一个容器类名，标题各自独立", () => {
    expect(DRAWER_CONTENT_SECTION_CLASS).toBe("drawer-content-section");
    expect(DRAWER_DESCRIPTION_TITLE).toBe("任务描述");
    expect(DRAWER_ACCEPTANCE_TITLE).toBe("验收标准");
    expect(DRAWER_FEEDBACK_TITLE).toBe("验收反馈");
  });

  it("验收标准按 Markdown 列表排版，空白项被丢弃", () => {
    expect(acceptanceMarkdown(["验收项一", "  ", "验收项二 "])).toBe(
      "- 验收项一\n- 验收项二",
    );
    expect(acceptanceMarkdown([])).toBe("");
  });
});

describe("验收反馈", () => {
  it("空白反馈不算反馈", () => {
    expect(hasVerifierFeedback({ ...base })).toBe(false);
    expect(hasVerifierFeedback({ ...base, verifier_issues: [] })).toBe(false);
    expect(hasVerifierFeedback({ ...base, verifier_issues: ["", "  "] })).toBe(false);
    expect(hasVerifierFeedback({ ...base, verifier_issues: ["需要补充边界"] })).toBe(true);
    expect(verifierFeedback(["需要补充边界", " ", "缺少测试"])).toEqual([
      "需要补充边界",
      "缺少测试",
    ]);
    expect(verifierFeedback(undefined)).toEqual([]);
  });
});

describe("依赖与位置分层", () => {
  it("关键字段常显，其余字段在折叠区且不重复", () => {
    expect(DEPS_PRIMARY_FIELDS).toEqual(["deps", "queue", "waitingReason"]);
    const folded = depsFoldedFields({ archiveReference: false, archiveRecovery: false });
    expect(folded).toEqual(DEPS_FOLDED_FIELDS);
    expect(folded).toEqual(
      expect.arrayContaining(["branch", "worktree", "failureKind", "agent"]),
    );
    for (const field of folded) {
      expect(DEPS_PRIMARY_FIELDS).not.toContain(field);
    }
  });

  it("归档 ref 与恢复命令只在可用时进入折叠区", () => {
    const folded = depsFoldedFields({ archiveReference: true, archiveRecovery: true });
    expect(folded).toEqual([...DEPS_FOLDED_FIELDS, "archiveRef", "archiveRecovery"]);
  });
});

describe("drawerLayout 区块顺序", () => {
  it("agentPool 始终是 tail 的最后一项", () => {
    for (const item of cases) {
      const layout = drawerLayout(item.input);
      expect(layout.tail[layout.tail.length - 1]).toBe("agentPool");
    }
  });

  it("同一布局不产生重复区块，也不出现第二个 gate 或 failure 区块", () => {
    for (const item of cases) {
      const layout = drawerLayout({ ...item.input, last_error: "boom" });
      expect(new Set(layout.tail).size).toBe(layout.tail.length);
      expect(layout.tail).not.toContain(layout.lead as never);
      const sections = layout.tail as string[];
      expect(sections.filter((section) => section === "gate")).toHaveLength(0);
      expect(sections.filter((section) => section === "failure")).toHaveLength(0);
    }
  });

  it("gate 首区块承载失败原因，抽屉不再追加原始错误入口", () => {
    for (const input of [
      { ...base, status: "blocked" },
      { ...base, human_gate: "milestone" },
    ]) {
      const layout = drawerLayout({ ...input, last_error: "boom" });
      expect(layout.lead).toBe("gate");
      expect(layout.gateCarriesError).toBe(true);
      expect(layout.tail).not.toContain("rawError" as never);
      expect(rawErrorEntry(layout)).toBe("none");
    }
  });

  it("无 gate 且有失败原因时保留一个默认收起的原始错误入口", () => {
    for (const input of [base, { ...base, stage: "completed" }, { ...base, status: "canceled" }]) {
      const layout = drawerLayout({ ...input, last_error: "boom" });
      expect(layout.tail.filter((item) => (item as string) === "rawError")).toHaveLength(1);
      expect(rawErrorEntry(layout)).toBe("collapsed");
    }
  });

  it("没有失败原因时不渲染原始错误入口", () => {
    expect(rawErrorEntry(drawerLayout(base))).toBe("none");
  });

  it("验收标准只出现一次，且紧跟任务描述之后", () => {
    for (const item of cases) {
      const layout = drawerLayout(item.input);
      expect(layout.tail.filter((entry) => entry === "acceptance")).toHaveLength(1);
      expect(layout.tail.filter((entry) => entry === "description")).toHaveLength(1);
      expect(layout.tail.indexOf("acceptance")).toBe(
        layout.tail.indexOf("description") + 1,
      );
    }
  });

  it("验收反馈只在存在反馈时出现一次", () => {
    for (const item of cases) {
      expect(drawerLayout(item.input).tail.filter((entry) => entry === "feedback")).toHaveLength(0);
      expect(
        drawerLayout({ ...item.input, verifier_issues: ["需要补充边界"] }).tail.filter(
          (entry) => entry === "feedback",
        ),
      ).toHaveLength(1);
    }
    expect(drawerLayout({ ...base, verifier_issues: ["", "  "] }).tail).not.toContain(
      "feedback",
    );
  });

  it("验收反馈位于验收标准之后，并先于派生与依赖信息", () => {
    const layout = drawerLayout({ ...base, verifier_issues: ["需要补充边界"] });
    const feedback = layout.tail.indexOf("feedback");
    expect(feedback).toBeGreaterThan(layout.tail.indexOf("acceptance"));
    expect(feedback).toBeLessThan(layout.tail.indexOf("derive"));
    expect(feedback).toBeLessThan(layout.tail.indexOf("deps"));
  });

  it("执行与验收历史位于验收标准之后，且先于派生与依赖信息", () => {
    for (const item of cases) {
      const layout = drawerLayout(item.input);
      expect(layout.tail.filter((entry) => entry === "history")).toHaveLength(1);
      expect(layout.tail.indexOf("history")).toBeGreaterThan(
        layout.tail.indexOf("acceptance"),
      );
      expect(layout.tail.indexOf("history")).toBeLessThan(layout.tail.indexOf("derive"));
      expect(layout.tail.indexOf("history")).toBeLessThan(layout.tail.indexOf("deps"));
    }
  });

  it("结果摘要与验收摘要只在任务终态进入 tail", () => {
    for (const item of cases) {
      const layout = drawerLayout({ ...item.input, result_summary: "done" });
      const terminal = isTaskTerminal(item.input);
      expect(layout.tail.includes("verification")).toBe(terminal);
      // 终态中 completed/canceled 由 result 首区块承载结果；已完成里程碑在 tail 补结果。
      expect(layout.tail.includes("result")).toBe(
        terminal && layout.lead === "gate",
      );
      if (terminal) {
        expect(layout.tail.indexOf("history")).toBeGreaterThan(
          layout.tail.indexOf("verification"),
        );
      }
    }
  });

  it("进行中的任务不安排结果摘要与验收摘要区块", () => {
    for (const input of [
      base,
      { ...base, stage: "verifying" },
      { ...base, status: "paused" },
      { ...base, status: "blocked" },
      { ...base, stage: "pending" },
    ]) {
      const layout = drawerLayout({ ...input, result_summary: "done" });
      expect(layout.tail).not.toContain("result");
      expect(layout.tail).not.toContain("verification");
    }
  });

  it("结果摘要不重复：result 首区块时 tail 不再含 result", () => {
    expect(drawerLayout({ ...base, stage: "completed" }).tail).not.toContain("result");
  });

  it("gate 首区块在已完成里程碑时保留结果摘要作为裁决依据", () => {
    const layout = drawerLayout({
      stage: "completed",
      status: null,
      human_gate: "milestone",
      result_summary: "done",
    });
    expect(layout.tail).toContain("result");
  });

  it("控制条只出现在可操作任务的 tail 中", () => {
    expect(drawerLayout(base).tail).not.toContain("control");
    expect(drawerLayout({ ...base, status: "blocked" }).tail).toContain("control");
    expect(drawerLayout({ ...base, status: "canceled" }).tail).not.toContain("control");
  });
});
