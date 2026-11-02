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

export interface ActivityPollLoopOptions {
  /// 读取一次活动；失败时抛出，由 onError 统一转换文案。
  load: () => Promise<JobActivity>;
  /// 每次成功读取后的投影回调；同时负责清除上一次的错误。
  onActivity: (activity: JobActivity) => void;
  onError: (error: unknown) => void;
  /// 每次请求结束后调用，无论成功或失败。
  onSettled: () => void;
  /// 下一拍与当前时刻的间隔。
  nextDelay: () => number;
  schedule: (run: () => void, delay: number) => number;
  cancel: (handle: number) => void;
}

export interface ActivityPollLoop {
  /// 以给定的运行状态启动首拍；每个作业只调用一次。
  start: (running: boolean) => void;
  /// 运行状态变化时调整续拍策略：转 true 立即恢复，转 false 补一拍收尾。
  setRunning: (running: boolean) => void;
  /// 停止后续请求并取消挂起的定时器。
  stop: () => void;
}

/// 作业活动轮询的生命周期：
/// - 每一拍结束后，只有 running 为 true 才排下一拍；
/// - running 由 false 转 true 时立即补拍并恢复续拍，避免作业排队期间展开后永久停摆；
/// - running 由 true 转 false 时取消挂起的定时器，并在当前请求结束后补一拍，
///   取到作业结束时产生的尾部输出后再停止。
export function createActivityPollLoop({
  load,
  onActivity,
  onError,
  onSettled,
  nextDelay,
  schedule,
  cancel,
}: ActivityPollLoopOptions): ActivityPollLoop {
  let running = false;
  let started = false;
  let stopped = false;
  let inFlight = false;
  let finalPending = false;
  let timer: number | undefined;

  function clearTimer() {
    if (timer === undefined) return;
    cancel(timer);
    timer = undefined;
  }

  async function tick(isFinal: boolean) {
    inFlight = true;
    try {
      const next = await load();
      if (stopped) return;
      onActivity(next);
    } catch (error) {
      if (stopped) return;
      onError(error);
    } finally {
      inFlight = false;
      if (!stopped) onSettled();
    }
    if (stopped) return;
    if (finalPending && !isFinal) {
      finalPending = false;
      void tick(true);
      return;
    }
    if (!running) return;
    timer = schedule(() => {
      timer = undefined;
      void tick(false);
    }, nextDelay());
  }

  function start(nextRunning: boolean) {
    if (started) return;
    started = true;
    running = nextRunning;
    void tick(false);
  }

  function setRunning(next: boolean) {
    if (next === running) return;
    running = next;
    if (next) {
      if (!inFlight && timer === undefined) void tick(false);
      return;
    }
    clearTimer();
    if (inFlight) {
      finalPending = true;
      return;
    }
    void tick(true);
  }

  function stop() {
    stopped = true;
    clearTimer();
  }

  return { start, setRunning, stop };
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
  // 作业结束时父组件会把 running 切到 false；运行状态只决定是否续拍，
  // 不重建读取状态，已经显示的更新时间与内容保持不变。
  const runningRef = useRef(running);
  const loopRef = useRef<ActivityPollLoop | null>(null);
  runningRef.current = running;

  useEffect(() => {
    signatureRef.current = "";
    setLoading(true);
    setLastUpdatedAt(null);

    const loop = createActivityPollLoop({
      load: () => api.getJobActivity(jobId),
      onActivity: (next) => {
        setActivity(next);
        setError("");
        const signature = activitySignature(next);
        if (signature && signature !== signatureRef.current) {
          signatureRef.current = signature;
          setLastUpdatedAt(Date.now());
        }
      },
      onError: (err) => setError(errorMessage(err)),
      onSettled: () => setLoading(false),
      nextDelay: () => nextBeatDelay(Date.now(), ACTIVITY_POLL_MS),
      schedule: (run, delay) => window.setTimeout(run, delay),
      cancel: (handle) => window.clearTimeout(handle),
    });
    loopRef.current = loop;
    loop.start(runningRef.current);

    return () => {
      loop.stop();
      if (loopRef.current === loop) loopRef.current = null;
    };
  }, [jobId]);

  useEffect(() => {
    runningRef.current = running;
    loopRef.current?.setRunning(running);
  }, [running]);

  return { activity, loading, error, lastUpdatedAt };
}
