import type { AcceptanceCheck } from './types';

/**
 * 机械检查的可读格式（每行一条）：
 *   file: hello.txt              要求文件存在
 *   not-file: tmp/out.log        要求文件不存在
 *   cmd: cargo test => 0         执行命令并要求退出码（省略 => 0）
 * 空行与以 # 开头的行忽略。
 */
export function parseChecks(text: string): AcceptanceCheck[] {
  const out: AcceptanceCheck[] = [];
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    if (line.startsWith('file:')) {
      const p = line.slice('file:'.length).trim();
      if (p) out.push({ type: 'file_exists', path: p });
    } else if (line.startsWith('not-file:')) {
      const p = line.slice('not-file:'.length).trim();
      if (p) out.push({ type: 'file_not_exists', path: p });
    } else if (line.startsWith('cmd:')) {
      const rest = line.slice('cmd:'.length).trim();
      const m = rest.match(/^(.*?)(?:\s*=>\s*(-?\d+))?$/s);
      const cmd = (m?.[1] ?? rest).trim();
      if (cmd) {
        out.push({ type: 'command', cmd, expect_exit: m?.[2] ? Number(m[2]) : 0 });
      }
    }
  }
  return out;
}

export function formatChecks(checks?: AcceptanceCheck[] | null): string {
  if (!checks || checks.length === 0) return '';
  return checks
    .map(c => {
      if (c.type === 'file_exists') return `file: ${c.path}`;
      if (c.type === 'file_not_exists') return `not-file: ${c.path}`;
      return `cmd: ${c.cmd} => ${c.expect_exit ?? 0}`;
    })
    .join('\n');
}
