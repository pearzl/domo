import { useLayoutEffect, useMemo, useRef, useState } from "react";
import type { JobActivity } from "../types";
import { useAlignedNow } from "../clock";
import { ActivityStream } from "./JobActivity";
import { useJobActivity } from "../useJobActivity";
import { activitySignature, isNearBottom } from "../presentation/activity";

/// 项目分析运行中的实时区：与作业活动共享思维链主视图、固定节拍时间与紧凑分组。
/// 只读取活动投影，不改接口与 stdout/stderr 存储。
export function AnalysisLiveActivity({ jobId }: { jobId: string }) {
  const { activity, loading, error, lastUpdatedAt } = useJobActivity(jobId, true);
  const now = useAlignedNow();
  return (
    <LiveActivitySummary
      jobId={jobId}
      activity={activity}
      loading={loading}
      error={error}
      lastUpdatedAt={lastUpdatedAt}
      now={now}
      running
    />
  );
}

/// 实时区的纯展示部分：思维链默认展开并自动跟随最新内容，
/// 工具与步骤收进紧凑分组；不提供 stdout/stderr 入口。
export function LiveActivitySummary({
  jobId,
  activity,
  running,
  now,
  lastUpdatedAt,
  loading,
  error,
}: {
  jobId: string;
  activity: JobActivity | null;
  running: boolean;
  now: number;
  lastUpdatedAt: number | null;
  loading: boolean;
  error: string;
}) {
  const [following, setFollowing] = useState(true);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const followingRef = useRef(true);
  const signature = useMemo(() => activitySignature(activity), [activity]);

  // 切换作业时先恢复跟随，再执行贴底滚动，保证新作业从最新内容开始展示。
  useLayoutEffect(() => {
    followingRef.current = true;
    setFollowing(true);
  }, [jobId]);

  useLayoutEffect(() => {
    if (!followingRef.current) return;
    const element = scrollRef.current;
    if (element) element.scrollTop = element.scrollHeight;
  }, [signature, jobId]);

  function handleScroll() {
    const element = scrollRef.current;
    if (!element) return;
    const atBottom = isNearBottom(
      element.scrollHeight,
      element.scrollTop,
      element.clientHeight,
    );
    followingRef.current = atBottom;
    setFollowing(atBottom);
  }

  function followLatest() {
    followingRef.current = true;
    setFollowing(true);
    const element = scrollRef.current;
    if (element) element.scrollTop = element.scrollHeight;
  }

  return (
    <div className="message message-planner live-message">
      <div className="message-role">规划者（实时进展）</div>
      <ActivityStream
        activity={activity}
        running={running}
        following={following}
        now={now}
        lastUpdatedAt={lastUpdatedAt}
        loading={loading}
        error={error}
        limit={0}
        variant="live"
        scrollRef={scrollRef}
        onScroll={handleScroll}
        onFollow={followLatest}
      />
    </div>
  );
}
