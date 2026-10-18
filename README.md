# DOMO

> 多 Agent 协作工作台 —— 把一群 AI Agent 组织成一个团队，你只做负责人。

DOMO 是一个开源的个人项目。你提交项目说明，DOMO 组织一组 BYO（自带）的 Agent 完成：
调研分析 → 任务拆解 → 里程碑规划 → 角色分配 → **执行** → **验收** → 自动调度推进，直到项目结束。
你可以从宏观层面掌握整体进展，也可以深入任意任务查看全过程，还可以随时介入管控。

## 核心理念

- **协作团队，而非工具箱**：Agent 按角色组织（规划者 / 执行者 / 验收者 / 决策者），决策者是人类在团队中的代理。
- **执行 + 验收双步闭环**：每个任务默认必须经过执行与验收才算完成；验收标准由执行者与验收者协商一致后才允许开工。特别简单的任务可由决策者裁定豁免。
- **自动调度**：任务按依赖关系（DAG）自动推进，前置完成即启动后继，支持多任务并行。
- **动态纠偏**：执行中发现的规划问题由决策者复核处理；每个里程碑是人工确认与调整后续规划的天然窗口；无里程碑的项目可全程无需人工介入。
- **随时人工管控**：项目级暂停 / 恢复 / 取消（归档），或对单个任务的规划与执行方式给出具体指导。
- **可审计**：全过程事件溯源，任何任务的执行日志、中间产物、验收记录、决策依据可回放。
- **异常可恢复**：三档恢复降级策略（原生会话恢复 → 交接摘要重建 → 人工重建），进程重启后从最后状态续跑。

## 当前状态

项目已交付 **M1 垂直切片 → M2 真实 Agent 接入 → M2.5 使用打磨**（见 [docs/ROADMAP.md](docs/ROADMAP.md)）。
当前包含：任务 DAG 手工/AI 规划（OpenCode planner）+ 真实执行/验收（OpenCode 适配器）+ 自动调度 + 看板 UI + 决策收件箱 + 暂停/恢复/归档 + 工作区落地 + **人工介入通道**（用户说明 → planner 分析探讨 → 方案逐项应用）+ 事件审计与服务端运行日志。
决策者 agent、动态重规划与项目总结报告在 M3 陆续落地；**M3 前端三视图增强已交付**：任务视图 DAG 图（节点/边=任务/依赖，按状态着色、按里程碑分组，点击进详情，SSE 实时刷新）、项目总览里程碑进度条与待确认 gate 一键跳转、组织视图 agent 负载（当前任务 / 队列长度 / max_parallel 占用比 / 角色分布）。
**里程碑数据模型与 gate 调度已落地**：里程碑内全部任务 DONE → `awaiting_confirm` + 决策收件箱 `milestone_gate` 项（前端可一键通过/退回）；gate 只挂起该里程碑的下游依赖分支，无关分支继续自动派发；无里程碑项目行为与 M2 一致；里程碑与 gate 状态经 SQLite 在 kill -9 重启后完整恢复；迁移可重复执行。
**gate 两条裁决路径与里程碑窗口重规划已落地**：确认 → 里程碑 `passed`、下游分支自动解挂续跑并跑完；退回 → 里程碑 `failed` 并重开「里程碑窗口」（收件箱专属操作区，可追加任务 / 调整后续规划，逐项应用）——新增任务默认带验收、依赖按任务标题解析（含同批新增）、新增/修改依赖做 DAG 环校验，成环项被拒绝并汇报原因、其余项继续应用；窗口内成员全部完成后里程碑重入 `awaiting_confirm`，再次确认后放行下游；全部操作写入事件审计（`milestone.window_opened`/`window_applied`、`task.created|updated` source=`milestone_window`）并可在审计页完整回放。端到端冒烟 `scripts/smoke_milestone_window.sh`（61 项）。
**动态重规划·决策者常规纠偏通道已落地**：执行中上报规划问题 → `replan.proposed` + 决策请求 → 决策者复核；不触及里程碑/项目目标的调整自决，批准后复用规划会话产出新任务/依赖调整（新任务默认带验收、插入 DAG 做环校验、落地即进入调度），拒绝则关闭请求并通知上报者且原流程不动；触及里程碑/项目目标的请求自动升级到人工收件箱，批准后若规划产出又触及里程碑/项目目标亦自动升级人工，批准/否决写入 `replan.approved`/`replan.rejected`，全程事件可在审计页回放。
**项目总结报告已落地**：项目进入 `COMPLETED` 后自动调用 planner 角色 agent，汇总事件审计、里程碑通过/退回、任务完成/退回/失败/挂起统计、验收记录与产物清单，生成含里程碑汇报段落的 Markdown 总结报告并落盘 `<data>/projects/<prj_id>/summary-report.md`（另有事实快照 JSON），提供 `GET/POST /api/projects/:id/report` 与前端「查看总结报告」弹窗入口（SSE `report` 实时刷新）；planner 失败时报告置 `failed` 并写入 `data/domo.log`，前端可手动重试。
**验收标准协商机制（executor↔acceptor 多轮）已落地**：任务在 `planned`（PLANNED）阶段由执行者与验收者 agent 携带任务上下文与历史轮次多轮协商验收标准，一致后写入 `acceptance_criteria` 且 `negotiation_status=agreed`，`planned_gate` 才放行进入 `ready`；协商未达成前调度器不派发执行。达最大轮次（3）仍不一致 → `negotiation_status=failed`，自动登记 `negotiation_failed` 决策请求交决策者裁决（决策者缺失/故障转人工收件箱，任务挂起但不阻塞无关分支），裁定写入最终标准并恢复流转。每轮写入 `negotiation.round` 事件（executor 提案 / acceptor 同意与否与意见），任务详情「协商过程」区完整回放；已完成轮次与协商状态经 SQLite 持久化，`kill -9` 重启后不丢失、从事件流重建历史并从下一轮续跑；opencode 适配器为 executor/acceptor 各自复用协商会话（memory 连续）。端到端冒烟 `scripts/smoke_negotiation.sh`（68 项）。
**M3 端到端冒烟已交付**：`scripts/smoke_m3_e2e.sh` 用**真实 opencode**（glm 系 agent）组 8 实例团队池（planner/executor/acceptor/decision_maker 各 2），跑通完整正向链路（planner 含里程碑规划 → 人确认 → 执行者↔验收者协商标准 → 执行/验收 → 里程碑 gate 确认与退回窗口追加任务 → 决策者纠偏重规划 → 全终态自动生成总结报告）与负路径（协商失败转决策者、成环依赖被拒、决策者缺席转收件箱、gate 只挂起下游分支、未确认 gate 阻断项目完成），并含 `kill -9` 重启恢复续跑；共 134 项检查（phase1 90 / phase2 44）全绿，连续七次运行检查集一致（`data/smoke_m3_e2e/STABILITY.md`）；交付过程中发现并修复了调度器 1 处阻塞级编译缺陷（`suspend` 调用签名与失败原因解构，详见 [docs/SMOKE_M3_E2E.md](docs/SMOKE_M3_E2E.md)）；结果与关键日志归档 `data/smoke_m3_e2e/{summary.md,results.md,domo.log}`，用例清单与最近一次执行记录见 [docs/SMOKE_M3_E2E.md](docs/SMOKE_M3_E2E.md)。
**M4 · 三档恢复降级已落地（依赖 OpenCode resume，DESIGN §10）**：中断任务的恢复从「一律全新尝试重派（attempt+1、新会话）」升级为依序三档——①**原生会话恢复**：agent 声明 `supports_resume==true` 且该中断 attempt 的真实 opencode 会话 id 已持久化（执行派发从 CLI 输出解析到真实 id 即落 SQLite `session_map`，不再只存内存）→ `opencode run --session <同一真实id>` 直接续作（**attempt 不自增、逻辑会话名 `exec_session(task_id, attempt)` 不变**）；②**交接摘要重建**：resume 跳过或失败 → 从事件流+任务数据生成结构化交接书（**目标/已完成/剩余/约束/产物路径**五节，事件 payload 持久化 + 落盘 `<data>/projects/<prj>/handoffs/`）以**新会话**续作（新 session id ≠ 原 id）；③**人工重建**：两档均失败 → 任务 `suspended` + 收件箱条目（payload 同含两档失败原因，按任务去重不堆叠），**每轮重启至多再进一轮三档**，成功即脱离挂起并自动关闭原收件箱条目。恢复档失败不占用「失败 3 次挂起」额度；失败后重试仍走原 attempt+1 语义。审计契约固定：事件 `task.recovered` 携带 `tier∈{native_resume,handoff,manual} / outcome∈{succeeded,failed,skipped,suspended} / reason / task_id / attempt / session_id`，每档各落一条（成功/失败/跳过均含），append-only、任务时间线与审计视图可检索。项目 `paused` 期间中断任务不自动恢复（无新派发/无新事件），resume 后按三档续跑；同一 task 任何恢复路径下恒只有一个在途执行 worker/句柄（任务详情新增 `live_handles` 与 `recovery` 状态便于核验）。端到端冒烟 `scripts/smoke_recovery_tiers.sh`（真实 opencode + mock 混编，104 项检查，用例清单见 [docs/SMOKE_RECOVERY_TIERS.md](docs/SMOKE_RECOVERY_TIERS.md)），结果归档 `data/smoke_recovery_tiers/{summary.md,results.md,domo.log}`。

## 快速开始

```bash
# 1. 构建前端
cd web && npm install && npm run build && cd ..

# 2. 构建并启动后端（同时托管前端静态资源）
cd server && cargo run --release
# 打开 http://127.0.0.1:8787
```

数据（SQLite）存放在 `data/domo.db`；服务端运行日志同步写入 `data/domo.log`（可用 `DOMO_LOG` 改路径、`DOMO_LOG_LEVEL` 调级别，默认 info）。

## 文档

| 文档 | 内容 |
|---|---|
| [docs/DESIGN.md](docs/DESIGN.md) | 完整产品设计：概念模型、状态机、调度规则、验收机制、角色权限、恢复策略、接入协议 |
| [docs/ROADMAP.md](docs/ROADMAP.md) | 渐进式实施路线图与各阶段验收标准 |
| [docs/API.md](docs/API.md) | 服务端 API 契约（REST + SSE），前后端对接依据 |

## 技术栈

- 后端：Rust (Axum + Tokio + rusqlite/SQLite)
- 前端：React + Vite + TypeScript
- Agent：BYO，通过适配器接入（Mock / OpenCode，后续扩展）

## 许可证

MIT
