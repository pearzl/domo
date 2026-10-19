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

已交付 **M0 设计 → M1 垂直切片 → M2 真实 Agent 接入 → M3 里程碑与重规划 → M4 流程重构与并发隔离**（见 [docs/ROADMAP.md](docs/ROADMAP.md)）。

当前能力：
- **规划**：planner 自动拆解（含里程碑）；人工确认迭代（带意见重规划，原子替换）；人工介入探讨通道
- **任务生命周期**：开工前执行者↔验收者协商验收标准（一致才 READY，失败转决策者裁定）→ 执行者制定计划 → 验收者审查 → 按计划执行 → 按计划与产物验收（打回仅重新执行）
- **Agent 池**：项目级角色优先级表；按优先级动态选人并在高优先级 agent 处排队；失败换人 + 限流熔断；人类可固定单任务 agent
- **并发隔离**：任务沙盒（git worktree / 独立子目录）；验收通过由系统合并（冲突决策者先裁）；多仓库项目串行
- **三档恢复（已落地）**：中断任务 `kill -9` 重启后依序降级——①**原生会话 resume**（agent 声明 `supports_resume` 且该中断 attempt 的真实 opencode 会话 id 已落 SQLite `session_map` 跨重启可查 → `opencode run --session <同一真实id>`，**attempt 不自增、逻辑会话名不变**）→ ②**交接摘要重建**（结构化交接书五节：目标/已完成/剩余/约束/产物路径，事件 payload 持久化 + 落盘，以新会话续作，新 id ≠ 原 id）→ ③**人工重建**（任务 `suspended` + 收件箱条目附两档失败原因，按任务去重、恢复成功自动关闭）；项目暂停期间恢复被冻结，resume 后续跑。审计事件 `task.recovered{tier∈{native_resume,handoff,manual}, outcome∈{succeeded,failed,skipped,suspended}, reason, task_id, attempt, session_id}`（每档各落一条、append-only、任务时间线与审计检索可见）；冒烟 `tests/smoke_recovery_tiers.sh`（用例清单 [docs/SMOKE_RECOVERY_TIERS.md](docs/SMOKE_RECOVERY_TIERS.md)，归档 `data/smoke_recovery_tiers/`）
- **管控**：决策收件箱裁决、决策者自动裁决（战术自决/战略转人）、暂停两态 + 强制暂停、归档
- **可观测**：事件溯源与审计（倒序/分页/筛选/关联详情）、任务轮次日志、服务端运行日志、项目总结报告
- **端到端冒烟（已实跑）**：真实 opencode 8 实例团队池跑通规划→协商→计划/执行/验收→里程碑 gate→纠偏重规划→总结报告，覆盖协商失败/成环拒绝/决策者缺席/gate 只挂下游等负路径与 `kill -9` 重启恢复；2026-10-20 连续两次 **139/139** 全绿，结果与日志归档 `data/smoke_m3_e2e/` 并保留验收快照 `data/m3-accepted-2026-10-20/`，用例清单 [docs/SMOKE_M3_E2E.md](docs/SMOKE_M3_E2E.md)
- **工作区**：默认 `~/.domo/workspace/<project_id>`，也可显式指定 local 路径或 git URL

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

- [docs/SCRIPTS.md](docs/SCRIPTS.md)：冒烟脚本与文档索引

| 文档 | 内容 |
|---|---|
| [docs/DESIGN.md](docs/DESIGN.md) | 完整产品设计：概念模型、状态机、调度规则、验收机制、角色权限、恢复策略、接入协议 |
| [docs/ROADMAP.md](docs/ROADMAP.md) | 渐进式实施路线图与各阶段验收标准 |
| [docs/API.md](docs/API.md) | 服务端 API 契约（REST + SSE），前后端对接依据 |
| [docs/SMOKE_M3_E2E.md](docs/SMOKE_M3_E2E.md) | M3 端到端冒烟：用例清单、运行方式、结果与日志归档、可重复性记录 |

## 技术栈

- 后端：Rust (Axum + Tokio + rusqlite/SQLite)
- 前端：React + Vite + TypeScript
- Agent：BYO，通过适配器接入（Mock / OpenCode，后续扩展）

## 许可证

MIT
