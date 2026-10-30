import { describe, expect, it } from "vitest";
import {
  DRAWER_LEAD_TITLES,
  canControlTask,
  drawerLayout,
  drawerLead,
  rawErrorEntry,
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

  it("验收只出现一次", () => {
    for (const item of cases) {
      const layout = drawerLayout(item.input);
      expect(layout.tail.filter((entry) => entry === "acceptance")).toHaveLength(1);
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
