import { useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type { Task } from "../types";

const COMPLETION_MS = 1600;

/** 完成反馈只响应已观察到的任务阶段跳转，首次展示的历史任务保持静态。 */
export function useTaskCompletion(tasks: Task[]): ReadonlySet<string> {
  const previous = useRef(new Map(tasks.map(task => [task.id, task.stage])));
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const [completed, setCompleted] = useState<Set<string>>(() => new Set());

  useEffect(() => {
    for (const task of tasks) {
      const stage = previous.current.get(task.id);
      if (stage && stage !== "completed" && task.stage === "completed" && task.status !== "canceled") {
        setCompleted(current => new Set(current).add(task.id));
        const timer = setTimeout(() => {
          timers.current.delete(task.id);
          setCompleted(current => {
            const next = new Set(current);
            next.delete(task.id);
            return next;
          });
        }, COMPLETION_MS);
        clearTimeout(timers.current.get(task.id));
        timers.current.set(task.id, timer);
      }
    }
    const eligible = new Set(tasks.filter(task => task.stage === "completed" && task.status !== "canceled").map(task => task.id));
    setCompleted(current => {
      if ([...current].every(id => eligible.has(id))) return current;
      return new Set([...current].filter(id => eligible.has(id)));
    });
    previous.current = new Map(tasks.map(task => [task.id, task.stage]));
  }, [tasks]);

  useEffect(() => {
    const activeTimers = timers.current;
    return () => {
      activeTimers.forEach(clearTimeout);
      activeTimers.clear();
    };
  }, []);

  return completed;
}

/** 任务标识决定循环相位，轮询和排序时节奏保持稳定。 */
export function taskMotionStyle(id: string): CSSProperties {
  let hash = 0;
  for (const character of id) hash = (hash * 31 + character.charCodeAt(0)) >>> 0;
  return { "--runtime-delay": `${-(hash % 32) / 10}s` } as CSSProperties;
}
