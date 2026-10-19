# M4 三档恢复冒烟 · 用例清单（smoke_recovery_tiers）

脚本：[`tests/smoke_recovery_tiers.sh`](../tests/smoke_recovery_tiers.sh)（退出码 0 = 全部通过）
归档：`data/smoke_recovery_tiers/{summary.md,results.md,domo.log}`（另按 runN 逐次留档）
模型：**必填** `DOMO_RT_MODEL=<provider>/<model>`（如 `opencode-go/glm-5.3-flash`）；端口 `DOMO_RT_PORT`（默认 8797）。

```bash
DOMO_RT_MODEL=opencode-go/glm-5.3-flash bash tests/smoke_recovery_tiers.sh
```

冒烟用**真实 opencode**（原生 resume / 交接重建）+ **mock 适配器**（跳过第一档路径）混编团队池，
每个用例都以「任务执行中 `kill -9` 整个服务进程组（含 opencode CLI 子进程）→ 重启」构造中断现场。

> **与 M4.5 任务生命周期的衔接**：任务先走「执行者制定计划 → 验收者审查 → 执行」，计划阶段占用第 1 轮
> attempt，执行轮通常为 attempt=2。因此本冒烟的 attempt 断言一律**相对中断时刻**（恢复后 attempt ==
> 中断前 attempt、逻辑会话名 `{task}_a{中断attempt}` 不变），而非绝对值；执行派发的固定 agent 通过
> 创建任务时显式指定 executor（`agent_pinned`）锁定，动态选人不覆盖。

## 覆盖用例 ↔ 验收口径

| 用例 | 场景 | 覆盖验收口径 |
|---|---|---|
| 0 | 团队池：`oc-exec`(opencode, supports_resume=true) / `oc-exec-nr`(opencode, false) / `mock-exec`(mock, 30s) / `mock-acc`(mock)；两个 running 项目 | — |
| 4 | 真实 opencode 中断 → 重启 → **tier=native_resume 原生恢复** → 跑完 | 口径 1（基线差异对照）、3（session id 跨重启持久化）、4（原生 resume 端到端）、12 |
| 5 | `supports_resume=false` 的真实 agent（固定指派）→ 第一档 skipped → **第二档交接摘要重建**（新会话 + 交接书）→ 跑完 | 口径 2、5、12 |
| 6 | mock 适配器（supports_resume=false）→ **跳过①直接②**，reason 注明 supports_resume=false | 口径 6、8、12 |
| 7 | 持久化真实 session id 置为**无效值** → 第一档 failed → 自动降级② → 跑完 | 口径 7、2、12 |
| 9 | 两档均失败（工作区失效）→ 任务 **suspended** + 收件箱（payload 同含两档失败原因）；无关分支任务不受阻塞 | 口径 9、12 |
| 10 | tier=manual 挂起任务 **kill -9 重启后再进一轮三档** → 成功脱离 suspended、原收件箱条目关闭（不堆叠） | 口径 10 |
| 11 | 项目 **paused** 期间中断任务不自动恢复（无新派发 / 无新 `task.recovered` / 无新日志），resume 后按三档续跑 | 口径 11 |
| 全局 | `task.recovered` 事件契约、append-only、时间线与审计检索可见；日志/无 panic；采样汇总 | 口径 8、13 |

## 检查项明细

### 0) 团队池与项目
- 4 个 agent 注册成功（opencode×2 + mock×2）；`oc-exec` supports_resume=true、`oc-exec-nr`=false
- 两个项目创建并 `running`（工作区指向冒烟临时目录）

### 用例 4 — 原生会话恢复（真实 opencode）
1. 任务创建并进入执行；**中断前 attempt ≥ 1（记录为 attempt_pre，恢复全程不自增）**
2. 崩溃前已解析到真实 opencode 会话 id 并落 SQLite（`session_map`：`{task}_a{attempt_pre} → ses_…`）
3. 重启后任务自动恢复并跑完（`awaiting_acceptance`/`done`）
4. 落 `task.recovered` `tier=native_resume` `outcome=succeeded`；契约字段齐全
5. 恢复事件 `session_id` == 崩溃前真实 id；**恢复后 attempt == 中断前 attempt**
6. 恢复派发逻辑会话名不变（`exec_session(task_id, attempt)` = `{task}_a{attempt_pre}`，trigger=recovery）
7. 恢复派发日志出现 `--session <崩溃前同一真实id>`
8. 重启后 `session_map` 行与崩溃前完全一致（事件/DB 双证）
9. 仅第一档：无 handoff / manual 事件（逐档判定）
10. **对照断言**：改造前行为痕迹不存在——无 `{task}_a{attempt_pre+1}` 全新会话重派、attempt 未自增
11. 采样：`live_handles` 恒 ≤1、DB 占用表恒 ≤1、期间确有在途 worker

> **基线差异对照（口径 1）**：改造前对无活句柄的 `executing` 任务走 `dispatch_exec(…, "recovery")`
> —— attempt 自增 → 逻辑会话名变成 `{task}_a{pre+1}` → 全新会话重派；本用例以
> 「attempt 不自增 + 会话名 `{task}_a{pre}` 不变 + `--session <崩溃前真实id>`」作为第一档落地证据，
> 改造前该组断言必失败（`data/smoke_recovery_tiers/` 归档可复查）。

### 用例 5 — 交接摘要重建（真实 opencode，跳过第一档）
1. 执行者固定为 `oc-exec-nr`（supports_resume=false 且 `agent_pinned`，动态选人不覆盖）
2. 中断前解析到真实会话 id、重启后跑完
3. 第一档 `outcome=skipped` 且 reason 注明 `supports_resume=false`
4. 第二档 `handoff` `outcome=succeeded`
5. 交接书**五节齐全非空**：目标 / 已完成 / 剩余 / 约束 / 产物路径（事件 payload 内）
6. 交接书 Markdown 含五节标题；落盘且事件记录路径（文件存在可复查）
7. 第二档 `session_id` ≠ 原 session id（事件）且执行日志出现新会话 id（日志双证）
8. attempt 未自增；交接会话逻辑名 = `{task}_a{attempt_pre}_h1`
9. 采样：句柄/占用恒 ≤1

### 用例 6 — mock 路径（跳过①直接②）
1. 执行者固定为 mock-exec（supports_resume=false）
2. 中断后重启 → `tier=native_resume outcome=skipped`，reason 含 `supports_resume=false`
3. `tier=handoff outcome=succeeded`，且 handoff reason 汇报第一档 skip 原因
4. attempt 未自增；事件契约字段齐全；采样断言

### 用例 7 — 第一档强制失败降级
1. 崩溃后把 `session_map` 中该 attempt 的真实 id 改为无效值（不以 `ses` 开头）
2. 重启 → `tier=native_resume outcome=failed` 且含失败原因（CLI 报错 `Expected a string starting with "ses"`）
3. 自动降级②：`handoff outcome=succeeded` 且新 session id ≠ 崩溃前 id
4. attempt 未自增；**恢复档失败不计入「失败 3 次挂起」额度**（failure_count 不变）
5. 采样断言；稳定复现（无时间窗依赖）

### 用例 9 — 两档均失败 → 人工重建
1. 崩溃后把项目工作区指向不存在路径（两档都会失败：resume 启动失败 + 交接会话派发失败）
2. 重启 → 第一档 `failed` → 第二档 `failed` → `tier=manual outcome=suspended`
3. manual 事件 reason 逐档列明第一档与第二档失败原因
4. 收件箱新增 `escalation` 条目（`reason=recovery_exhausted`），payload 同时含 `tier1_reason` / `tier2_reason`
5. attempt 未自增、failure_count 未被恢复失败抬高
6. **无关分支任务不被阻塞**（同项目 mock 任务在挂起期间跑完）
7. 采样：主任务句柄恒 ≤1（秒失败场景）；无关分支在途 worker 采样断言

### 用例 10 — SUSPENDED 重启再恢复
1. 恢复工作区后再 kill -9 → 挂起任务重新进一轮三档
2. `tier=native_resume outcome=succeeded`（用崩溃前同一真实 id 续作）→ 脱离 suspended 跑完
3. 原收件箱条目 `resolved`（`resolution=auto_recovered`），无重复堆叠（open 条目为 0）
4. attempt 全程未自增；采样断言

### 用例 11 — 暂停语义不冲突
1. 任务执行中暂停项目（在途会话跑完当前尝试即停：`pausing`→`paused`）→ `kill -9` → 重启（项目已 paused）
2. 无新派发：任务仍 `executing`；**无新 `task.recovered` 事件**；任务日志无新增
3. `recovery.pending=true` 保留（标记不丢，只冻结派发）；项目状态仍 `paused`
4. `resume` 后按三档流程恢复（native skipped → handoff succeeded）并跑完；暂停/恢复语义回归不变

### 全局 — 事件契约与审计（口径 8 / 13）
1. 全部 `task.recovered` 事件契约字段齐全且取值在枚举内
   （`tier∈{native_resume,handoff,manual}`、`outcome∈{succeeded,failed,skipped,suspended}`、
   `reason`、`task_id`、`attempt`、`session_id`）
2. 三档事件均出现过；outcome 覆盖四态（成功/失败/跳过/挂起各档各落一条）
3. append-only：事件 id 无重复且各项目时间线严格递增
4. 任务详情时间线包含 `task.recovered`；审计按 `type=task.recovered` 可检索
5. 服务端运行日志含 opencode spawn/finished、无 panic
6. 全部恢复路径采样汇总：无双在途 worker/句柄；DB 占用表无重复条目

检查项计数由脚本运行时打印（`N/M checks passed`），结果与关键日志归档至
`data/smoke_recovery_tiers/{summary.md,results.md,domo.log}`。
