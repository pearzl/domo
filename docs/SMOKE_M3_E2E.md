# M3 端到端冒烟 —— 用例清单与结果落盘约定

> **归档现状（2026-10-20）**：当前归档 `data/smoke_m3_e2e/run1` 为**当前 HEAD（M4.5 语义）**的真实模型实跑结果
> （139/139 全绿），并保留只读验收快照 `data/m3-accepted-2026-10-20/`。M3 代码时代（134 项、run1~run7
> 连续七次）的明细在环境重置后未保留，下方「M3 验收版本（历史）」仅作回放参考；M4.5 起脚本迁移到
> `tests/smoke_m3_e2e.sh`，并按「计划制定/审查」等新语义改写。

脚本：

- M3 验收版本（历史）：`scripts/smoke_m3_e2e.sh`（可用 `git show c1a97c1:scripts/smoke_m3_e2e.sh` 取回）
- 当前 HEAD 版本（M4.5 语义）：`tests/smoke_m3_e2e.sh`

用**真实 opencode 适配器**（glm 系 agent）跑通 M3 全链路，覆盖正向主链、负路径与重启恢复。
团队池固定为 8 个实例（多角色多实例）：

| 角色 | 实例 | 适配器 | 模型 |
|---|---|---|---|
| planner | planner-1 / planner-2 | opencode | `opencode-go/glm-5.3-flash`（`DOMO_M3_MODEL` 可覆盖） |
| executor | exec-1 / exec-2 | opencode | 同上 |
| acceptor | acc-1 / acc-2 | opencode | 同上 |
| decision_maker | judge-1 / judge-2 | opencode | 同上 |

## 运行方式

M3 验收时（历史，脚本后迁移为 `tests/smoke_m3_e2e.sh`）：

```bash
bash scripts/smoke_m3_e2e.sh                 # 退出码 0 = 全部通过
DOMO_M3_PORT=8801 bash scripts/smoke_m3_e2e.sh
DOMO_M3_MODEL=opencode-go/glm-5.3-flash bash scripts/smoke_m3_e2e.sh
```

当前 HEAD（M4.5 语义；模型默认 `opencode-go/glm-5.3-flash`，可用 `DOMO_M3_MODEL` 覆盖）：

```bash
bash tests/smoke_m3_e2e.sh                                    # 端口自动挑选空闲端口
DOMO_M3_PORT=8801 bash tests/smoke_m3_e2e.sh                  # 指定端口（被占用则立即失败）
DOMO_M3_MODEL=opencode-go/glm-5.3-flash bash tests/smoke_m3_e2e.sh
```

**端口隔离（可重复性修复）**：未指定 `DOMO_M3_PORT` 时脚本自动挑选一个空闲端口；显式指定但已被
占用时立即失败，绝不静默复用遗留实例。启动后会确认本次进程仍存活、且应答的是本次全新库
（`/api/projects` 为空、`DOMO_DB` 已创建），否则放弃本次启动。这从根上避免了「新进程绑定失败退出、
健康检查却连到旧服务端，请求被写进旧 DB/旧 RUN_DIR」所导致的假失败（如「项目报告未落盘」）。

脚本可重复执行且结果稳定：每次运行独立 `mktemp` 运行目录（独立 SQLite / 工作区 / 日志），
结束自动清理服务进程；真实 agent 的 LLM 调用带超时与重试，断言基于**状态持久化**而非时序竞态。

M3 验收使用默认模型 `opencode-go/glm-5.3-flash`（glm 系）。免费层 `opencode/glm-5.3-flash-free`
在上游有较严的速率/配额限制（实测频繁返回 429/500），会使 planner 规划失败，故不作为默认；
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

> 当前归档 `data/smoke_m3_e2e/run1` ~ `run3` 是当前 HEAD（M4.5 语义、139 项检查）的三次连续运行，
> 逐条检查集一致；只读快照见 `data/m3-accepted-2026-10-20/`。M3 代码时代（134 项、run1~run7）与
> 强化前（130 项）的历史明细在环境重置后未保留，相关小节仅作回放参考。

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
| C3 | 计划审查连续打回升级 | 确定性触发 `task.plan_rejected`×≥2 → 任务 `escalated` → 收件箱升级项 → 人工 `relax` → `done` |
| C4 | 决策者缺席转收件箱 | 删除全部决策者 → `decision_maker_down` 收件箱 + 决策 `down` + 任务 `suspended` |
| C5 | 不阻塞无关分支 | 已完成的无关任务不受影响 |
| C6 | 里程碑 gate 阻断项目完成 | 全部任务终态但里程碑 `awaiting_confirm` → 项目保持 `running`（不提前 `completed`、不提前生成报告，两条断言）；人工确认 gate 后项目才 `completed` 并自动生成报告 |

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
| R2 | 决策者缺席现场保留 | 项目仍 `paused`；决策仍 `down`、`decision_maker_down` 收件箱仍 `open`、任务仍 `suspended` |
| R3 | 恢复后续跑 | 重注册决策者 → 收件箱 `retry` → 裁定制 → C2 跑完 → 项目 C `running` |
| R4 | gate 阻断与放行 | 全部任务终态后里程碑 gate 打开且项目保持 `running`/无报告；确认 gate 后 `completed` + 报告 `ready` |

### L. 审计与运行日志

| # | 用例 | 断言要点 |
|---|---|---|
| L1 | 关键审计事件齐全 | planner/negotiation/acceptance/milestone/replan/decision/report 全链路事件存在 |
| L2 | 服务端运行日志 | 含 `opencode run spawn` / `opencode run finished`，无 `panicked at` |

## 最近一次执行记录（可复查）

### 当前 HEAD 冒烟（2026-10-20，M4.5 语义）

| 项目 | 值 |
|---|---|
| 执行时间 | 2026-10-20 00:34:03 +08:00 |
| 脚本 | `tests/smoke_m3_e2e.sh`（真实 opencode 适配器） |
| 模型 | `opencode-go/glm-5.3-flash`（glm 系） |
| 团队池 | 8 实例：planner×2 / executor×2 / acceptor×2 / decision_maker×2 |
| 运行目录 | `/tmp/opencode/domo_m3_e2e.nW7qNY`（SQLite、工作区、`projects/<prj>/summary-report.md`） |
| 检查结果 | phase1 97/97、phase2 42/42，共 **139/139 PASS，0 FAIL**，退出码 0/0 |
| 归档 | `data/smoke_m3_e2e/run1-*` / `run2-*` / `run3-*`（含 summary/results/domo.log）+ latest 别名 `summary.md` / `results.md` / `domo.log`；只读快照 `data/m3-accepted-2026-10-20/` |
| 服务端日志 | `run1-domo.log`（70 次 `opencode run spawn` / 69 次 `finished`，末次 spawn 在收尾 kill 前未落 finished）、`run2-domo.log`（74/74）、`run3-domo.log`（65/65）；三次均 0 条 ERROR、0 次 `panicked at`，最新一份同份写入 `data/domo.log` |
| 重启恢复 | phase1 结束后 `kill -9` 并重启，A/B/D 终态与 C 的失败现场全部保留，续跑至 C `completed` 且报告 `ready` |
| 可重复性 | run1（00:34）/ run2（01:14）/ run3（01:23，`opencode-go/glm-5.3` 全量模型）连续三次 **139/139** 通过，逐条检查集一致（`data/smoke_m3_e2e/STABILITY.md`）；M3 时代另有连续七次 134/134 记录（明细未保留，见历史小节） |
| 单元测试 | `cargo test`：71 passed / 0 failed（2026-10-20 复跑确认） |

### M3 验收版本（历史，2026-10-18）

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
| 单元测试 | `cargo test`（2026-10-20 当前 HEAD）：**71 passed / 0 failed** |

复跑命令：

```bash
# M3 历史版本（脚本已迁移，先从首个开源提交取回到 tests/ 下，保持自身路径定位 ROOT）
git show c1a97c1:scripts/smoke_m3_e2e.sh > tests/.smoke_m3_e2e.m3.sh
bash tests/.smoke_m3_e2e.m3.sh
rm -f tests/.smoke_m3_e2e.m3.sh

# 当前 HEAD 版本（M4.5 语义）
DOMO_M3_MODEL=opencode-go/glm-5.3-flash bash tests/smoke_m3_e2e.sh
```

两者退出码 0 均表示全部通过；每次运行使用独立临时目录，结果与日志按上文约定归档。

## 已修复的阻塞级缺陷（本次交付）

本次冒烟交付中发现并修复了 2 处阻塞级缺陷（分别导致服务端无法编译、失败换人时整服务挂起）：

1. **调度器编译失败（`server/src/scheduler.rs`）**
   - `suspend()` 调用少了 `phase` 参数（`E0061`）：执行端失败上限分支未传入 `"execute"`；
   - `escalate()` 中把失败原因（`last_fail_reason` 返回 `Option<(String, String)>`）当作
     `Option<String>` 使用（`E0277`）。
   - 修复：补齐 `suspend(state, &conn, t, "execute")`；将失败原因二元组解构为
     具体原因字符串后回退默认文案。
   - 结果：`cargo build` / `cargo test` 恢复通过（当时 **44 passed / 0 failed**，当前 HEAD 为 71），冒烟全绿。

2. **失败换人路径自死锁（`server/src/pool.rs`）**
   - 现象：执行/验收 agent 失败触发换人时，`switch_on_failure` 在已持有 DB 连接的
     `handle_agent_failure` 调用链中又调用 `events::log_line_state`，对同一把
     `std::sync::Mutex` 二次加锁 → 整个服务端挂起，后续所有任务停滞。
   - 修复：换人审计日志改用调用方已持有的连接（`events::log_line(state, conn, ...)`），
     消除重入加锁；失败换人语义与事件内容不变。
   - 结果：失败换人可正常继续（含 agent 池降级/重试路径），冒烟与恢复用例不再卡死；
     `cargo test` 71 passed / 0 failed。

## 本次交付的脚本稳定性修复

复跑时发现两处会破坏「可重复性/稳定性」的缺陷并修复：

1. **遗留服务端占用固定端口导致假失败**
   - 现象：上一次运行遗留的 `domo-server` 仍占用脚本默认端口（如 8799）。新运行启动的进程绑定
     失败后立即退出，而健康检查仍从遗留实例拿到 200，于是后续请求被写进**旧实例的 DB / 旧 RUN_DIR**，
     断言在本次运行目录里找不到产物（如 `projects/<prj>/summary-report.md`），表现为莫名其妙的 FAIL。
   - 修复（`tests/smoke_m3_e2e.sh`）：① 未设置 `DOMO_M3_PORT` 时自动挑选空闲端口；
     ② 显式设置但端口被占用时立即失败并提示；③ 启动后先 `kill -0` 确认本次进程存活，
     再校验应答服务使用的是本次全新库（空 `/api/projects` 且 `DOMO_DB` 已建），否则拒绝启动。
   - 结果：冒烟可在有遗留进程的环境下稳定复跑；显式端口冲突会被快速、明确地报出，而非静默复用旧实例。

2. **mock 确定性任务偶发未固定（导致负路径不确定）**
   - 现象：负路径用 mock 实例构造「计划审查连续打回」，但创建 agent 的响应偶发丢失时，
     显式 `executor_agent_id` 会变成空值，任务被动态改派给真实 agent，于是不再触发升级，
     该负路径用例随机失败。
   - 修复（`tests/smoke_m3_e2e.sh`）：创建后按名字回查 mock agent id；若任务未 `agent_pinned`
     则 PATCH 重新固定，并新增断言确认确定性 mock 已就绪、任务已固定。
   - 结果：C1「计划审查连续打回 → escalated」负路径稳定可复现。

3. **规划任务的协商状态断言与设计不一致（会误判 A 组）**
   - 现象：脚本曾断言 `/plan` 后任务 `negotiation_status=agreed`；但 DESIGN §7.1 与实现均为
     「需验收任务默认 `pending`，由 executor↔acceptor 协商一致后才置 `agreed`」，该断言会确定性 FAIL。
   - 修复：断言改为 `negotiation_status=pending`，与设计/实现一致；「协商失败转决策者」仍由
     C 组负路径覆盖。
   - 结果：A2/A4 与后续协商链路稳定通过。

4. **真实 agent 在并发/上游抖动下的超时过紧（会把正常长任务误判为卡死）**
   - 现象：`/plan` 客户端 420s 超时短于服务端 planner 预算，客户端放弃后重试会叠加后台运行；
     agent 300s 无输出即被 idle-watchdog 判死，进而让无关分支任务因 `failure_limit` 被 `suspended`。
   - 修复：`/plan` 最多 5 次、单次 900s（失败不落库，可安全重试）；团队池 agent 的
     `timeout_ms` 300s → 600s；依赖任务完成等待同步放宽。断言仍基于持久化状态，不引入时序竞态。
   - 结果：run3（01:23，处于并发/上游抖动环境）139/139 通过。

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
   - 现象：planner 偶尔返回的任务/里程碑结构或协商 JSON 需要重试（脚本对 planner 规划做最多 5 次尝试、
     单次 900s；协商失败走决策者裁决）；协商调用也可能因上游长时间无输出触发 600s idle 终止，随后按决策通道兜底。
   - 处理：断言均基于持久化状态而非固定轮次/时序；M3 时代曾连续七次运行检查集一致；本次当前 HEAD
     （M4.5 语义）实跑 139/139 通过。
   - 影响：单次运行耗时可能波动，但结果稳定，非阻塞。

4. **上游 planner 偶发 500 / incomplete tool call（已由规划重试兜底）**
   - 现象：2026-10-19 23:13 的一次并行验证中，planner 的 opencode 调用连续返回
     `Streaming response failed: [400] Generation stopped with an incomplete tool call.`（接口侧 500），
     三次规划尝试均未产出，脚本按设计记为规划失败（该次运行未进入主链，未写入有效归档）。
   - 处理：脚本对规划最多重试 5 次（单次 900s）；重试窗口内上游恢复即可继续，后续同一脚本实跑 139/139 通过。
     上游拥塞导致 flash 长时间无输出时，可用 `DOMO_M3_MODEL=opencode-go/glm-5.3`（同为 glm 系、全量模型）
     复跑；run3 即由该模型 139/139 通过。
   - 影响：属于上游模型/网络波动，非产品缺陷；失败时现场（SQLite / `domo.log`）完整保留可复查。
