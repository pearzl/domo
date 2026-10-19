# DOMO 路线图

原则：每个阶段结束时产出**可实际使用的版本**，在真实使用中迭代。每个阶段都保持：可启动、可观察、可恢复。

## 已交付

- **M0 设计定稿**：产品设计（DESIGN）、前后端契约（API）、路线图（ROADMAP）、产品说明（README）。
- **M1 垂直切片**：任务 DAG 手工规划 + Mock agent 执行/验收 + 自动调度 + 看板/组织/收件箱/审计四视图 + 事件溯源 + 暂停/恢复/归档。
- **M2 真实 Agent 接入**：OpenCode 适配器（真实执行/验收、流式日志、会话恢复、`--auto` 权限）、planner 自动规划（含里程碑）、工作区落地（local/git）。
- **M3 里程碑与重规划**：里程碑 gate（人工确认、只挂下游分支）、决策收件箱裁决通道、决策者 agent 自动裁决（战术自决/战略转人工）、人工介入探讨通道、DAG 图视图、项目总结报告
  - 开工前验收标准协商（§7.1）：executor↔acceptor 多轮协商，一致（`negotiation_status=agreed`）才 READY；超轮次/运行失败 → `failed` 转决策者裁定；未协商完成的任务不派发执行；过程与轮次历史全程留痕、跨重启续跑。
  - 决策者角色隔离（裁判不下场）：决策者实例不得被登记/编辑/指派为同项目的 executor/acceptor，动态选人也跳过；BYO 池过小时可用 `DOMO_DM_ROLE_OVERLAP=1` 显式打开例外。
- **M4 流程重构与并发隔离**：
  - 任务生命周期：执行者制定计划 → 验收者审查 → 按计划执行 → 按计划与产物验收（验收打回仅重新执行）
  - 规划确认迭代（带修改意见重规划，**原子替换**：成功才替换，失败保留原计划）
  - 并发隔离：任务沙盒（git worktree / 非代码任务独立子目录）；验收通过由系统合并，冲突交决策者先裁
  - 默认工作区（无需手动指定）；强制暂停；多仓库项目任务串行
  - Agent 池：项目级角色优先级表、按优先级动态选人并排队、失败换人与限流熔断、接入层错误分类与处置策略
  - **三档恢复降级（✅ 已落地，DESIGN §10）**：执行派发从 CLI 输出解析到的真实 opencode 会话 id 落 SQLite
    `session_map(logical, real, task_id, phase, attempt)`（跨 kill -9 可查）；中断任务重启后依序降级——
    ① `native_resume`：`supports_resume==true` **且**该中断 attempt 真实 id 可查 → `--session <同一真实id>` 续作，
    **attempt 不自增、逻辑会话名 `exec_session(task_id,attempt)` 不变**；② `handoff`：跳过/失败 → 结构化交接书
    （目标/已完成/剩余/约束/产物路径；事件 payload + 落盘 `<data>/projects/<prj>/handoffs/`）以新会话续作
    （新 session id ≠ 原 id）；③ `manual`：两档均失败 → `suspended` + 收件箱条目（payload 同含两档失败原因，
    按任务去重不堆叠，恢复成功自动关闭 `auto_recovered`），每次重启至多再进一轮三档；恢复档失败不计入
    「失败 3 次挂起」额度；项目 `paused/pausing` 期间不派发恢复，resume 后续跑；同一 task 任何恢复路径下恒只有一个
    在途 worker/句柄（任务详情暴露 `live_handles` 与 `recovery{pending,tier,manual}`）。
    **事件契约**：`task.recovered{tier∈{native_resume,handoff,manual}, outcome∈{succeeded,failed,skipped,suspended},
    reason, task_id, attempt, session_id}`——每档各落一条（成功/失败/跳过均含），append-only，任务时间线与审计
    按 `type=task.recovered` 可检索。冒烟 `tests/smoke_recovery_tiers.sh`（真实 opencode + mock，覆盖口径 1–12/13），
    用例清单 [docs/SMOKE_RECOVERY_TIERS.md](SMOKE_RECOVERY_TIERS.md)，结果归档 `data/smoke_recovery_tiers/`

## M5 — 审计深化、恢复完善与多 Agent 适配

- 审计关联详情**按事件族逐类设计**（执行失败/验收退回/决策/报告/agent 生命周期各自的关联视图与原始输出；
  agent 运行输出已归档 `agent_outputs`，按阶段/轮次回放）
- 三档恢复的运维化：僵尸会话清理、卡死判定（空闲看门狗）、压力测试收口
- 任务时间线回放、事件检索深化
- **适配器抽象 + 多 Agent 适配（本里程碑交付）**：
  - 抽离适配器接口（会话生命周期、执行/验收/规划调用、错误分类与冷却、能力声明），
    提供两种实现：**spawn 模式**（每次 `opencode run`，现状）与 **serve/HTTP 模式**（`opencode serve` + HTTP API）
  - serve 模式收益：服务端原生会话管理、结构化事件与 tool 调用、HTTP 状态码直接映射错误分类
    （429 → Retry-After 冷却）、无 argv 长度限制、token/用量统计、权限 API 细粒度控制
  - 同期接入通用 CLI 适配器与 Claude Code 适配器；评估 `opencode acp`（Agent Client Protocol）
    作为跨适配器统一协议

## M6+ — 生态与沉淀

- A2A/MCP 等更远端的协议适配
- 项目知识库与经验沉淀、项目模板
- 多仓库项目的并行策略（当前串行）：仓库级锁、跨仓改动影响评估

## 待办清单（使用中发现，按需排期）

- **真实模型冒烟实跑确认（均已通过，2026-10-20，模型 `opencode-go/glm-5.3-flash`）**：
  - `tests/smoke_m3_e2e.sh`（全链路）：**139/139 全绿**（phase1 97 + phase2 42），退出码 0/0；
    结果与关键日志归档 `data/smoke_m3_e2e/{summary.md,results.md,domo.log}`，并保留只读验收快照
    `data/m3-accepted-2026-10-20/`；用例清单见 [SMOKE_M3_E2E.md](SMOKE_M3_E2E.md)。
  - `tests/smoke_recovery_tiers.sh`（三档恢复）：**106/106 全绿**，归档 `data/smoke_recovery_tiers/`，
    用例清单见 [SMOKE_RECOVERY_TIERS.md](SMOKE_RECOVERY_TIERS.md)。
- 规划者选择防呆：下拉标注免费层易限流；失败信息点名当前规划者
- 审计：非任务级 agent 会话原始输出可视化
- 工作区资源上传（`resources/` 目录与引用约定）
- 大量事件时的检索性能优化（分页索引）
