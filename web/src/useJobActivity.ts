import { useEffect, useRef, useState } from "react";
import { api, errorMessage } from "./api";
import type { JobActivity } from "./types";
import {
  ACTIVITY_POLL_MS,
  activitySignature,
  nextBeatDelay,
} from "./presentation/activity";

export interface JobActivityState {
  activity: JobActivity | null;
  loading: boolean;
  error: string;
  /// 活动内容最近一次变化的本地时刻；内容未变化时保持原值，不随轮询推进。
  lastUpdatedAt: number | null;
}

/// 读取作业活动：运行中的作业按固定节拍轮询，结束后停止。
/// 组件只消费投影结果，不改变接口、stdout/stderr 存储或作业状态。
export function useJobActivity(
  jobId: string,
  running: boolean,
): JobActivityState {
  const [activity, setActivity] = useState<JobActivity | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [lastUpdatedAt, setLastUpdatedAt] = useState<number | null>(null);
  const signatureRef = useRef("");
  // 作业结束时父组件会把 running 切到 false；只停止后续轮询，
  // 不重建读取状态，已经显示的更新时间与内容保持不变。
  const runningRef = useRef(running);
  runningRef.current = running;

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;

    function apply(next: JobActivity) {
      const signature = activitySignature(next);
      setActivity(next);
      if (signature && signature !== signatureRef.current) {
        signatureRef.current = signature;
        setLastUpdatedAt(Date.now());
      }
    }

    async function tick() {
      try {
        const next = await api.getJobActivity(jobId);
        if (cancelled) return;
        apply(next);
        setError("");
      } catch (err) {
        if (cancelled) return;
        setError(errorMessage(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
      if (!cancelled && runningRef.current) {
        timer = window.setTimeout(
          () => void tick(),
          nextBeatDelay(Date.now(), ACTIVITY_POLL_MS),
        );
      }
    }

    setLoading(true);
    signatureRef.current = "";
    setLastUpdatedAt(null);
    void tick();
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [jobId]);

  return { activity, loading, error, lastUpdatedAt };
}
