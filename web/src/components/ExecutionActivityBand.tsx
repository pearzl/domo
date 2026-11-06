import type { JobSummary, Task } from "../types";
import { isTaskRunning } from "../presentation/taskList";
import { ROLE_LABELS, taskStageLabel } from "../ui";
import { taskMotionStyle, useTaskCompletion } from "./useTaskCompletion";

const MAX_LANES = 6;

export function ExecutionActivityBand({ tasks, jobs }: { tasks: Task[]; jobs: JobSummary[] }) {
  const completed = useTaskCompletion(tasks);
  const runningTasks = tasks.filter(isTaskRunning);
  const runningJobs = jobs.filter(job => job.status === "running");
  const taskIds = new Set(runningTasks.map(task => task.id));
  // 完成反馈沿用任务列表顺序，避免刚完成的任务把并行轨道挤乱。
  const taskLanes = tasks.flatMap(task => {
    const running = isTaskRunning(task);
    if (!running && !completed.has(task.id)) return [];
    return [{
      id: task.id,
      key: task.key,
      title: task.title,
      label: running ? taskStageLabel(task.stage) : "已完成",
      done: !running,
    }];
  });
  const jobLanes = runningJobs
    .filter(job => !job.task_id || !taskIds.has(job.task_id))
    .map(job => ({
      id: job.id,
      key: ROLE_LABELS[job.role] ?? job.role,
      title: job.agent_name ?? "Agent",
      label: "运行中",
      done: false,
    }));
  const lanes = [...taskLanes, ...jobLanes];
  const activeCount = runningTasks.length + jobLanes.length;
  if (lanes.length === 0) return null;

  return (
    <section className="execution-band" aria-label="实时运行状态" data-active={activeCount > 0}>
      <div className="execution-band-head">
        <div className="execution-band-heading">
          <span className="runtime-beacon" aria-hidden="true"><span /></span>
          <strong>{activeCount > 0 ? "正在推进" : "任务已完成"}</strong>
          <span className="execution-band-count" role="status">
            {runningTasks.length > 1 ? `${runningTasks.length} 个任务并行` : `${runningTasks.length} 个任务运行中`}
            <span aria-hidden="true"> · </span>{runningJobs.length} 个运行作业
          </span>
        </div>
        <span className="execution-band-caption">{activeCount > 1 ? "多路协同" : "实时运行"}</span>
      </div>
      <div className="execution-lanes">
        {lanes.slice(0, MAX_LANES).map(lane => (
          <div key={lane.id} className="execution-lane" data-completed={lane.done} style={taskMotionStyle(lane.id)} title={`${lane.key} · ${lane.title} · ${lane.label}`}>
            <div className="execution-lane-label"><span className="execution-lane-key">{lane.key}</span><span>{lane.label}</span></div>
            <div className="execution-lane-track" aria-hidden="true"><span /></div>
            <div className="execution-lane-title">{lane.title}</div>
          </div>
        ))}
      </div>
      {lanes.length > MAX_LANES ? <div className="execution-band-overflow">另有 {lanes.length - MAX_LANES} 路活动</div> : null}
    </section>
  );
}
