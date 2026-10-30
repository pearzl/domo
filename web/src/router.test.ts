import { describe, expect, it } from "vitest";
import { readUsageProjectId, usagePath } from "./router";

describe("usagePath", () => {
  it("带上项目过滤参数", () => {
    expect(usagePath("p1")).toBe("/usage?project_id=p1");
  });

  it("编码特殊字符的项目 id", () => {
    expect(usagePath("p 1&x")).toBe("/usage?project_id=p+1%26x");
  });

  it("未指定项目时只返回路径", () => {
    expect(usagePath(null)).toBe("/usage");
    expect(usagePath(undefined)).toBe("/usage");
    expect(usagePath("")).toBe("/usage");
    expect(usagePath("   ")).toBe("/usage");
  });
});

describe("readUsageProjectId", () => {
  it("解析查询串中的项目 id", () => {
    expect(readUsageProjectId("?project_id=p1")).toBe("p1");
    expect(readUsageProjectId("project_id=p1")).toBe("p1");
  });

  it("返回解码后的项目 id", () => {
    expect(readUsageProjectId("?project_id=p%201%26x")).toBe("p 1&x");
    expect(readUsageProjectId(usagePath("p 1&x").split("?")[1])).toBe("p 1&x");
  });

  it("缺省参数返回 null", () => {
    expect(readUsageProjectId("")).toBeNull();
    expect(readUsageProjectId("?")).toBeNull();
    expect(readUsageProjectId("?agent_id=a1")).toBeNull();
  });

  it("空串参数返回 null", () => {
    expect(readUsageProjectId("?project_id=")).toBeNull();
    expect(readUsageProjectId("?project_id=%20%20")).toBeNull();
  });

  it("非法查询返回 null", () => {
    expect(readUsageProjectId("?project_id=%")).toBeNull();
    expect(readUsageProjectId("??&&==")).toBeNull();
  });

  it("多个同名参数取第一个非空值", () => {
    expect(readUsageProjectId("?project_id=&project_id=p2")).toBe("p2");
  });
});
