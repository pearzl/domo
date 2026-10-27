import { useEffect, useRef, useState } from "react";
import { api, errorMessage } from "../api";

const POLL_INTERVAL_MS = 1000;
const FOLLOW_THRESHOLD_PX = 24;

interface JobOutputSnapshot {
  stdout: string;
  stderr: string;
  pid: number | null;
  running: boolean;
}

/**
 * Reads a job's stdout/stderr and keeps polling while it runs. The timer is
 * owned by this component: remounts and running-state changes replace it
 * instead of adding another one.
 */
export function LiveOutput({
  jobId,
  running: parentRunning,
}: {
  jobId: string;
  running: boolean;
}) {
  const [output, setOutput] = useState<JobOutputSnapshot | null>(null);
  const [running, setRunning] = useState(parentRunning);
  const [error, setError] = useState("");
  const [following, setFollowing] = useState(true);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const followingRef = useRef(true);

  useEffect(() => {
    let cancelled = false;
    let shouldPoll = parentRunning;
    let timer: number | undefined;
    setRunning(parentRunning);

    async function tick() {
      try {
        const next = await api.getJobOutput(jobId);
        if (cancelled) return;
        setOutput(next);
        setRunning(next.running);
        setError("");
        shouldPoll = next.running;
        if (!next.running) return;
      } catch (err) {
        if (cancelled) return;
        setError(errorMessage(err));
      }
      if (!cancelled && shouldPoll) {
        timer = window.setTimeout(() => void tick(), POLL_INTERVAL_MS);
      }
    }

    void tick();
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [jobId, parentRunning]);

  const stdout = output?.stdout ?? "";
  const stderr = output?.stderr ?? "";

  useEffect(() => {
    if (!followingRef.current) return;
    const element = scrollRef.current;
    if (element) element.scrollTop = element.scrollHeight;
  }, [stdout, stderr]);

  function handleScroll() {
    const element = scrollRef.current;
    if (!element) return;
    const atBottom =
      element.scrollHeight - element.scrollTop - element.clientHeight <=
      FOLLOW_THRESHOLD_PX;
    followingRef.current = atBottom;
    setFollowing(atBottom);
  }

  function followLatest() {
    const element = scrollRef.current;
    followingRef.current = true;
    setFollowing(true);
    if (element) element.scrollTop = element.scrollHeight;
  }

  return (
    <div className="live-output">
      <div className="live-output-head">
        <span className="live-output-title">
          {running ? "实时输出" : "运行输出"}
        </span>
        {running ? <span className="live-output-dot" aria-hidden="true" /> : null}
      </div>
      {error ? (
        <div className="live-output-error" role="status">
          读取输出失败：{error}
        </div>
      ) : null}
      <div className="live-output-scroll" ref={scrollRef} onScroll={handleScroll}>
        <OutputSection title="标准输出" value={stdout} running={running} />
        <OutputSection title="标准错误" value={stderr} running={running} />
      </div>
      {!following ? (
        <button className="ghost live-output-follow" type="button" onClick={followLatest}>
          回到最新
        </button>
      ) : null}
    </div>
  );
}

function OutputSection({
  title,
  value,
  running,
}: {
  title: string;
  value: string;
  running: boolean;
}) {
  return (
    <section className="live-output-section">
      <div className="job-block-title">{title}</div>
      {value ? (
        <pre>{value}</pre>
      ) : (
        <div className="muted live-output-empty">
          {running ? "等待输出…" : "暂无输出"}
        </div>
      )}
    </section>
  );
}
