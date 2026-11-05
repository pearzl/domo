import { describe, expect, it } from "vitest";
import type { JobSummary } from "../types";
import { latestFailedJobId } from "./jobSelection";

function job(status: string, failure_kind: string | null = null): JobSummary {
  return { id: "job-current", status, failure_kind, can_continue: false } as JobSummary;
}

describe("新会话作业选择", () => {
  it("人工停止后即使不能继续原会话，仍可选择新会话", () => {
    expect(latestFailedJobId([job("canceled", "user_stopped")])).toBe("job-current");
  });

  it("普通取消和已完成作业不提供新会话入口", () => {
    expect(latestFailedJobId([job("canceled")])).toBeNull();
    expect(latestFailedJobId([job("succeeded")])).toBeNull();
  });

  it("后续尝试替代历史失败或人工停止作业", () => {
    expect(latestFailedJobId([job("canceled", "user_stopped"), job("running")])).toBeNull();
    expect(latestFailedJobId([job("failed"), job("succeeded")])).toBeNull();
  });
});
