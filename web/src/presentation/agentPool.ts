import type { ProjectRolePool, Role } from "../types";
import { ROLE_LABELS } from "../ui";

/// 角色选项卡元数据；顺序与服务端返回的 roles 顺序一致。
export interface RoleTab {
  role: Role;
  label: string;
  count: number;
  unavailableCount: number;
}

/// 按服务端顺序生成角色选项卡；空角色保留标签，计数为 0。
export function roleTabs(roles: ProjectRolePool[]): RoleTab[] {
  return roles.map((group) => ({
    role: group.role,
    label: ROLE_LABELS[group.role] ?? group.role,
    count: group.entries.length,
    unavailableCount: group.entries.filter((entry) => !entry.available).length,
  }));
}

/// 默认选中的角色：合法请求优先，其次第一个有 Agent 的角色，最后回落到第一个角色。
export function resolveActiveRole(
  tabs: RoleTab[],
  requested?: Role | null,
): Role | null {
  if (tabs.length === 0) {
    return null;
  }
  if (requested && tabs.some((tab) => tab.role === requested)) {
    return requested;
  }
  const populated = tabs.find((tab) => tab.count > 0);
  return (populated ?? tabs[0]).role;
}

/// 方向键切换目标；按选项卡顺序移动并在首尾循环，当前角色不在列表中时回落到默认角色。
export function nextRoleTab(
  tabs: RoleTab[],
  current: Role | null,
  direction: "previous" | "next",
): Role | null {
  if (tabs.length === 0) {
    return null;
  }
  const currentIndex = tabs.findIndex((tab) => tab.role === current);
  if (currentIndex < 0) {
    return resolveActiveRole(tabs, current);
  }
  const step = direction === "next" ? 1 : -1;
  const last = tabs.length - 1;
  const target = tabs[currentIndex + step];
  if (currentIndex + step > last) {
    return tabs[0].role;
  }
  if (currentIndex + step < 0) {
    return tabs[last].role;
  }
  return target.role;
}
