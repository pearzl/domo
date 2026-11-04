import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { EventTimeline } from "../components/EventTimeline";
import { eventSummary, isKeyEventKind } from "./events";
import type { AuditEvent } from "../types";

describe("事件摘要与分层", () => {
  it("关键事件排除机械状态同步，但保留人工决策、失败和完成", () => {
    for (const kind of ["task_state_changed", "project_state_changed", "job_state_changed", "task_executed"]) expect(isKeyEventKind(kind)).toBe(false);
    for (const kind of ["task_rejected", "job_failed", "task_completed", "arbitration_retry"]) expect(isKeyEventKind(kind)).toBe(true);
  });

  it("摘要不混入 Agent 原始 JSON，普通可读文本保留并按字符截断", () => {
    const raw = '任务 T4 执行失败，将自动重试：Agent 进程退出码 Some(1): {"type":"reasoning","text":"SECRET_RAW"}';
    expect(eventSummary(raw)).toBe("任务 T4 执行失败，将自动重试");
    expect(eventSummary('{"type":"message"}')).toContain("展开查看原始详情");
    expect(eventSummary("正常完成")).toBe("正常完成");
    expect(Array.from(eventSummary("中文内容".repeat(50), 40))).toHaveLength(40);
  });

  it("默认只展示最近二十条，原始详情和更早记录仍可达", () => {
    const events = Array.from({ length: 35 }, (_, i) => ({ id: i + 1, kind: "task_retry", message: `重试记录 ${i + 1}：Agent 进程退出码 Some(1): {"type":"message","text":"RAW_MARKER"}`, message_length: 20000, created_at: "2026-01-10T10:00:00Z", project_id: "p1", task_id: null, job_id: null })) as AuditEvent[];
    const markup = renderToStaticMarkup(<EventTimeline events={events} />);
    expect(markup.match(/class="event-item"/g)).toHaveLength(20);
    expect(markup).toContain("加载更早 20 条");
    expect(markup).toContain("显示 20 / 35 条");
    expect(markup).toContain("查看完整");
    expect(markup).not.toContain("RAW_MARKER");
    expect(markup).toContain("重试记录 35");
  });
});
