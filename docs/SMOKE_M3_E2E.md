# M3 端到端冒烟 —— 用例清单与结果落盘约定

脚本：`scripts/smoke_m3_e2e.sh`

用**真实 opencode 适配器**（glm 系 agent）跑通 M3 全链路，覆盖正向主链、负路径与重启恢复。
团队池固定为 8 个实例（多角色多实例）：

| 角色 | 实例 | 适配器 | 模型 |
|---|---|---|---|
| planner | planner-1 / planner-2 | opencode | `opencode-go/glm-5.3-flash`（`DOMO_M3_MODEL` 可覆盖） |
| executor | exec-1 / exec-2 | opencode | 同上 |
| acceptor | acc-1 / acc-2 | opencode | 同上 |
| decision_maker | judge-1 / judge-2 | opencode | 同上 |

## 运行方式

```bash
bash scripts/smoke_m3_e2e.sh                 # 退出码 0 = 全部通过
DOMO_M3_PORT=8801 bash scripts/smoke_m3_e2e.sh
DOMO_M3_MODEL=opencode-go/glm-5.3-flash bash scripts/smoke_m3_e2e.sh
```

脚本可重复执行且结果稳定：每次运行独立 `mktemp` 运行目录（独立 SQLite / 工作区 / 日志），
结束自动清理服务进程；真实 agent 的 LLM 调用带超时与重试，断言基于**状态持久化**而非时序竞态。

默认模型 `opencode-go/glm-5.3-flash`（glm 系）。免费层 `opencode/glm-5.3-flash-free` 在上游
有较严的速率/配额限制（实测频繁返回 429/500），会使 planner 规划失败，故不作为默认；
如所在环境可用，可用 `DOMO_M3_MODEL=opencode/glm-5.3-flash-free` 覆盖。

## 结果与日志落盘（可复查）

每次运行结束归档到仓库 `data/smoke_m3_e2e/`：

| 文件 | 内容 |
|---|---|
| `data/smoke_m3_e2e/summary.md` | 本次运行头部信息 + 用例结果明细（逐条 PASS/FAIL） |
| `data/smoke_m3_e2e/results.md` | phase1 / phase2 的逐条检查结果与备注 |
| `data/smoke_m3_e2e/domo.log` | 服务端运行日志（`DOMO_LOG` 语义，含 opencode spawn/finish、错误等） |

另有 `RUN_DIR`（如 `/tmp/opencode/domo_m3_e2e.XXXXXX/`）保留本次运行的 SQLite、工作区与
`projects/<prj>/summary-report.md` 等产物。

> 归档目录中的 `run1` ~ `run7` 是当前（134 项检查）版本的七次连续运行，逐条检查集一致；
> 强化前的 130 项历史运行保留在 `data/smoke_m3_e2e/history-2026-10-18-pre-strengthen/` 备查。

## 用例清单

### A. 正向主链路 —— 项目 A（planner 驱动，含里程碑）

| # | 用例 | 断言要点 |
|---|---|---|
| A1 | 团队池注册 | 8 个 opencode agent、角色正确 |
| A2 | planner 产出含里程碑的规划 | `/plan` 返回 ≥2 任务、≥1 里程碑、里程碑有成员、任务均属里程碑 |
| A3 | 角色分配契约 | 任务 executor/acceptor 均已分配、非决策者、执行≠验收 |
| A4 | 进入验收标准协商 | 规划任务 `negotiation_status=pending` |
| A5 | 人确认 | `/confirm` → `running` |
| A6 | 协商→执行→验收→gate 通过 | 项目 `completed`、里程碑全 `passed`、任务全 `done` |
| A7 | 审计留痕 | `negotiation.round`/`negotiation.agreed`/`acceptance.recorded`/`milestone.passed` |
| A8 | 协商历史落库可回放 | 任务详情 `negotiation.rounds` 非空 |
| A9 | 项目总结报告自动生成 | `report.status=ready`、正文非空、`summary-report.md` 落盘 |

### B. gate 语义与退回窗口 —— 项目 B（手工受控 DAG，真实 agent）

| # | 用例 | 断言要点 |
|---|---|---|
| B1 | gate 只挂起下游分支 | 无关分支任务独立 `done`；跨里程碑下游任务被冻结（`attempt=0` 未开工） |
| B2 | 成员全 DONE → gate | `awaiting_confirm` + `milestone_gate` 收件箱项 |
| B3 | gate 退回 | 里程碑 `failed` + 重开重规划窗口（`replan/milestone_window`），下游仍冻结 |
| B4 | 窗口追加任务 | `add` 应用成功；新任务默认挂本里程碑、默认带验收与标准 |
| B5 | 成环依赖被拒 | `skipped` 含 `cycle` 原因，且不影响同批其余项应用；成环 `update` 被拒后原依赖保持不变（无半应用副作用） |
| B6 | 窗口审计 | `task.created source=milestone_window`、`milestone.window_opened`/`window_applied` |
| B7 | 窗口成员完成后重入 gate | 里程碑重入 `awaiting_confirm`，事件 `reason=window_replan_done` |
| B8 | 二次确认 → 下游解冻跑完 | 里程碑 `passed`；下游分支执行 `done`；M2 gate 通过；项目 `completed` |

### C. 决策通道负路径 —— 项目 C

| # | 用例 | 断言要点 |
|---|---|---|
| C1 | 协商失败转决策者 | 强制协商失败 → `decision(kind=negotiation_failed)`；未裁定前保持 `planned`（不开工） |
| C2 | 决策者裁决恢复 | 真实决策者 agent 裁定（`by=agent`）→ `negotiation.settled` → 任务恢复流转至 `done` |
| C3 | 决策者缺席转收件箱 | 删除全部决策者 → `decision_maker_down` 收件箱 + 决策 `down` + 任务 `suspended` |
| C4 | 不阻塞无关分支 | 已完成的无关任务不受影响 |
| C5 | 里程碑 gate 阻断项目完成 | 全部任务终态但里程碑 `awaiting_confirm` → 项目保持 `running`（不提前 `completed`、不提前生成报告，两条断言）；人工确认 gate 后项目才 `completed` 并自动生成报告 |

### D. 决策者纠偏重规划 —— 项目 D

| # | 用例 | 断言要点 |
|---|---|---|
| D1 | 上报规划问题 | `/tasks/:id/replan` → `scope=tactical` + `decision(kind=replan)` |
| D2 | 决策者批准 | 真实决策者裁定 `approve`（`by=agent`）；`replan.approved` 事件 |
| D3 | planner 复用会话产出并落地 | `replan.applied` 事件；新增任务落地进入调度且默认带验收 |
| D4 | 项目收敛 | 纠偏后项目终态（或超时归档，非阻塞记录）；进入 `completed` 后总结报告自动生成 `ready` |

### R. 重启恢复（kill -9）

| # | 用例 | 断言要点 |
|---|---|---|
| R1 | A/B/D 终态持久化 | 重启后 `completed`/`archived`、里程碑状态、报告 `ready` 均保留 |
| R2 | 协商失败现场保留 | C2 `negotiation_status=failed` 且 `suspended` |
| R3 | 决策者缺席现场保留 | 决策仍 `down`、`decision_maker_down` 收件箱仍 `open` |
| R4 | 恢复后续跑 | 重注册决策者 → 收件箱 `retry` → 裁定制 → C2 跑完 → 项目 C `completed` |

### L. 审计与运行日志

| # | 用例 | 断言要点 |
|---|---|---|
| L1 | 关键审计事件齐全 | planner/negotiation/acceptance/milestone/replan/decision/report 全链路事件存在 |
| L2 | 服务端运行日志 | 含 `opencode run spawn` / `opencode run finished`，无 `panicked at` |

## 最近一次执行记录（可复查）

| 项目 | 值 |
|---|---|
| 执行时间 | 2026-10-18 18:53:07 +08:00 |
| 模型 | `opencode-go/glm-5.3-flash`（真实 opencode，glm 系） |
| 团队池 | 8 实例：planner×2 / executor×2 / acceptor×2 / decision_maker×2 |
| 运行目录 | `/tmp/opencode/domo_m3_e2e.qBfnXe`（SQLite、工作区、`projects/<prj>/summary-report.md`） |
| 检查结果 | phase1 90/90、phase2 44/44，共 **134/134 PASS，0 FAIL**，退出码 0/0 |
| 归档 | `data/smoke_m3_e2e/run7-summary.md` / `run7-results.md` / `run7-domo.log` + latest 别名 `summary.md` / `results.md` / `domo.log` |
| 服务端日志 | `data/smoke_m3_e2e/run7-domo.log`（57 次 `opencode run spawn` / 57 次 finished、0 条 ERROR、0 次 panic；2 条已知自定义 session 回退 WARN，见非阻塞问题 1），同份写入 `data/domo.log` |
| 重启恢复 | phase1 结束后 `kill -9` 并重启，A/B/D 终态与 C 失败现场全部保留，续跑至 C `completed` 且报告 `ready` |
| 可重复性 | run1 ~ run7 连续七次 134/134 PASS，逐条检查集一致（`data/smoke_m3_e2e/STABILITY.md`） |
| 单元测试 | `cargo test`：44 passed / 0 failed |

复跑命令：

```bash
bash scripts/smoke_m3_e2e.sh   # 退出码 0 = 全部通过；每次运行独立临时目录
```

## 已修复的阻塞级缺陷（本次交付）

本次冒烟交付中发现并修复了 1 处阻塞级缺陷（会导致服务端无法编译、阻断全部用例）：

1. **调度器编译失败（`server/src/scheduler.rs`）**
   - `suspend()` 调用少了 `phase` 参数（`E0061`）：执行端失败上限分支未传入 `"execute"`；
   - `escalate()` 中把失败原因（`last_fail_reason` 返回 `Option<(String, String)>`）当作
     `Option<String>` 使用（`E0277`）。
   - 修复：补齐 `suspend(state, &conn, t, "execute")`；将失败原因二元组解构为
     具体原因字符串后回退默认文案。
   - 结果：`cargo build` / `cargo test` 恢复通过（**44 passed / 0 failed**），冒烟全绿。

## 已知非阻塞问题

当前无未决的阻塞级缺陷（历史阻塞项见上）。以下为冒烟过程中观察到、不影响验收结论的问题，记录备查：

1. **opencode 对自定义 `--session` id 的拒绝（已自动降级）**
   - 现象：进程内首次以 `--session <domo 生成 id>` 调用时，CLI 返回
     `Expected a string starting with "ses"`，日志出现 1 条 `WARN opencode run failed`。
   - 处理：适配器识别该错误后自动改用服务端分配的 session 重跑，成功并缓存判定
     （`CUSTOM_SESSION_OK`），每次服务进程启动最多多花一次失败的 spawn；不影响任务状态与产物。
   - 影响：仅首次调用的额外延迟（约 1 秒），非阻塞。

2. **LLM/CLI 传输偶发中断（已由决策通道兜底）**
   - 现象：此前的运行中曾出现一次 `Transport: The socket connection was closed unexpectedly`，
     导致该轮协商被判定失败（`negotiation_status=failed`）。
   - 处理：按既有语义登记 `negotiation_failed` 决策请求，真实决策者裁定后写回标准并恢复流转，
     任务最终 `done`；无数据丢失、不阻塞无关分支。
   - 建议（非阻塞）：对明确的瞬时传输错误增加有界重试，可减少一次不必要的决策升级；
     当前行为符合设计中的异常兜底路径，故未改动。

3. **真实 agent 输出存在非确定性**
   - 现象：planner 偶尔返回的任务/里程碑结构或协商 JSON 需要重试（脚本对 planner 规划做最多 3 次尝试，
     协商失败走决策者裁决）。
   - 处理：断言均基于持久化状态而非固定轮次/时序；脚本连续七次运行检查集一致。
   - 影响：单次运行耗时可能波动，但结果稳定，非阻塞。
