import { describe, expect, it } from "vitest";
import {
  readProjectView,
  updateProjectView,
} from "./projectView";

describe("readProjectView", () => {
  it("读取选项卡与任务，空值和重复参数按首个有效值", () => {
    expect(readProjectView("?tab=tasks&task=tsk_1")).toEqual({
      tab: "tasks",
      taskId: "tsk_1",
    });
    expect(readProjectView("?tab=&tab=report&task=%20%20&task=tsk_2")).toEqual({
      tab: "report",
      taskId: "tsk_2",
    });
  });

  it("忽略不认识的选项卡并容错非法编码", () => {
    expect(readProjectView("?tab=unknown", ["analysis", "tasks"])).toEqual({
      tab: null,
      taskId: null,
    });
    expect(readProjectView("?tab=%E0%A4%A&task=%ZZ")).toEqual({
      tab: null,
      taskId: null,
    });
  });
});

describe("updateProjectView", () => {
  it("替换选项卡与任务，保留无关参数", () => {
    expect(
      updateProjectView("?foo=1&tab=analysis&task=tsk_1", {
        tab: "report",
        taskId: "tsk/2",
      }),
    ).toBe("?foo=1&tab=report&task=tsk%2F2");
  });

  it("null 表示删除参数，全部删除后返回空串", () => {
    expect(updateProjectView("?tab=analysis", { tab: null })).toBe("");
    expect(updateProjectView("?tab=analysis&task=tsk_1", { taskId: null })).toBe(
      "?tab=analysis",
    );
  });

  it("未提供的字段保持原值", () => {
    expect(updateProjectView("?tab=tasks", { taskId: "tsk_1" })).toBe(
      "?tab=tasks&task=tsk_1",
    );
  });
});
