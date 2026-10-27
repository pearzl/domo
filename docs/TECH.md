# DOMO 技术方案

> 本文详细说明 DOMO 的技术架构与核心功能的实现路径。（本文档由人工维护，AI不得擅自修改）。

## 1. 技术选型和部署形态

DOMO 被实现为一个本地 Web 应用。能快速提供简单易用的图形界面的同时，还能很好的支持 WSL 等场景。

- 后端：Rust（Axum + Tokio + SQLite），单进程。
- 前端：React + Vite + TypeScript，构建产物（`web/dist`）由后端在启动时托管，SPA 路由 fallback 到 `index.html`。
- Agent：由用户自行配置，后端按 Agent 类型适配为独立子进程（或内置 mock），进程结束时以文件交换结构化结果。

## 2. 系统架构总览

所有运行时状态都以 SQLite 为唯一事实来源（single source of truth），调度器不做内存态决策，每个调度周期都从数据库重新推演。这保证"进程重启"与"一次作业结束"在架构上是同一类事件——都是触发一次状态对账。

```
浏览器 ──HTTP──► Axum HTTP 层 ──┬── REST API（api.rs）
        ◄──SSE────────┘        │
                               │ 读写
                        SQLite（db.rs）◄──── 状态对账（scheduler.rs，秒级 tick + 通知唤醒）
                               │                │
                               │   ┌────────────┘
                               ▼   ▼
                     作业执行（runner.rs）── 启动/收割 ──► Agent 子进程（agents.rs）
                               │                            ▲
                        git worktree（git.rs）          结果文件 result.json
                     工作区 ~/.domo/workspace/           提示词渲染（prompts.rs）
```

模块职责（`server/src/`）：

| 模块 | 职责 |
|---|---|
| `config.rs` | 启动时一次性解析环境变量（数据/工作区/日志根目录、端口、作业超时与卡死阈值等），不支持运行时修改 |
| `db.rs` | SQLite 连接（WAL、busy_timeout 10s、连接池 5）、内嵌建表语句、列级迁移、默认 Agent 种子 |
| `models.rs` | 全部常量（阶段/状态/角色/失败类型/Agent 类型）与行类型 |
| `domain.rs` | ID 生成、DAG 校验、Agent 结果协议解析（`parse_result`）、失败分类（`classify_failure`）、冷却时长解析 |
| `scheduler.rs` | 单循环调度器：启动恢复、每秒对账、按阶段派发作业、Agent 选择、阻塞重算 |
| `runner.rs` | 作业生命周期：作业目录与上下文准备、子进程执行、结果判定、任务/项目状态转移、合并、失败处理 |
| `agents.rs` | Agent 适配层：mock / opencode / cli 三种类型，提示词投递、进程管理、超时与清理 |
| `prompts.rs` | 按角色渲染提示词，内含"结果必须写到指定 JSON 路径"的协议约定 |
| `git.rs` | 全部 Git 操作：固定提交身份、worktree 增删、squash 合并、diff、commit |
| `replan.rs` | 项目重规划：作废未完成任务与在途作业、清理 worktree、回退到规划阶段 |
| `usage.rs` | 成本与用量记账、可用性计划（时段窗口）、用量限额（按天/月）的评估 |
| `events.rs` | 审计事件：落库 + broadcast 广播给前端 SSE |
| `api.rs` | 全部 REST 路由与 SSE 端点 |
| `state.rs` | 共享运行时状态：DB 连接池、配置、事件总线、调度通知通道、schedule_lock / git_lock |

全局锁约定：

- `schedule_lock`：所有任务/项目状态转移和对账过程持有。Agent 子进程运行期间绝不持有该锁，保证长作业不阻塞其他项目的调度。
- `git_lock`：所有 Git 写操作（worktree 增删、commit、merge）串行执行，避免对同一个仓库的并发 worktree 操作相互踩踏。

## 3. 目录与数据布局

三个根目录均在启动时创建，可用环境变量覆盖：

| 根目录 | 默认路径 | 覆盖变量 |
|---|---|---|
| 工作区 | `~/.domo/workspace/` | `DOMO_WORKSPACE_ROOT` |
| 数据 | `~/.domo/data/` | `DOMO_DATA_ROOT` |
| 日志 | `~/.domo/log/` | `DOMO_LOG_ROOT` |

SQLite 数据库（`domo.db`）存放在数据根目录；日志按天滚动写入日志根目录，同时输出到 stdout。

### 3.1 项目工作区

每个项目拥有独立工作区 `~/.domo/workspace/<project_id>/`，创建项目时生成：

```
<project_id>/
├── repo/                          # 项目 worktree 检出，检出到项目交付分支 `branch`
├── tasks/<task_id>/               # 任务执行 worktree（每任务一个，检出到任务分支）
├── verify/<task_id>-<job短id>/    # 验收用 detached worktree，用后即删
└── jobs/<job_id>/                 # 作业目录：prompt.md、context.json、pid、result.json
```

### 3.2 作业目录契约

一次作业 = 一次 Agent 进程调用。作业目录是 DOMO 与 Agent 的隔离交换区，全部文件在 Git 仓库之外：

| 文件 | 写入方 | 用途 |
|---|---|---|
| `prompt.md` | runner | 渲染完成的最终提示词快照，供审计回溯 |
| `context.json` | runner | 完整输入上下文：项目目标、任务详情、依赖摘要、对话历史、验收拒因、diff 统计等。mock Agent 以此为唯一输入 |
| `result.json` | Agent | 结构化输出协议，进程结束后 runner 据此判定 |
| `pid` | runner | `"{pid}:{进程启动时间}"`，用于可靠终止整个进程组 |

`pid` 记录进程启动时间用于对抗 PID 复用：终止前比对 `/proc/<pid>/stat` 中的启动时间，不匹配则跳过，不会误杀无关进程。这也使服务重启后仍能清理遗留的孤儿 Agent 进程。

## 4. 数据模型

SQLite 表结构（`db.rs` 内嵌，启动时 `CREATE TABLE IF NOT EXISTS`，旧库补齐缺失列）：

| 表 | 说明 | 关键字段 |
|---|---|---|
| `agents` | Agent 全局配置 | `kind`（mock/opencode/cli）、`roles`、`model`、`max_concurrency`、`cooldown_until`、`transient_failures`、`failure_kind`、`availability_json`（时段）、`usage_limit_json`（限额） |
| `projects` | 项目 | `goal`、`repo_path`、`base_branch`、`branch`（交付分支）、`workspace`、`stage`、`status`、`blocked_reason`、`report`/`report_feedback`、`replan_reason`/`replan_source`/`replan_requested_at` |
| `project_agents` | 项目角色池（`(project_id, role, agent_id)`） | `priority`（越小越优先）、`enabled` |
| `default_team` | 默认团队：系统级 Agent 模板（`(role, agent_id)`） | `priority`（越小越优先）、`enabled`；空库种子 Agent 初始化，存量库按旧有全局顺序快照；新建项目的项目池按它继承 |
| `tasks` | 任务 | `key`（项目内唯一，排 `canceled` 外）、`deps`、`parent_id`（派生）、`milestone`、`human_gate`、`forced_agent_*`（人工裁决指定）、`stage`/`status`、`worktree`/`branch`、`attempts`/`rejections`、`verifier_issues`、`guidance`、`result_summary`、`waiting_reason` |
| `jobs` | 作业调用记录 | `role`、`agent_id`、`attempt`、`prompt`、`result`、`stdout`/`stderr`、`exit_code`、`failure_kind`、用量四元组（`input_tokens`/`output_tokens`/`cost`/`usage_known`）、`started_at`/`finished_at` |
| `messages` | 项目级对话（用户↔规划者） | `role`、`content`，参与目标确认阶段的上下文 |
| `events` | 审计事件流水 | `project_id`、`task_id`、`kind`、`message` |

阶段与状态常量：

- 项目阶段：`goal_confirm` → `planning` → `executing` → `summarizing` → `completed`
- 项目状态：`blocked` / `running` / `pausing` / `paused` / `archived`
- 任务阶段：`pending` → `executing` → `verifying` → `completed`
- 任务状态：`queued` / `running` / `blocked` / `pausing` / `paused` / `canceled`
- 作业状态：`running` / `succeeded` / `failed` / `canceled`

阶段与状态是正交的两个维度，非法组合（如已完成阶段叠加暂停态）由各状态转移函数自行约束，不在数据层面表达。

## 5. 调度器

`scheduler.rs` 运行为单一 async 任务循环：

1. 启动时执行一次恢复（见第 10 节）。
2. 之后每秒 tick 一次，或收到调度通知（`notify` 通道，作业结束/用户操作时发送）时立即执行。
3. 对账（`reconcile`）在 `schedule_lock` 内完成，产出本轮应派发的作业 ID 列表；派发动作本身（`runner::execute_job`）在每个作业独立 spawned task 中执行，不持有 schedule_lock。

### 5.1 对账逻辑

对账只处理状态为 `running` / `pausing` 中的项目，按项目阶段分派：

- **`pausing` 中的项目**：在途作业计数归零后整体置 `paused`，并把任务批量置 `paused`。实现"优雅暂停"——不强制杀进程，等当前作业自然结束。
- **`planning` / `summarizing`**：对应角色（planner）无在途作业时，选择 Agent 派发 `goal` / `summary` 作业。
- **`executing`**：遍历任务队列（`queue_order`、`seq` 排序）：
  - 任务 `pausing` 且无在途作业 → 置 `paused`；
  - 全部任务完成 → 项目推进到 `summarizing`，派发总结作业；
  - 任务 `queued` 且前置依赖已完成（DAG 就绪检查）→ 按任务阶段选择 executor 或 verifier 角色的 Agent 并派发作业；
  - 依赖未就绪 → 记录 `waiting_reason`（等待哪些前置任务）；
  - 角色池无可用 Agent → 记录不可用原因，任务保持排队。

每轮末尾重算项目级阻塞：若项目中无在途作业、且没有任何可自动推进的任务（排除用户主动暂停的任务，那是有意的停止而非阻塞），项目置 `blocked` 并记录原因。

### 5.2 Agent 选择（`choose_agent`）

按以下优先级决策：

1. **人工裁决指定**（`forced_agent_id` + `forced_agent_role`）：临时使用指定 Agent 执行该角色作业，绕开调度策略与启用约束，仅受并发上限约束；派发后立即清除。
2. **任务级角色覆盖**（executor/verifier 单任务指定 Agent）：需具备该角色且通过门检查。
3. **项目角色池**：`project_agents` 按角色分组，`priority` 升序。候选依次过筛选：
   - Agent 仍存在且声明了该角色；
   - `enabled` 开启；
   - 不在 `cooldown_until` 冷却期内；
   - 可用性计划允许（`availability_json`：按本地时区的星期+时段窗口）；
   - 用量限额未超（`usage_limit_json`：metric ∈ tokens/input_tokens/output_tokens/cost，window ∈ day/month，按本地时区计算窗口边界）。
4. **并发与轮换**：统计每个 Agent 当前 running 作业数，选第一个未达 `max_concurrency` 的候选；上一次失败的 Agent（`last_agent_id`）被排除，避免立刻在同一个坏 Agent 上重试。

无可用 Agent 时不阻塞等待，任务保留排队并记录具体原因（含逐项分类：不可用/冷却中/超限额/不在时段），用户在界面上能看到完整解释。

## 6. 作业执行（runner）

`runner.rs` 是状态转移的执行体。设计原则：**长作业进程不持锁，所有状态转移在进程退出后原子应用**。

### 6.1 执行流程

1. 作业必须处于 `running`；项目被归档则直接取消作业。
2. 按角色确定工作目录 `cwd`：
   - `goal` / `summary` → 项目 `repo/`（规划者与总结者在项目分支上工作）；
   - `executor` → 准备任务 worktree（见第 8 节）；
   - `verifier` → 准备验收 worktree（detached，基于任务分支）。
3. 渲染提示词，写 `prompt.md` 与 `context.json`，prompt 全文落库到 `jobs.prompt`。启动前校验 `cwd` 是独立 worktree 根目录（`git rev-parse --show-toplevel` 与 `cwd` 一致），不一致则作业失败且不启动进程。
4. 执行前再次检查项目/任务暂停与归档意图，暂停态下作业标记取消且不启动进程。
5. 作业受两道看门狗约束：整体超时 `DOMO_JOB_TIMEOUT_SECS`（默认 30 分钟）与卡死阈值 `DOMO_STALL_SECS`（默认 10 分钟，0 关闭）。卡死判定基于每个作业独立的输出进度（stdout/stderr 每次读到数据即刷新），超过阈值无任何输出增长即判定卡死。任一触发都按 pid 文件杀掉该作业的进程组，记录 `job_stalled`（仅卡死）事件并按"进程失败"处理；其他作业的进程组与槽位不受影响。
6. Agent 进程结束后：
   - verifier 的验收 worktree 立即删除（一次性工作区，防止泄漏）；
   - `goal` / `summary` 作业结束后对 `repo/` 执行 `git reset --hard`，保证规划者在项目分支上不留工作区残留；
   - 重读项目/任务/作业的当前状态，若此时已被暂停/取消/归档，直接退出，该作业的结果不参与状态转移。

### 6.2 结果判定

进程退出码为 0 且未超时、未卡死 → 读取 `result.json` 并按角色解析（`parse_result`）。解析失败、协议不合法、Agent 未写结果文件，均计为作业失败。失败原因经 `classify_failure` 分类（见第 7 节）；超时与卡死都按进程失败计入 `transient`。

`finish_job` 以 `WHERE id = ? AND status = 'running'` 条件更新作业记录，保证重复执行/并发收割不产生双写；作业成功时顺带清除对应 Agent 的错误计数与冷却。状态转移在 `finish_job` 成功后才应用，二者都持有 `schedule_lock`。

### 6.3 按角色的结果处理

| 角色 | 结果类型 | 处理 |
|---|---|---|
| `goal` | questions | 写入 planner 消息，项目回 `goal_confirm` 阶段并置 `blocked`，等待用户补充 |
| `goal` | plan | DAG 校验通过后批量入库任务（事务），项目进入 `executing`；若项目已有活动任务则忽略重复规划结果 |
| `executor` | replan | 作业记为成功，转交重规划模块（见第 9 节） |
| `executor` | derive | 校验派生范围（见第 9 节），事务内创建子任务并把父任务依赖改写为"全部子任务"，父任务回到 `pending` 等待子任务完成 |
| `executor` | done | 在任务 worktree 内 `git add -A + commit`（固定身份），提交失败按执行失败处理；提交成功后任务进入 `verifying`（派生任务则直接跳过验收，squash 合入父任务分支并即完成） |
| `executor` | failed | 重试/换 Agent/阻塞（见第 7 节） |
| `verifier` | pass | 里程碑任务：在 `human_gate` 上锁等待人工二次验收；普通任务：任务分支 squash 合并到项目交付分支，合并成功即 `completed`，失败则任务 `blocked`（合并冲突供人工处理） |
| `verifier` | reject | 记录拒绝原因与 issues（累计去重，保留最近 20 条），任务退回 `executing`；连续 3 次拒绝置 `blocked` 等待人工裁决 |
| `summary` | report | 报告落库，项目置 `blocked` 等待人工确认报告 |

里程碑验收、报告评议、人工裁决（换 Agent/重试/指导）等入口在 API 层（见第 12 节）。

## 7. 失败分类与容错

失败原因按内容与退出状态分类为四类（`domain::classify_failure`，关键词匹配中英双语）：

| 类型 | 对 Agent 的影响 | 对任务的影响 |
|---|---|---|
| `rate_limit`（限流） | 按 `Retry-After` 解析（支持秒/分钟/小时，上限 24h，默认 30 分钟）设置冷却并立即换下一优先级 Agent | 不消耗任务重试预算 |
| `transient`（供应商波动/超时/卡死/网络） | 连续失败计数，达到上限（默认 3 次，`DOMO_TRANSIENT_RETRY_LIMIT`）后冷却 30 分钟并换 Agent | 不消耗任务重试预算 |
| `permanent`（配置错误、鉴权失败） | 立即换下一优先级 Agent | 若换遍角色池仍无可用 Agent，任务 `blocked` |
| `task`（作业本身失败） | Agent 不受影响 | attempts +1；连续 3 次后任务 `blocked`，等待人工裁决 |

运行时基础设施错误（无法加载作业、无法准备工作目录等）由 `fail_runtime` 兜底：作业置失败，并按角色走与正常失败一致的状态转移，保证项目不会因基础设施异常而卡在"运行中"。

## 8. Git 与工作区隔离

所有 Git 操作使用固定身份（`user.name=DOMO`、`user.email=domo@localhost`、禁用 GPG 签名），使项目结果不依赖用户全局 Git 配置；全部写操作串行于 `git_lock`。

- **项目 worktree**：创建项目时以 `base_branch` 为起点在用户仓库上 `git worktree add -b <branch>`，交付分支即 `projects.branch`。
- **任务 worktree**：`tasks/<task_id>/`，分支名 `domo-task/<项目短id>/<seq>-<key>`。路径与分支持久化在任务行上：执行前若 `worktree/*/.git` 仍有效则直接复用（含重试、验收后重执行的全部轮次）；失效则删除重建。派生子任务的 worktree 基于父任务分支创建。
- **产物合并**：全部使用 squash merge，保持交付分支线性、避免中间提交噪音。派生任务完成后先合入父任务分支（父任务等于其子任务的集成层），普通任务验收通过后合入项目交付分支。
- **验收 worktree**：`verify/<task_id>-<job短id>/`，detached 检出任务分支，供验收者核查产物；作业结束后立即删除。
- **重规划清理**：作废未完成任务时同步删除其 worktree 与在途作业进程。

## 9. 重规划与任务派生

### 9.1 重规划（`replan.rs`）

触发源：执行者在结果协议中请求（`kind: "replan"`），或人工从界面发起。流程（全程持 `schedule_lock`）：

1. 仅在 `executing` / `summarizing` 阶段生效。
2. 未完成任务批量置 `canceled`（已完成任务、取消的任务行、作业记录、审计事件全部保留，可回溯）。
3. 属于未完成任务的 running 作业批量取消，随后按 pid 杀进程。
4. 收集到的 worktree 全部删除。
5. 项目回 `planning` 阶段，`replan_reason` / `replan_source` / `replan_requested_at` 落库，作为规划者作业上下文的一部分。

派生的合法范围（`validate_derive_scope`）：单次最多 8 个子任务、子任务 `deps` 必须为空、派生任务不能再派生（`parent_id` 非空即拒绝）、`key` 不与现存活动任务冲突。父任务依赖被改写为全部子任务 key，子任务全部完成后父任务以子任务集成结果重新进入执行循环。

## 10. 启动恢复与进程管理

启动时（`scheduler::recover`）：

1. **孤儿进程清理**：扫描 `running` 作业，按其项目工作区的 pid 文件杀掉可能的遗留 Agent 进程。
2. **中断作业标记**：所有 `running` 作业置 `failed`（原因"服务重启导致作业中断"）并记录时长；关联任务 attempts +1（达到 3 次则 `blocked`，否则回 `queued` 由调度器重派）；`goal` / `summary` 类项目作业把项目阶段复位，项目状态由后续对账重算。
3. **项目状态重算**：按人工闸门、阻塞任务数量重算项目 `running` / `blocked`，报告已生成等待人工确认的项目置 `blocked`。

在途的 `pausing` 项目沿用优雅暂停语义，恢复后继续等在途作业归零。

## 11. Agent 适配层

三种 Agent 类型（`agents.kind`），对外接口统一为"渲染提示词 → 启动 → 等待 → 收结果"：

| 类型 | 行为 |
|---|---|
| `mock` | 内置 mock，不启动进程。读取 `context.json` 后按角色产出确定性结果，支持通过环境变量造出失败/限流/重规划/派生/验收拒绝等场景（配合作业目录下的一次性标记文件实现"首败后重试成功"），供冒烟测试全流程复现状态机 |
| `opencode` | 调用 `opencode run --auto --format json`，可按 Agent 配置覆盖 `--model` |
| `cli` | 按用户提供的 JSON 命令模板逐项展开，支持 `{prompt}`（参数或 stdin，自动选择）、`{job_dir}`、`{cwd}`、`{model}`、`{model_args}` 占位符 |

进程环境变量注入 `DOMO_JOB_ID` / `DOMO_JOB_ROLE` / `DOMO_JOB_DIR`，并把 `PWD` 同步为 `cwd`（部分 CLI 以 `PWD` 判定工作区），进程以 `cwd` 为工作目录并用 `process_group(0)` 建立独立进程组，保证超时/取消时可整组终止。stdout/stderr 采集上限各 2MB，仅入库存档，不作为协议通道。

## 12. API 层

REST 路由（`api.rs`）按资源分组：

- `GET /api/health`：健康检查，返回 `ok`、`version`（Cargo 包版本）与 `git_hash`（构建期注入的提交短哈希；非 Git 环境为 `unknown`）
- `GET/POST /api/agents`、`PATCH/DELETE /api/agents/{id}`、`POST /api/agents/{id}/usage/reset`；优先级只在角色池/默认团队中配置，全局 Agent 创建与更新不接收优先级
- `GET/POST /api/team/default`、`PATCH/DELETE /api/team/default/{role}/{agent_id}`：默认团队模板的加入、上下移（相邻交换、编号连续）、启停与移除
- `GET /api/usage`：用量报告，支持按 project / agent / role 聚合与时间过滤
- `GET/POST /api/projects`、`GET /api/projects/{id}`（详情 + 任务视图 + 作业流水 + 项目用量）
- 项目手术：`confirm`（确认目标/答复问题）、`pause`、`resume`、`retry`、`replan`、`archive`、`report`（提交报告评议结果）
- 项目角色池：`GET/POST /api/projects/{id}/agents`、`PATCH .../agents/{role}/{agent_id}`；项目池在创建时按默认团队快照继承（角色、顺序、启停）
- 任务手术：`retry`、`guidance`（写入指导，下发次作业生效）、`agents`（任务级 agent 覆盖）、`pause`/`resume`、`queue`（排队顺序）、`milestone`（标记/接受/拒绝里程碑；拒绝可选退回执行或要求重新规划）、`arbitration`（人工裁决查询与提交）
- 审计：`GET /api/events`（回溯）、`GET /api/events/stream`（SSE 实时推送）、`GET /api/jobs/{id}`（单作业详情含 stdin/stdout/result 存储）

SSE 端点订阅 `EventBus`（tokio broadcast，容量 512），事件先落库再广播，浏览器刷新后可从事件表补齐历史。

前端（`web/src`）为单页应用：Projects / Project / Agents 三个页面，纯 fetch + SSE，无全局状态库；类型契约集中在 `types.ts`。

## 13. 成本与用量

- 记账来源：作业 `result.json` 中的可选 `usage` 对象（`input_tokens` / `output_tokens` / `cost` / `duration_ms`）。Agent 未提供的值保持 NULL，**绝不强转为 0**——聚合统计中"未知"作业单独计数（`usage_known` 标记），可用量限额只消耗已知值，避免因数据缺失低估用量。
- 可用性计划：按本地时区的星期 + 时段窗口评估，跨天/跨月自然恢复启用，无需显式重新打开。
- 用量限额：metric + window（day/month）+ limit；窗口边界按本地时区日切/月切，`usage_reset_at` 支持手动重置；限额在调度选 Agent 时评估，超限的 Agent 被排除并给出可读原因。

## 14. 测试与验证策略

- Rust 单测覆盖纯逻辑：DAG 校验、结果协议解析、失败分类、限流冷却时长解析、可用性/限额窗口计算、用量聚合。
- 冒烟脚本（`tests/test_*.sh`）默认使用 mock Agent，通过环境变量驱动"首败重试 / 重规划 / 派生 / 验收拒绝 / 限流熔断"等状态机场景，全程不依赖真实模型与外网。

## 15. 规划中的技术改动（尚未落地）

对应 DESIGN 的未来能力，当前架构预留的扩展方向：

- **项目记忆**：任务摘要、事件流水等已落库可回溯，跨项目/跨会话的知识检索与复用尚未实现。
- **运行沙箱**：Agent 子进程目前只做进程组隔离与工作目录约束，文件系统/网络级沙箱尚未实现。
- **调度策略扩展**：可用性计划、用量限额、失败冷却已实现；可用性计划中的更多维度（按并发余量、按时间序列学习）为潜在扩展点。
- **长作业流式输出**：作业 stdout 当前整体采集入档，未提供实时预览。
