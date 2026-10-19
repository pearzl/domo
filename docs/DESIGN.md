# DOMO 设计

> 本文件描述**当前设计**（不含历史演进与实施过程）。契约见 [API.md](API.md)，路线图见 [ROADMAP.md](ROADMAP.md)。

## 1. 产品定位

DOMO 是**多 Agent 协作工作台**：用户提交项目说明，agent 团队自动调研、拆解、执行、验收与调度，直至项目完成。本地优先、单用户、BYO agent（自带 agent 实例），首个适配器为 OpenCode。从 coding 工作台起步，设计面向通用任务。

## 2. 名词表

| 名词 | 含义 |
|---|---|
| 项目（Project） | 一次交付目标；拥有目标、工作区、任务 DAG、里程碑、agent 池 |
| 任务（Task） | 可执行、可验收的最小工作单元；含描述、验收标准、任务计划、产物 |
| 里程碑（Milestone） | 任务分组（交付批次）；组内全部任务完成 → gate 等人工确认 |
| 角色（Role） | planner / executor / acceptor / decision_maker；是标签而非绑定，实例可多角色（唯决策者例外：不得兼任同项目的执行/验收） |
| Agent 池 | 项目级「角色 → 按优先级排序的 agent 列表」，决定选人顺序 |
| 沙盒（Sandbox） | 任务的工作目录：git worktree 或独立子目录 |
| 收件箱（Inbox） | 所有需要人拍板事项的统一入口（升级、gate、重规划、决策） |

## 3. 项目生命周期

```
initializing ──确认规划──> running ──暂停──> pausing ──会话排空──> paused
      │                        │                                      │
      │ 带意见重新规划（原子替换） │ 全任务终态                            └──恢复──> running
      └───────────────────────> completed ──> （归档）archived（只读）
```

- **initializing**：规划者可生成/调整规划；人工确认任务拆分、里程碑与目标后才进入运行。
- **running**：调度器自动推进（计划 → 审查 → 执行 → 验收）。
- **pausing → paused**：暂停冻结一切新派发；常规暂停等待在途会话跑完当前尝试后自动转「已暂停」，也可「强制暂停」立即终止会话。暂停期间人工裁决不受限制。
- **completed**：全部任务终态，自动产出项目总结报告。
- **archived**：取消即归档，成果/日志/事件全量保留、只读。

## 4. 数据模型（要点）

- Project：name、brief（项目目标，仅用于规划类提示词）、workspace、agent_pool（角色优先级表）、repos（工作区内登记的 git 仓库）、status。
- Task：title、description、acceptance_criteria、**execution_plan**（执行者产出、验收者审查通过的计划）、depends_on、milestone_id、executor/acceptor（**动态解析结果或人类指定**）、agent_pinned、scope_repos、status、attempt、failure/rejection/plan_rejection 计数、artifacts、tried_agents。
- Milestone：name、goal、effect、task_ids、status（pending / awaiting_confirm / passed / failed）。
- Agent：adapter、roles、config（model、timeout、cooldown 等）、capabilities、status（idle/busy/deleted 软删除）。
- 事件（Event）：append-only 审计流；日志（Log）：任务级运行输出。

## 5. 任务状态机

```
planned ──协商一致(或豁免裁定)──> ready
   │                    │
   │                    │（会话被中断 → recovering：等待恢复续跑，agent 忙时在此排队）
   │                    ▼
   │                    │
   │                    ▼
   │             plan_drafting ──计划产出──> plan_review ──通过──> executing
   │                    ▲                        │打回（≥2 轮 → escalated）
   │                    └──── 重拟计划 ◄──────────┘
   │                                                 │执行完成
   │                                                 ▼
   │                                        awaiting_acceptance
   │                                          │通过 → done
   │                                          │打回（仅重新执行，≥2 轮 → escalated）
   │                                          └────────────► executing
   └─ 无需验收且豁免经决策者裁定 → ready
```

- **计划阶段**：执行者先产出任务计划（步骤、将改动的文件/产物、验证方式、风险），验收者审查通过后才开始执行。
- **协商阶段**：需验收的任务在开工前由执行者与验收者就**验收标准**多轮协商（详见 §7.1）；
  协商一致（`negotiation_status=agreed`，`acceptance_criteria` 非空）才进入 `ready`；协商失败转决策者裁定。
- **验收阶段**：验收者依据「任务计划 + 验收标准 + 交付产物」判定；打回只要求**重新执行**（计划已确认不可再改）。
- **失败**：执行/计划/验收基建失败 → 先按 agent 池换下一优先级重试（不计失败预算）；池内耗尽后计入 failure_count，≥3 次挂起（suspended）。
- **状态归一**：只有**真正派发**的任务才处于 `executing`；被中断的尝试进入 `recovering`（等待三档恢复续跑），
  等轮到自己的任务保持 `ready` —— 因此任务视图与组织视图（占用/排队）始终一致。
- 终态：done / cancelled。

## 6. 角色、Agent 池与选人

- **角色是标签**：一个实例可同时具备多角色（规划者与决策者可共存）。
- **角色隔离（裁判不下场）**：**决策者不得同时担任执行者/验收者**——它代表人类裁决，
  若在同一项目既执行又验收，「战术自决」就变成自审自裁。系统在以下入口**拒绝**该重叠：
  登记 agent（`POST /api/agents`）、编辑角色（`PATCH /api/agents/:id`）、
  任务指派（`POST|PATCH /api/tasks...`）、项目池启用（`PUT /api/projects/:id/pool`），
  且动态选人时始终跳过决策者实例。
  例外：BYO 池过小时可用环境变量 `DOMO_DM_ROLE_OVERLAP=1` 显式打开（仅影响人工显式指派）。
- **项目 agent 池**：每个角色一份**有序启用列表**（数字越小优先级越高，恒连续编号）；未列出的实例对该角色禁用；池缺省=全部具备该角色的实例（按登记顺序）。
- **动态选人**：任务**不在规划时绑定 agent**；执行时按「角色 + 池优先级」解析——**空闲不是分配依据**：任务始终分配给优先级最高的可用 agent 并在该 agent 处排队（忙则等待，不降级）。人类可为单个任务显式指定 agent（`agent_pinned`，永不自动改派）。
- **失败容错与熔断**：接入层给出错误分类与处置策略——
  | 类别 | 处置 |
  |---|---|
  | 限流/额度（rate_limit） | 该 agent **全局临时熔断**（时长由该 agent 配置/供应商提示决定），期间所有项目跳过；换下一优先级重试 |
  | 配置错误（config_error） | 熔断较长时间并提示人工修复；换下一优先级 |
  | 供应商 5xx/网络（transient） | 短期退避 + 换下一优先级 |
  | 超时（timeout） | 换下一优先级（提示调超时或拆任务） |
- **决策者**：战术事项自决（豁免裁定、任务级变更复核、合入影响评估等）；战略事项（里程碑/项目目标/新 agent/卡死）升级到人；决策者故障转收件箱。
  隔离要求：决策者实例不参与本项目的执行/验收（见上「角色隔离」），裁决请求只派给未同时担任执行/验收的决策者实例。

## 7. 规划、确认与重规划

- **生成规划**：规划者（可选指定实例）依据项目目标、团队与工作区仓库清单产出任务与里程碑。

### 7.1 验收标准协商（开工前）

- **触发**：任务创建/规划落地时为 `planned`。需验收（`acceptance_required=true`）且项目具备执行者/验收者角色候选时，
  默认进入协商（`negotiation_status=pending`）；无需验收或缺少协商角色时直接视为一致。
- **多轮协商**：调度器派发专用协商 worker——执行者提出/修订验收标准，验收者逐轮审核（可反对并给出意见），
  最多 `MAX_NEGOTIATION_ROUNDS=3` 轮。每轮写入 `negotiation.round` 事件（角色 / 提案 / 是否同意 / 意见），
  过程边界写入 `negotiation.started|resumed|agreed|failed|settled`。
- **一致即放行**：某轮验收者同意（或给出最终标准）→ 写入 `acceptance_criteria` 且 `negotiation_status=agreed`，
  `planned_gate` 才允许任务进入 `ready`；**协商未完成的 `planned` 任务不会被派发执行**。
- **失败转裁决**：超轮次仍不一致或协商运行失败 → `negotiation_status=failed`，自动登记
  `decision(kind=negotiation_failed)`（携带提案草案与完整轮次历史），由决策者裁定最终标准；
  裁定前任务保持 `planned`（决策者缺席则挂起等待），裁定落地写 `negotiation.settled` 并恢复流转。
- **留痕与恢复**：已完成轮次持久化于 `tasks.negotiation_round`，历史从事件流重建；任务详情返回
  `negotiation.rounds` 供前端回放。进程重启后从下一轮继续（会话上下文复用，执行者/验收者各自沿用同一协商会话）。
- **人工介入**：`POST /api/tasks/:id/negotiation` 可人工置 `pending|agreed|failed`（并写回标准）；
  人工置 `failed` 同样登记决策裁定，置 `pending` 会清零已完成轮次重新协商。
- **执行中重规划**：执行者/验收者可在输出末尾用 `REPLAN:` 块，或调用 `POST /api/tasks/:id/replan` 上报规划问题 → 决策者裁决（战术自决 / 战略转人工收件箱）→ 批准后复用规划会话落地（新任务默认带验收、依赖调整做环校验、单项失败不影响其余项）。
- **人工介入**：项目负责人可直接与规划者对话（共用规划会话记忆），得到调整方案后自行采纳。
- **里程碑重规划窗口**：里程碑 gate 被人工退回 → 里程碑转为 `failed` 并重开一条 `replan` 收件箱项
  （`payload.kind="milestone_window"`）；人在窗口内追加任务（默认带验收、默认挂到该里程碑）或调整后续规划，
  经 `POST /api/milestones/:id/window/apply` 逐项落地（依赖按任务标题解析、含同批新增；成环项拒绝并汇报原因、
  其余项照常应用），窗口项随即 `resolved`；窗口内成员全部完成后里程碑自动重新进入 `awaiting_confirm`，
  再经人工确认放行下游分支。全过程写入事件审计（`milestone.window_opened` / `milestone.window_applied`
  / 带 `source="milestone_window"` 的 `task.created` / `task.updated`）。

## 8. 调度规则

- 依赖满足（depends_on 全 done）才可派发；gate 只挂起该里程碑的下游分支，无关分支继续。
- 验收标准由规划者给出并随确认锁定（不再有开工前协商）。
- **多仓库项目任务串行**：同一时刻至多一个任务在途（保证合入同步与影响评估有效）。
- 失败重试采用指数退避（5s→300s 封顶），避免限流下的重试风暴。
- **卡死判定 = 空闲看门狗**：不限制 agent 的总运行时长；只要它持续产出（stdout/stderr 有输出）就继续跑，
  连续 **无任何输出** 达到该 agent 配置的超时值（`timeout_ms`）才判定卡死并终止进程（先 TERM、3 秒后 KILL）。
  该失败归入「卡死」类 → 按池换下一优先级 agent 重试。

## 9. 并发隔离与合入（沙盒）

- **沙盒位置**：永远在 DOMO 管辖目录 `~/.domo/workspace/<project_id>/` 内——
  - 代码任务（git）：`wt/<task>/<repo>/` 为 worktree（分支 `domo/task-<id>`）
  - 非代码任务：`tasks/<task>/` 独立子目录
- **源仓库只读心智**：用户指定的工作区（local 路径 / git URL clone）只作为源，改动只发生在 worktree；合并时才写回源仓库。
- **多仓库**：确认时扫描登记工作区内 git 仓库；任务用 `scope_repos` 声明涉及仓库（planner 可产出、人工可改，缺省主仓库）。
- **合入流程**：验收通过 → 系统把任务分支**同步到 main 最新**（rebase）后 `merge --no-ff` → 若该仓库仍有其他在途并行任务，**决策者评估合入影响**（是否可能与其他改动产生语义错误），有风险则交人工裁决。
- **冲突**：合并冲突由决策者先裁（rebase 重试 / 退回执行者解决 / 转人工），失败进收件箱。
- **违规检测**：每轮执行后比对 scope 外源仓库的脏文件基线，发现绕过沙盒直写 → `sandbox.violation` 事件 + 收件箱。

## 10. 异常与恢复

- 卡死：执行失败 3 次挂起（suspended）、验收/计划打回 2 轮升级（escalated），均不阻塞无依赖分支。
- 进程重启：中断的在途任务走三档恢复（原生会话 resume → 交接摘要重建 → 人工重建）。
- 决策者不可用：其待办转收件箱（decision_maker_down），相关分支挂起。

## 11. 界面信息架构

- **项目总览**：项目卡片（状态过滤复选框）、目标/工作区/进度、归档与暂停/恢复（含强制暂停）。
- **任务视图**：初始化态显示**规划台**（项目目标、agent 池表格、规划者、生成/重新规划、确认规划）；运行时显示 DAG 图（默认，可切上下/左右）与看板（列内滚动）；任务详情抽屉（描述、验收标准、**任务计划**、协商/轮次日志、事件时间线、人工指导/验收）。
- **组织**：agent 登记与编辑、角色与能力、负载/队列明细。
- **收件箱**：待处理与已处理（分页），升级项带上下文（任务快照、最近失败原因、涉及 agent、处置指引）。
- **审计**：事件倒序分页、类型/时间/项目筛选、关联详情抽屉（任务快照、会话轮次、日志尾部）。

## 12. 审计与可观测性

- 事件溯源：任务/项目/里程碑/agent/决策全生命周期事件（含 `agent.session_started/ended` 的 purpose/trigger/detail、错误分类与处置策略）。
- 任务日志：按轮次拆分（轮次间最新在前、轮次内时序）。
- **agent 运行输出归档**：每次 agent 运行的完整助手输出与原始 stdout 流自动落库
  （`agent_outputs`：phase/attempt/会话/结果/文本/原始流），任务详情可展开查看、可拉全文；
  验收结论由归档文本解析得出（不要求模型做任何额外动作），审计可完整回放。
- 服务端运行日志：`data/domo.log`（stdout 同步；`DOMO_LOG`/`DOMO_LOG_LEVEL` 可调）。

## 13. 工作区与资源

- **默认工作区**：未显式指定时使用 `~/.domo/workspace/<project_id>/`（确认时自动创建；`DOMO_WORKSPACE_ROOT` 可覆盖）。
- **显式工作区**：local 路径（已有代码库/目录）或 git URL（clone 到 DOMO 管辖目录）。非 git 目录无隔离语义（任务直接在其中工作，建议 `git init`）。
- 资源（输入）与产物（输出）：产物默认落在沙盒内，`artifacts` 记录相对路径；项目级资源目录约定见 M6+ 待办。

## 14. 未来功能

- 审计关联详情按事件族逐类设计；非任务级 agent 会话原始输出可视化
- 多仓库并行（仓库级锁与跨仓影响评估）
- 项目知识库与经验沉淀、项目模板
- 更多适配器（通用 CLI、Claude Code、A2A/MCP）
- 成本核算面板（token/时长统计）

## 15. 现行关键决策

| # | 决策 |
|---|---|
| 1 | 角色是标签；实例可多角色；但**决策者不得兼任同项目的 executor/acceptor**（裁判不下场，`DOMO_DM_ROLE_OVERLAP=1` 可显式例外） |
| 2 | 任务不在规划时绑定 agent：执行时按角色 + 池优先级动态选人；**空闲不是分配依据**，任务在高优先级 agent 处排队；人类可对单任务固定 agent |
| 3 | 验收标准由规划者给出并随确认锁定；执行者先制定任务计划、验收者审查通过后才执行；验收打回只重新执行 |
| 4 | 规划确认可迭代；重规划为**原子替换**（成功才替换，失败保留原计划） |
| 5 | 决策者权限分层：战术自决、战略升级到人；决策者故障转收件箱 |
| 6 | 卡死：执行失败 3 次挂起、计划/验收打回 2 轮升级；均不阻塞无依赖分支 |
| 7 | 暂停两态（暂停中/已暂停）+ 强制暂停；取消=归档（只读） |
| 8 | 里程碑 gate 需人工确认，只挂下游分支；gate 是重规划窗口；无里程碑项目可全程无人工介入 |
| 9 | 恢复三档降级（原生 resume → 交接摘要 → 人工重建） |
| 10 | 并发隔离：任务沙盒恒在项目工作区内（worktree/子目录）；源仓库只在合并时写入；合并前同步、合并后决策者评估影响 |
| 11 | 多仓库项目任务串行 |
| 12 | agent 失败容错：接入层错误分类 + 全局熔断（限流/配置错误）+ 换下一优先级（不计失败预算），池内耗尽才计失败 |
| 13 | 默认工作区自动化；显式 local/git 可覆盖 |
| 14 | 服务端运行日志落盘 `data/domo.log`；全站时间东八区；分页单页 15 |
| 15 | 决策者自动裁决通道：豁免裁定 / 任务级变更复核（战术）由决策者 agent 自决并留痕；触及里程碑·项目目标·新 agent·卡死（战略）自动进人工收件箱；决策者实例缺失或运行故障 → 待办转 `decision_maker_down` 收件箱、相关分支挂起、无关分支不受阻 |
