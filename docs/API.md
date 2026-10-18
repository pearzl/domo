# DOMO API 契约（M1）

后端 `http://127.0.0.1:8787`。所有请求/响应 JSON（除 SSE）。生产模式由 Axum 直接托管 `web/dist` 静态资源（SPA fallback 到 index.html）。

## 枚举

```
ProjectStatus: initializing | pending_confirm | running | paused | completed | archived
TaskStatus:    planned | ready | executing | awaiting_acceptance | escalated | suspended | done | cancelled
Role:          planner | executor | acceptor | decision_maker
Adapter:       mock | opencode        // M1 仅 mock
InboxType:     escalation | replan | negotiation_failed | decision_maker_down
InboxStatus:   open | resolved
```

## 对象结构

```jsonc
// Task
{
  "id": "tsk_...", "project_id": "prj_...", "milestone_id": null,
  "title": "...", "description": "...",
  "depends_on": ["tsk_..."],
  "assignee_role": "executor",
  "executor_agent_id": "agt_...", "acceptor_agent_id": "agt_...",
  "acceptance_required": true,
  "acceptance_criteria": "...",          // 协商一致后写入（M3 由 executor↔acceptor 协商）
  "negotiation_status": "pending|agreed|failed",   // M3：PLANNED 阶段协商状态
  "negotiation_round": 0,                // 已完成的协商轮次（跨重启持久化）
  "status": "executing", "attempt": 1, "rejection_count": 0, "failure_count": 0,
  "artifacts": [], "created_at": 0
}

// Agent
{
  "id": "agt_...", "name": "...", "adapter": "mock",
  "roles": ["executor", "acceptor"],
  "config": { "duration_ms": 4000 },    // 适配器私有配置（mock: duration_ms；opencode: 模型/工作目录等）
  "capabilities": { "supports_resume": false, "max_parallel": 1 },
  "status": "idle" | "busy", "current_task_ids": ["tsk_..."]
}

// InboxItem
{ "id": "inb_...", "project_id": "prj_...", "type": "escalation",
  "payload": { "task_id": "...", "reason": "rejection_limit", "detail": "..." },
  "status": "open", "resolution": null, "created_at": 0 }

// Event（审计，append-only）
{ "id": 0, "ts": 0, "project_id": "...", "task_id": null,
  "type": "task.status_changed", "payload": { "from": "...", "to": "...", "reason": "..." } }
```

Mock 适配器行为（M1 约定）：
- 执行：任务描述含 `[fail]` → 本次尝试失败；否则耗时 `duration_ms`（agent 配置，默认 4000）后成功并产出一条 artifact 文本。
- 验收：任务描述含 `[reject]` → 拒绝（触发退回/升级路径）；否则 2 秒后通过。

## REST

```
GET    /api/projects                                  → [{Project}]
POST   /api/projects        {name, brief}             → Project（status=initializing）
GET    /api/projects/:id                              → {project, tasks[], agents[], inbox_open_count}
PATCH  /api/projects/:id    {name?, brief?}           → Project（仅 initializing/pending_confirm 可改）

POST   /api/projects/:id/confirm                      → 规划确认，status→running
POST   /api/projects/:id/pause                        → status→paused（冻结含验收）
POST   /api/projects/:id/resume                       → status→running，从冻结点续跑
POST   /api/projects/:id/cancel                       → status→archived（取消全部在途）

POST   /api/projects/:id/tasks  {title, description, depends_on[], executor_agent_id,
        acceptor_agent_id, acceptance_required?, acceptance_criteria?}   → Task
PATCH  /api/tasks/:id            {title?, description?, depends_on?, executor_agent_id?,
        acceptor_agent_id?, acceptance_criteria?}                        → Task
        （文本/依赖仅 planned|ready 可改；executor/acceptor 任意非终态可改，
          在途会先摘占位并终止在途会话；挂起/升级任务换好 agent 后到收件箱「恢复重派」）
POST   /api/tasks/:id/guidance   {text}               → Guidance（作用于该任务当前/下次尝试）
GET    /api/tasks/:id            → Task 详情 + logs[] + events[]（该任务时间线）+ negotiation{}（协商轮次）
POST   /api/tasks/:id/accept     {comment?}           → 人工验收通过（等同 acceptor）
POST   /api/tasks/:id/reject     {comment}            → 人工验收退回
POST   /api/tasks/:id/negotiation {status, acceptance_criteria?} → {task, decision?}（人工写协商态；failed 触发裁决）

GET    /api/agents                                     → [{Agent}]
POST   /api/agents         {name, adapter, roles, capabilities} → Agent（M1 仅 mock）
GET    /api/inbox                                      → [{InboxItem}]（open 优先）
POST   /api/inbox/:id/resolve {resolution, note?}      → 处理收件箱事项（升级裁决：resume|reassign|relax|abandon）
GET    /api/events?project_id=&task_id=                → [{Event}] 升序
GET    /api/stream                                     → SSE（见下）
```

升级裁决语义（inbox resolve）：
- `resume`：清零计数，重派原 agent；
- `reassign`：指定/自动换 executor 重派；
- `relax`：降低验收标准后重做（escalation 且 type=acceptance 时）；
- `abandon`：任务 → cancelled，下游依赖任务进收件箱（replan）。

## SSE `/api/stream`

事件名（`event:` 行）与 data：

```
event: project   data: {<Project>}                    // 状态变化
event: task      data: {<Task>}                       // 状态/字段变化
event: log       data: {task_id, ts, line}            // 任务日志追加
event: agent     data: {<Agent>}                      // 队列/负载变化
event: inbox     data: {<InboxItem>}                  // 新增或解决
event: report    data: {<Report>}                     // 项目总结报告状态/正文变化
event: event     data: {<Event>}                      // 审计事件（前端审计页订阅用）
```

连接建立后立即推送一次 `snapshot`：`{projects, agents, inbox}`。

## 端口与数据

- 监听 `127.0.0.1:8787`（`DOMO_PORT` 可覆盖）
- SQLite：`data/domo.db`（相对启动目录；目录自动创建）
- 静态资源：`web/dist`（不存在时 `/` 返回提示文本）

---

# M2 增量（OpenCode 真实 agent + 工作区 + planner）

## Project 扩展

```jsonc
// Project 新增字段
{
  ...,
  "workspace": {                       // 可为 null
    "kind": "local" | "git",
    "path": "/abs/path",               // kind=local
    "url": "https://...", "branch": "" // kind=git（clone 到 data/workspaces/<prj_id>/）
  }
}
```

- `PATCH /api/projects/:id` 支持 `workspace`（archived 拒绝 409）。任意状态可设，git clone 在项目 confirm 时执行（已 clone 则跳过/复用）。

## Planner（自动规划）

```
POST /api/projects/:id/plan   {engine?: "opencode", planner_agent_id?, executor_agent_id?, acceptor_agent_id?}    → {tasks_added: [Task]}
              （三个角色均可指定，缺省自动选择/轮转；校验在派 planner 前完成——角色齐全、非决策者、执行≠验收、须在项目 agent 池内，失败秒回 400。\n              PATCH /projects/:id 支持 `agent_pool: [agent_id...]`（null=全部；只约束新分配：规划/介入/建任务的 execs/accs 过滤与显式指定校验））
```

- 仅 `initializing` 状态可用（409）。同步阻塞调用（LLM 常耗时 30s~5min，前端需长等待与 spinner）。
- 流程：用 planner 会话（`planner-<prj_id>`，同项目连续规划复用同一 session 获得记忆）向 opencode 发规划提示词（含项目名/简介/任务说明 + 当前 agent 池角色摘要），要求输出仅一个 JSON：`{"tasks":[{"title","description","depends_on":[数组下标],"acceptance_required":bool,"acceptance_criteria":string}]}`。
- 后端解析 JSON（容错剥离代码围栏）、校验依赖不越界不成环，**角色分配由后端完成**：executor 轮转选择有 executor 角色的 agent；acceptor 选择有 acceptor 角色且 ≠ 该任务 executor 的实例；decision_maker 实例禁止担任 executor/acceptor；池中无可用角色 → 400 并说明缺什么（自动生成的任务全部回滚）。
- 规划成功：任务创建为 planned，项目仍 initializing，等待用户确认（可继续手动增改）。
- 规划失败（LLM 无输出/JSON 解析失败/超时）：500 + 错误详情（含输出尾部 600 字符），项目保持 initializing，可重试；完整原始输出留存 `data/domo.log`。
- 解析容错：围栏块 → 整串 → 首尾 `{}` 区间 → 全部 `{×}` 组合候选取最长有效 JSON；提示词已强制「首字符 `{`、末字符 `}`、无围栏无解释」。

## 人工介入（planner 协作式重规划）

```
POST /api/projects/:id/intervene        {note: string}                 → {analysis, reply, changes: [Change]}
POST /api/projects/:id/intervene/apply  {changes: [Change]}            → {applied: [...], skipped: [{index, op?, title?, task_id?, reason}]}
```

- 仅非归档项目可用（409）；需要注册 opencode adapter 且含 `planner` 角色的实例（400 明确报错）。
- `intervene`：用规划会话（`planner-<prj_id>`，与生成规划共用 → 记忆连续）送入当前任务全貌（id/标题/状态/依赖/验收/尝试次数）+ 用户说明；planner 输出 JSON：
  `{"analysis","reply","changes":[{"op":"add","task":{...}} | {"op":"update","task_id","fields":{...}} | {"op":"cancel","task_id"}]}`。
  方案**只返回不落地**，供用户在 UI 探讨（可多轮发送）；`changes` 可为空（无需改动）。审计事件 `replan.proposed`。
- `apply`：用户确认后逐项应用，单项失败跳过并给原因，不影响其他项：
  - `add`：title 必填且与既有任务不重名；`depends_on` 用任务标题解析（既有任务或**同一批内先前新增**），未知名 → 跳过；executor/acceptor 后端轮转分配（同任务执行≠验收、决策者隔离）；成环 → 跳过；创建为 planned（`task.created` 事件 source=intervention）。
  - `update`：仅非终态任务；`fields` 允许 `title/description/depends_on(标题数组)/acceptance_required/acceptance_criteria`；改依赖做环校验。
  - `cancel`：仅非终态任务，走标准取消状态机。
  - 全部应用需过 20 条上限；`running` 项目新增任务经调度器自动推进。

## Agent 管理（组织视图）

```
PATCH  /api/agents/:id   {name?, roles?, config?, capabilities?{max_parallel?, supports_resume?}} → Agent
DELETE /api/agents/:id                                                            → {deleted}
```

- PATCH：部分更新，adapter 不可改；roles 校验同创建；事件 `agent.updated`。
- DELETE：被任何**非终态任务**（planned/ready/executing/awaiting_acceptance/suspended/escalated）引用 → 409，列出任务清单并提示先换 agent；只被已完成/取消任务引用的历史引用允许保留（显示回退为「未指派」）；事件 `agent.deleted`。

## Agent 适配器 opencode

```jsonc
// config 新增
{ "model": "provider/model",   // 可选，默认 opencode 默认模型
  "timeout_ms": 3600000 }      // 单次 run 超时（可选，默认 1 小时；规划/介入会话同上限）
// capabilities: 自带 supports_resume=true（CLI --session 支持）；max_parallel 默认 1
```

- **执行**：spawn `opencode run --session tsk_<task_id>_a<attempt> [--model m] "<prompt>"`，cwd=项目工作区（无工作区 → 400 拒绝派发该任务，提示先设置工作区）。stdout 逐行流式进任务日志；exit 0 → 成功，非 0 或超时 → 本次尝试失败（沿用 3 次挂起策略）。
- **验收**：acceptor 为 opencode agent 时，向其发审核提示词（任务说明 + 验收标准 + 执行报告），要求末行输出 `VERDICT: pass|reject`；reject 时下一行为理由（进入退回流程）。
- **planner 会话**：adapter 内部同样由 opencode 驱动（session 固定复用）。

## 决策收件箱裁决（升级项）

```
POST /api/inbox/:id/resolve  {resolution: resume|reassign|reassign_acceptor|relax|abandon, note?, agent_id?}
```

- **决策上下文**：`GET /api/inbox` 对 **open** 的 escalation 附加 `context` 字段（读时计算，兼容历史数据）：
  任务全貌（标题/描述/验收标准/尝试与失败退回计数）、最近一次失败原因（`last_error`，取自事件流的
  `task.execution_failed` / `acceptance.rejected`）、执行者/验收者 agent 摘要（名/适配器/model/状态）与
  `phase_hint`（执行端 vs 验收端问题的处理指引）。挂起/升级事件（`escalation.raised`、inbox payload）
  在创建时同样嵌入 `last_error` + executor/acceptor 摘要，事后审计无需反推。
- 按原因区分（创建事件与读时上下文均携带 `last_failure_type` 标注真实失败环节）：
  - `failure_limit` + last_failure_type=`task.execution_failed`：执行端失败（从未进入验收）→ 修 agent 配置后「恢复重派」，或「更换执行者」
  - `acceptance_failure_limit` + last_failure_type=`acceptance.run_failed`：**验收端运行失败**（执行已成功、成果保留；验收者限流/超时/配置错误）→ 换验收者后「恢复重派」；UI 徽章单独区分，默认动作自动推荐「更换验收者」
  - `rejection_limit` + last_failure_type=`acceptance.rejected`：验收退回升级（验收者真给了意见）→ 放宽/换验收者/返工后恢复
- **M4 任务生命周期**：`ready → plan_drafting → plan_review → executing → awaiting_acceptance → done`；计划打回→重拟（≥2 轮升级），验收打回→重新执行（≥2 轮升级）；`PATCH /projects/:id/pause {force:true}` 强制暂停；确认时自动默认工作区；`POST /projects/:id/plan {feedback}` 带意见重规划（覆盖未开工计划）
- **沙盒隔离**：代码任务 worktree（`~/.domo/workspace/<prj>/wt/<tsk>/<repo>/`，分支 `domo/task-<tsk>`），非代码任务 `tasks/<tsk>/`；`scope_repos` 声明涉及仓库；验收通过系统 merge（冲突→决策者 rebase/rework/manual）；`sandbox.violation` 检测直写源仓库
- **失败重试指数退避**：连续 run 级失败（执行或验收基建）后，调度器按失败连击 5s→15s→45s→120s→封顶 300s 压住重试风暴（免费层限流/供应商 5xx 场景不再雪崩）；人工恢复与编辑重派不受影响。
- `resume`：清零计数，原 agent 重派。
- `reassign`：更换执行者。`agent_id` 可显式指定（任意含 executor 角色实例）；缺省自动挑一个空闲执行者，
  **无空闲 → 409**（条目保持 open，提示指定 agent），不再出现「标成已处理但任务仍挂起」的假成功。
- `reassign_acceptor`：更换验收者（仅限 `escalated` 即验收退回升级态——执行成果保留）。终止在途验收会话、
  释放旧验收者占用、换上新 agent（清 rejection_count）→ 状态回 `awaiting_acceptance` 由调度器重派验收。
- `relax` / `abandon`：语义不变（放低标准重验 / 取消任务 + 下游生成 replan 条目）。

## 人工接管既有裁决语义不变

- 决策收件箱、暂停语义、验收人工 accept/reject 均不变；真实 agent 的失败/退回走同一状态机。

---

# M3 增量（里程碑与 gate）

## 数据模型

```jsonc
// Milestone
{ "id": "mst_...", "project_id": "prj_...", "name": "...",
  "goal": "...", "effect": "...",
  "task_ids": ["tsk_..."],                       // 与 tasks.milestone_id 同步的任务成员
  "status": "pending" | "awaiting_confirm" | "passed" | "failed" }
```

`Task` 新增 `milestone_id`（可空）。数据库迁移幂等：`milestones` 表与 `tasks.milestone_id` 列
按需创建/添加，重复启动不报错。

## REST

```
GET    /api/projects/:id                 → {project, tasks[], milestones[], agents[], inbox_open_count}
POST   /api/projects/:id/milestones       {name, goal?, effect?, task_ids?}     → Milestone
GET    /api/milestones/:id                → {milestone, tasks[]}
PATCH  /api/milestones/:id                {name?, goal?, effect?, task_ids?}    → Milestone
POST   /api/milestones/:id/confirm        {note?}                               → Milestone（gate 通过）
POST   /api/milestones/:id/fail           {note?}                               → Milestone（gate 退回）
```

- 创建里程碑时可选传入 `task_ids`（须属于本项目）完成关联；`PATCH` 改成员仅限 `pending`，
  其余字段任意非归档状态可改。
- `confirm` / `fail` 仅对 `awaiting_confirm` 或 `failed` 生效（其余 409）；两者等价于在收件箱中处理 gate 项，
  并会关闭对应的 open gate 收件箱条目。
- 任务创建 `POST /api/projects/:id/tasks` 与 `PATCH /api/tasks/:id` 接受 `milestone_id`
  （须属于本项目；`null` 表示解除关联）。文本/依赖/里程碑关联的编辑仅限开工前。

## 调度 gate 规则

- 里程碑内**全部任务 DONE** → 里程碑 `pending → awaiting_confirm`，投递 `milestone_gate` 类型
  InboxItem（payload 含 `milestone_id/name/goal/effect/task_ids/detail`），事件 `milestone.gate_opened`。
- **只挂起该里程碑的下游依赖分支**：任务的传递依赖闭包若命中任何 **未 `passed`** 里程碑的成员任务，
  且该任务不属于同一里程碑，则停止派发（`ready` 冻结、中断恢复不再重派）；无关分支继续自动推进。
- 人在收件箱 `POST /api/inbox/:id/resolve {resolution: pass|reject}`（或 milestone `confirm`/`fail`）：
  `pass → passed`（放行下游，事件 `milestone.passed`），`reject → failed`（下游保持冻结，事件 `milestone.failed`）。
- **项目无里程碑时行为与 M2 完全一致**；存在未确认 gate 时项目不进入 `completed`。
- 状态与 gate 全部落 SQLite：`kill -9` 重启后里程碑状态与收件箱 gate 项一致，
  `awaiting_confirm` 里程碑缺失 gate 项时自愈补投。
- 审计事件：`milestone.created` / `milestone.updated` / `milestone.status_changed` /
  `milestone.task_linked` / `milestone.gate_opened` / `milestone.passed` / `milestone.failed` /
  `milestone.gate_resolved`（SSE 事件名 `milestone`）。

## 里程碑窗口（gate 退回后的重规划窗口）

gate 退回（`fail` / 收件箱 `reject`）后，里程碑进入 `failed` 并**重开重规划窗口**：
投递一条 `type=replan`、`payload.kind="milestone_window"` 的 open 收件箱项（含
`milestone_id/name/goal/effect/task_ids/note`），事件 `milestone.window_opened`。下游依赖分支保持冻结。

```
POST /api/milestones/:id/window/apply  {changes: [Change], note?}
    → {milestone_id, applied: [...], skipped: [{index, op?, title?, task_id?, reason}]}
```

- 仅当该里程碑存在 open 窗口项时可用（否则 409）。复用与人工介入 / 决策者纠偏**同一**
  `apply_replan_changes` 落地逻辑：`add` 新增任务默认 `acceptance_required=true`（未给标准自动补兜底标准）、
  **默认挂到该里程碑**（可显式覆盖）；依赖按**任务标题**解析（既有任务或**同批新增**）；
  新增/修改依赖做 **DAG 环校验**——成环项被拒绝并在 `skipped` 汇报原因，**其余项继续应用**；
  `update` / `cancel` 仅限非终态任务。单项失败不影响其他项（20 项上限）。
- 应用后关闭窗口收件箱项（`resolution` 记录 `applied`/`skipped`），事件 `milestone.window_applied` /
  `milestone.window_resolved`；窗口内新增任务进入调度自动推进。窗口**成员**（含追加任务）全部 DONE 且
  窗口已关闭 → 里程碑重新进入 `awaiting_confirm`（事件 `milestone.gate_opened`，reason=`window_replan_done`），
  人工再次确认后放行下游。
- 幂等：重复退回 / 重启恢复时复用已存在的 open 窗口项；从未开过窗口的陈旧 `failed` 里程碑保持冻结，
  不会静默放行下游。
- 前端：收件箱的窗口条目为**专属操作区**（①追加任务 ②按标题调整后续规划依赖/字段/取消 ③逐项待应用方案 →
  应用窗口方案），成环项在结果区展示拒绝原因，条目随即 `resolved`；`task.created` / `task.updated` 事件带
  `source=milestone_window`，可在审计页完整回放追加与调整过程。
- 端到端冒烟：`bash scripts/smoke_milestone_window.sh`（61 项检查，覆盖确认放行 / 退回开窗 / 追加+调整 /
  同批依赖标题解析 / 环校验拒绝并汇报 / 重入 gate / 审计回放 / 前端 bundle 标记）。

## 动态重规划：决策者常规纠偏通道（执行中上报规划问题）

```
POST /api/tasks/:id/replan  {reason, detail?, reported_by?, proposed_changes?: [Change]}
    → {decision: Decision, reasons: [string], scope: "tactical"|"strategic", deduplicated?}
GET  /api/projects/:id/decisions            → [{Decision}]
POST /api/tasks/:id/decisions  {kind, proposal}          → {decision, ...}（任务级变更复核）
POST /api/projects/:id/decisions {kind, task_id?, proposal} → {decision, ...}
```

- 触发：executor/acceptor（或人代理）在任务执行/验收中发现问题即上报；同一任务已有未决
  （`pending`/`escalated`）重规划请求时幂等去重直接返回既有请求。终态任务或归档项目 → 409，
  `reason` 必填 → 400。
- 登记即写审计事件 `replan.proposed`，并创建一条 kind=`replan` 的 `Decision`。
- **权限分层**：任务挂任意里程碑，或请求/建议变更触及里程碑/项目目标（`milestone_id`/
  `milestone(s)`/`project`/`new_agent` 等键）→ `scope=strategic`，**落库即转人工收件箱**
  （`decision.escalated` + `type=replan` 的 InboxItem），决策者不得自决；否则 `scope=tactical`，
  交持有 `decision_maker` 角色的 agent 自动裁定。
- 裁定（agent `POST /api/inbox/:id/resolve` 无需介入；人工走收件箱 `approve|deny`）：
  - **批准** → 事件 `replan.approved`，`decision.ruled`（effect `replan_approved`，`planning=pending`）。
    调度器随后派发规划 worker（`replan.planning_started`）：优先复用规划会话
    `planner-<prj_id>`（memory 连续）产出 changes；无真实 planner 时采用上报者
    `proposed_changes`，再无则生成一条带验收的兜底补充任务。changes 经与人工介入通道**同一**
    落地逻辑逐项应用：**新增任务默认带验收**（`acceptance_required` 默认 true，可显式覆盖；只声明
    要求而未给标准时自动补一条兜底验收标准）、依赖按标题解析（含同批新增）、插入 DAG 前做**环校验**
    （成环/未知依赖 → 跳过并记原因，不影响其他项），新建任务为 `planned` 并进入调度。落地结果写事件
    `replan.applied`（含 `source/changes/applied/skipped`）。
  - **战略护栏**：批准后的规划产出（如 planner 新增任务时挂到里程碑）若触及里程碑/项目目标，而该决策
    本身是 `tactical` 且由决策者 agent 自决，则不自动落地——自动升级人工收件箱（`replan.escalated` +
    `decision.escalated`）等待裁决；人工裁定后重新派发规划并落地（不循环）。
  - **拒绝** → 事件 `replan.rejected`，请求关闭（`decision.ruled`，effect `replan_rejected`），
    在上报任务日志中通知上报者；**原任务流程不受影响，不产生任何新任务**。
  - 批准后若进程重启而规划尚未落地，`planning=pending` 的决策会被调度器继续派发（可恢复）。
- **暂停冻结**：暂停期间人工裁决仍立即生效（`planning=pending` 照写），但**自动规划派发冻结**，
  恢复到 `running` 后由调度器续跑，与 exec/accept 派发的冻结语义一致（决策者 agent 自决在暂停期间同样不派发）。
- 审计事件链：`replan.proposed` → `decision.requested` →（`decision.escalated` 如战略，或批准后
  `replan.escalated` 如规划产出触及战略目标）→ `decision.ruled` → `replan.approved`|`replan.rejected`
  → `replan.planning_started` → `replan.applied`（含 `task.created`/`task.updated`/`task.status_changed`）。
- 端到端冒烟：`bash scripts/smoke_replan.sh`（78 项检查，覆盖批准/拒绝/环校验/战略升级/暂停冻结/审计回放/验收标准兜底）。

## 项目总结报告（COMPLETED 自动生成 + 手工重试）

```
GET  /api/projects/:id/report   → {report: Report | null}     // 未生成时 null
POST /api/projects/:id/report   → {report: Report}            // 手工触发/失败重试（status=pending）
```

`Report` 对象：

```
{
  "id": "rpt_...", "project_id": "prj_...",
  "status": "pending|generating|ready|failed",
  "content": "# 项目总结报告 …（Markdown 正文）",   // GET /report 返回
  "narrative": "planner 撰写的概述段落",
  "error": null,                                    // failed 时为失败原因
  "path": "<data>/projects/<prj_id>/summary-report.md",
  "facts": { … },                                   // 确定性事实快照（见下）
  "attempts": 1, "created_at": 0, "updated_at": 0, "generated_at": 0
}
```

- **自动触发**：项目全部任务终态且无未决 gate → `completed` 时，调度器调用
  `report::maybe_generate_on_completion` 自动生成（幂等：已存在报告则不重复自动触发，失败靠手工重试）。
- **生成流程**：汇总事实快照（`facts`）→ 交 planner 角色 agent（优先 opencode，兼容 mock）撰写
  「报告概述 + 里程碑汇报段落」→ 与确定性章节拼装为 Markdown → 落库（`reports` 表）并写盘。
  报告正文覆盖：项目概况、**每个里程碑的 goal 与 gate 结果**、**任务状态统计**、
  **退回 / 升级 / 挂起事件摘要**、验收记录、**产物路径清单**、关键事件时间线。
  未注册 planner 角色时不阻断：回退确定性叙述，报告仍 `ready`。
- **持久化**：`<data>/projects/<prj_id>/summary-report.md`（正文）与
  `summary-report.json`（事实快照）；`GET /api/projects/:id` 附带轻量 `report` 摘要（无 content/narrative/facts）。
- **失败与重试**：planner 失败/输出为空 → 报告置 `failed`、`error` 记录原因，并 `tracing::error!`
  写入 `data/domo.log`；`POST /api/projects/:id/report` 可手工重试（`attempts` 累计）。生成中重复触发 → 409。
  进程重启时中断的 `pending/generating` 报告在启动迁移中统一置 `failed`（同样可手工重试）。
- 审计事件：`report.requested`（source=`auto|manual`）→ `report.generated` | `report.failed`（SSE 事件名 `report`）。
- 前端：项目总览对已完成/已归档/已有报告的项目提供「查看总结报告」入口，弹窗按需拉取并渲染 Markdown，
  `failed` 态显示错误与「↻ 重试生成」按钮，`report` SSE 事件实时刷新。
- 端到端冒烟：`bash scripts/smoke_report.sh`（49 项检查，覆盖自动生成 / planner 故障写日志 / 手工重试 /
  内容覆盖验收标准 / 落盘一致 / 无里程碑项目 / SSE 与前端入口标记）。

## 验收标准协商（executor↔acceptor 多轮）

任务在 `planned`（PLANNED）阶段先进入验收标准协商；协商一致前**不得派发执行**。

- **默认进入协商**：`POST /api/projects/:id/tasks` 创建时，若 `acceptance_required=true` 且同时指派了
  executor 与 acceptor，则 `negotiation_status=pending`（可显式传 `negotiation_status` 覆盖）；
  无需验收 / 缺执行者（无法协商）→ 直接 `agreed`。
- **多轮协商**：调度器为 `planned + pending + acceptance_required` 的任务派发协商 worker
  （每任务独占，不占用 agent 执行/验收负载口径）：
  executor 携带任务上下文与历史轮次（含上轮 acceptor 意见）提出/修订标准 → acceptor 审核同意或反对；
  一致 → 写 `acceptance_criteria` + `negotiation_status=agreed`；不一致继续下一轮。
- **放行闸门**：`planned → ready` 仅由 `planned_gate` 放行——需验收任务要求 `negotiation_status=agreed`
  （事件 `task.status_changed` reason=`criteria_agreed_auto`）。协商未达成（pending）或失败（failed）时保持 `planned`。
- **超轮次失败**：达 `MAX_NEGOTIATION_ROUNDS=3` 仍不一致（或 executor/acceptor 运行失败）→
  `negotiation_status=failed`，自动登记 `negotiation_failed` 决策请求交决策者裁决（幂等：已有请求不重复登记）。
  未裁定前不开工。
- **裁决落地**：决策者（agent 自决或人工收件箱）裁定后写 `negotiation_status=agreed` + 最终
  `acceptance_criteria`（事件 `negotiation.settled`，`by=agent|human`），任务恢复流转；
  决策者缺失/故障 → `decision_maker_down`（任务挂起，不阻塞无依赖分支），恢复后自动续裁。
- **留痕与可见**：每轮写事件 `negotiation.round`（`role`=executor|acceptor、`proposal`/`agree`/`comment`/`criteria`、
  可带 `carried_feedback`），过程边界写 `negotiation.started` / `negotiation.resumed` / `negotiation.agreed` /
  `negotiation.failed`。`GET /api/tasks/:id` 返回随详情附带的 `negotiation` 块：
  `{status, round, max_rounds, acceptance_criteria, rounds:[…]}`，前端任务详情「协商过程」区完整回放。
- **持久化与续跑**：`negotiation_status` 与 `negotiation_round` 落 SQLite，协商历史从事件流重建；
  `kill -9` 重启后状态不丢失，`pending` 任务由调度器从下一轮续跑，`failed` 任务待裁决者恢复后继续。
- **人工干预**：`POST /api/tasks/:id/negotiation {status: pending|agreed|failed, acceptance_criteria?}`——
  仅非终态、`failed` 仅限开工前（`planned|ready`）；置 `pending` 会清零已完成轮次重新协商；
  置 `failed` 触发决策请求；`PATCH /api/tasks/:id` 亦可在开工前改 `negotiation_status`。
- **会话复用**：opencode 适配器为 executor / acceptor 分别复用协商会话 `<task_id>_negotiate_exec` /
  `<task_id>_negotiate_acc`（memory 连续）。
- **前端展示**：任务详情抽屉渲染协商轮次（执行者提案 / 验收者同意与否与意见）、轮次进度 `round/max_rounds`
  与一致/未达成提示。
- 端到端冒烟：`bash scripts/smoke_negotiation.sh`（68 项检查，覆盖一致放行 / 多轮往复 / 未达成不派发 /
  对抗失败→裁决恢复 / `kill -9` 重启状态不丢）。



# M4 增量（三档恢复降级 · DESIGN §10）

## 事件契约 `task.recovered`（append-only）

中断任务（进程重启后仍为 `executing`、或第三档人工挂起后重启）按三档依序恢复，**每档各落一条**
`task.recovered` 事件（成功/失败/跳过均含），payload 固定字段：

```jsonc
{
  "tier": "native_resume | handoff | manual",
  "outcome": "succeeded | failed | skipped | suspended",   // manual 档挂起为 suspended
  "reason": "该档成功/失败/跳过的原因（含对上一档结果的引用）",
  "task_id": "tsk_…",
  "attempt": 1,                    // 恢复全程 attempt 不自增
  "session_id": "ses_… | null",    // 该档使用的 opencode 真实会话 id（不可知时为 null）
  // 档位专属补充：
  // handoff： "handoff": {"目标","已完成","剩余","约束","产物路径","markdown","path"}（交接书全文 + 落盘路径）
  // manual：  "tier1_reason" / "tier2_reason"（两档失败原因，同写收件箱 payload）
}
```

`agent.session_started` / `agent.session_ended`（phase=execute）新增 `real_session_id`（真实 opencode
会话 id，解析到即持久化）；恢复派发的 `session_started` 另带 `recovery_tier`，`trigger=recovery`
（失败重试派发的 trigger=`retry`，常规派发=`scheduled`）。

## 任务详情扩展

`GET /api/tasks/:id` 响应新增：

```jsonc
{
  "task": { … },
  "recovery": { "pending": true, "tier": "native_resume|handoff", "manual": false },
  "live_handles": 1,   // 当前在途 worker 句柄数（同一 task 恒 ≤1）
  …
}
```

## 收件箱（第三档人工重建）

两档均失败 → 任务 `suspended`，新增 kind=`escalation` 条目，payload `reason=recovery_exhausted`，
并携带 `tier1_reason` / `tier2_reason` / `last_error` / `attempt` / `executor`（同一任务去重：重复挂起
只更新原条目，不堆叠）。恢复成功后自动关闭（`status=resolved`，`resolution.resolution=auto_recovered`，
事件 `escalation.resolved`）；人工裁决（resume/reassign/relax/abandon）语义不变。

## 事件检索

`GET /api/events` 支持 `project_id / task_id / type（逗号分隔）/ type_prefix / from / to / order /
limit / offset`（带 `limit` 时返回 `{events,total,limit,offset}`，不带时返回数组）——审计视图与
任务时间线可直接按 `type=task.recovered` 检索三档恢复记录。

端到端冒烟：`bash scripts/smoke_recovery_tiers.sh`（真实 opencode resume + mock，用例清单见
[docs/SMOKE_RECOVERY_TIERS.md](SMOKE_RECOVERY_TIERS.md)，归档 `data/smoke_recovery_tiers/`）。
