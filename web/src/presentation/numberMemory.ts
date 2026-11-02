/**
 * 数字滚动的“上一次数值”记忆：按项目会话缓存，脱离组件生命周期。
 *
 * 为什么不能只放组件状态：切换项目选项卡、折叠展开区块都会卸载组件，
 * 组件状态随之丢失，回到同一处时就会把当前值当成首次渲染而丢掉滚动动画。
 * 这里用一个进程内的 Map 按调用方给定的键（约定包含项目 id 与会话 id）记住上一次渲染的值，
 * 纯函数读写，方便单测；不触碰 localStorage 与后端，刷新页面后自然重新开始。
 */

/// 记忆容量上限：超出后按插入顺序淘汰最早的键，避免长时间使用后无限增长。
export const NUMBER_MEMORY_LIMIT = 400;

const memory = new Map<string, string>();

/// 读取上一次记住的值；没有记录时返回 null，调用方据此判定首次渲染。
/// 只读不重排：渲染阶段会调用它，保持纯查询语义。
export function readNumberMemory(key: string): string | null {
  return memory.get(key) ?? null;
}

/// 记录一次渲染的值；超过上限时淘汰最早的键。
export function writeNumberMemory(
  key: string,
  value: string,
  limit: number = NUMBER_MEMORY_LIMIT,
): void {
  memory.delete(key);
  memory.set(key, value);
  while (memory.size > Math.max(1, limit)) {
    const oldest = memory.keys().next();
    if (oldest.done) break;
    memory.delete(oldest.value);
  }
}

/// 清空记忆；测试与“换项目会话”时使用。
export function clearNumberMemory(): void {
  memory.clear();
}

/// 当前记忆条目数；仅用于测试与诊断。
export function numberMemorySize(): number {
  return memory.size;
}

/// 组合一个稳定的记忆键：会话前缀 + 具体数值槽位。
export function numberMemoryKey(session: string, slot: string): string {
  const trimmedSession = session.trim();
  const trimmedSlot = slot.trim();
  if (!trimmedSession || !trimmedSlot) {
    throw new RangeError("数字滚动记忆键的会话与槽位都不能为空");
  }
  return `${trimmedSession}::${trimmedSlot}`;
}
