import { useEffect, useState } from "react";
import type { Role, Task, TaskAgentPool } from "../types";
import { api, errorMessage } from "../api";
import { AgentPoolPanel } from "./AgentPoolPanel";

/// 任务 Agent 池：从项目池对应角色派生，复用项目池的 AgentPoolPanel 交互。
export function TaskAgentPoolPanel({
  task,
  onRefresh,
  disabled,
  activeRole = "executor",
}: {
  task: Task;
  onRefresh: () => Promise<void>;
  disabled: boolean;
  activeRole?: Role;
}) {
  const [pool, setPool] = useState<TaskAgentPool | null>(null);
  const [loadError, setLoadError] = useState("");

  async function refreshPool() {
    try {
      const next = await api.getTaskAgentPool(task.project_id, task.id);
      setPool(next);
      setLoadError("");
    } catch (err) {
      setLoadError(errorMessage(err));
    }
  }

  useEffect(() => {
    setPool(null);
    void refreshPool();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [task.id, task.project_id]);

  if (!pool) {
    if (!loadError) return null;
    return <div className="error-box">{loadError}</div>;
  }
  return (
    <AgentPoolPanel
      key={task.id}
      title="任务 Agent 池"
      subtitle="从项目 Agent 池对应角色快照初始化，只作用于本任务。顺序越靠前优先级越高，按严格优先级派发，池不可用时不回退项目池。"
      roles={pool.roles}
      activeRole={activeRole}
      disabled={disabled}
      scope="task"
      onReorder={(role, agentId, position) =>
        api.updateTaskAgent(task.project_id, task.id, role, agentId, { position })
      }
      onMove={(role, agentId, direction) =>
        api.updateTaskAgent(task.project_id, task.id, role, agentId, { direction })
      }
      onToggle={(role, agentId, enabled) =>
        api.updateTaskAgent(task.project_id, task.id, role, agentId, { enabled })
      }
      onRefresh={async () => {
        await refreshPool();
        await onRefresh();
      }}
    />
  );
}
