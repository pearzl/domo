import type { Task } from "./types";

/**
 * 可调整排队顺序的任务：项目未归档、尚未开始、未被验收拒绝、未取消。
 * 服务端按同一口径筛选可调整队列，并以 (queue_order, seq) 排序。
 */
export function isReordableQueueTask(task: Task, archived: boolean): boolean {
  return (
    !archived &&
    task.stage === "pending" &&
    task.attempts === 0 &&
    task.rejections === 0 &&
    task.status !== "canceled"
  );
}

/** 可调整任务的 id 序列，顺序与服务端队列一致。 */
export function reordableQueueIds(tasks: Task[], archived: boolean): string[] {
  return tasks
    .filter((task) => isReordableQueueTask(task, archived))
    .sort((a, b) => a.queue_order - b.queue_order || a.seq - b.seq)
    .map((task) => task.id);
}

export interface QueueEdge {
  /** 任务在可调整队列中的位置，从 1 开始；不在队列中时为 0。 */
  position: number;
  size: number;
  canMoveUp: boolean;
  canMoveDown: boolean;
}

/** 队列边界：首项不能再上移，末项不能再下移。 */
export function queueEdge(queue: string[], taskId: string): QueueEdge {
  const index = queue.indexOf(taskId);
  return {
    position: index < 0 ? 0 : index + 1,
    size: queue.length,
    canMoveUp: index > 0,
    canMoveDown: index >= 0 && index + 1 < queue.length,
  };
}
