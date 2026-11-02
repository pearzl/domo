import type { ProjectDetail } from "../types";
import { api } from "../api";
import { AgentPoolPanel } from "./AgentPoolPanel";
import { EventTimeline } from "./EventTimeline";

/// 顶层信息区块：项目 Agent 池与活动审计与主体选项卡同级，始终可见；
/// 活动与审计默认折叠，配置类事件按需展开查看。
export function ProjectTopBlocks({
  detail,
  onRefresh,
}: {
  detail: ProjectDetail;
  onRefresh: () => Promise<unknown>;
}) {
  const { project, tasks, jobs, events } = detail;
  const archived = project.status === "archived";

  return (
    <div className="project-blocks" data-block-set="top">
      <div className="project-block" data-block="agents">
        <AgentPoolPanel
          title="项目 Agent 池"
          subtitle="每个角色按顺序排列 Agent，顺序越靠前优先级越高。项目池在创建时继承默认团队配置，之后独立维护；角色启停只影响当前项目，项目池停用会屏蔽已有任务。"
          roles={detail.agent_pool.roles}
          disabled={archived}
          scope="project"
          onReorder={(role, agentId, position) =>
            api.updateProjectAgent(project.id, role, agentId, { position })
          }
          onMove={(role, agentId, direction) =>
            api.updateProjectAgent(project.id, role, agentId, { direction })
          }
          onToggle={(role, agentId, enabled) =>
            api.updateProjectAgent(project.id, role, agentId, { enabled })
          }
          onRefresh={onRefresh}
        />
      </div>

      <section className="panel project-block" data-block="activity">
        <details>
          <summary>活动与审计（{events.length}）</summary>
          <EventTimeline events={events} tasks={tasks} jobs={jobs} />
        </details>
      </section>
    </div>
  );
}
