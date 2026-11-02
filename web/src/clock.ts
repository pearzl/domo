import { useEffect, useState } from "react";
import { ACTIVITY_CLOCK_MS, nextBeatDelay } from "./presentation/activity";

/// 按固定节拍推进的当前时刻。定时器总是排到下一个节拍边界，
/// 因此相对时间标签只会在节拍上变化，不受 1.5s / 2.5s 轮询时刻影响。
export function useAlignedNow(interval: number = ACTIVITY_CLOCK_MS): number {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;

    function schedule() {
      if (cancelled) return;
      timer = window.setTimeout(() => {
        setNow(Date.now());
        schedule();
      }, nextBeatDelay(Date.now(), interval));
    }

    schedule();
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [interval]);

  return now;
}
