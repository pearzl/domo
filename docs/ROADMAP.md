# DOMO 渐进式实施路线图

原则：每个里程碑（M 系列阶段）结束时产出**可实际使用的版本**，用户直接在真实使用中迭代。
每个阶段都保持：可启动、可观察、可恢复。

---

## M0 — 设计定稿（已完成）

README、DESIGN.md、ROADMAP.md、API.md。

## M1 — 垂直切片 Demo（✅ 已交付）

**实现位置**：`server/`（Rust 后端，crate `domo-server`）、`web/`（React 前端）。构建与端到端冒烟均已通过。

**已验证的行为**：DAG 依赖调度与并行派发；执行失败 3 次→挂起+收件箱（不阻塞其他分支）；验收退回 2 轮→升级；收件箱四种裁决（resume/reassign/relax/abandon + 下游 replan）；暂停冻结自动流程（含验收派发）、人工裁决不冻结；取消归档（在途任务转 cancelled、agent 释放、数据保留只读）；同实例不可既执行又验收；进程 kill -9 后重启自动恢复派发；全部任务终态→项目 completed；SSE 实时推送 + 事件审计（append-only）。

**交付时修复的问题**：首次启动迁移顺序 panic（启动恢复语句先于建表）；静态资源路径改为 cwd/server-crate/可执行路径三级解析（任意目录启动均可托管 SPA）。

**目标**：跑通「建项 → 规划 → 执行 → 验收 → 自动调度 → 里程碑完成」的最小闭环，全部用 Mock Agent，但**状态机、调度器、事件审计、管控操作都是真实实现**（后续替换 agent 适配器即可，不再动核心）。

### 范围
- 后端：Rust (Axum) + SQLite
  - 数据模型与事件溯源（DESIGN.md §4/§12）
  - 任务状态机 + 依赖调度器（§5/§8，含并行派发、失败 3 次挂起、退回 2 轮升级、无依赖分支不受阻塞）
  - Mock Agent 适配器：可配置执行时长 / 成功失败，流式日志；Mock 验收者自动验收（可配置拒绝以测试退回/升级）
  - REST API + SSE 实时推送（API.md）
  - 暂停/恢复/取消（归档）；决策收件箱
- 前端：React + Vite
  - 项目总览、任务看板、任务详情（流式日志 + 事件时间线 + 验收操作 + 指导输入）、组织视图（agent 队列/负载）、决策收件箱、事件审计页
- Mock 之外的真实能力：无（刻意保守，保证 Demo 稳定可用）

### 验收标准（用户直接可用）
1. 创建项目 → 添加带依赖的任务 → 确认规划 → 自动调度执行，看板与日志实时更新
2. 任务验收 → 通过流转 / 退回重做 → 退回 2 轮升级进收件箱 → 人裁决后继续
3. 执行失败 3 次 → 挂起进收件箱，其他分支不受影响
4. 暂停后跑完当前步骤冻结；恢复续跑；取消归档只读
5. 全程事件可在审计页回放
6. 进程重启后数据完整，中断任务标记待恢复

## M2 — 真实 Agent 接入（✅ 已交付）

**已交付并实测**：
- OpenCode 适配器（真实执行/验收、stdout 流式日志、超时 kill、cwd=项目工作区、`--auto` 权限放行；免费层拒绝自定义 session id 已做运行时探测回退，真实会话 id 捕获复用，审计事件带 phase+session_id）
- planner 自动规划（opencode 驱动、JSON 容错解析、依赖成环回滚、executor/acceptor 自动分配、无角色实例明确报错、会话复用保留规划记忆）
- 工作区落地（local 路径校验 / git confirm 时 clone 到 `data/workspaces/<prj_id>`）、前端工作区表单/徽章/「生成规划」交互
- 用真实 opencode（glm-5.3-flash-free）完成端到端：执行→ops 产出文件→opencode 验收者查工作区→VERDICT pass；拒绝→退回两次→escalated；负面用例（缺角色池返回 400 且零落库）通过

**M2 遗留（并入 M2.5/M3）**：
- 决策者角色 LLM 落地（变更复核、豁免裁定、协商失败裁决）——角色标签与隔离规则已实现，自动裁决 M2 阶段暂由人工收件箱承担
- 验收标准协商（执行者↔acceptor 多轮一致；当前由 planner 产出标准并视为 agreed）

## M2.5 — 使用打磨轮 3（✅ 已交付）

- **人工介入通道**：任务视图「🧭 人工介入」→ 用户说明 → planner（与规划共用会话 memory）分析并输出方案（新增/修改/取消任务）→ 用户多轮探讨 → 确认后逐项应用（依赖按标题解析含同批新增，环校验，单项失败跳过；全程事件留痕）。`POST /projects/:id/intervene` + `/intervene/apply`
- **服务端运行日志**：stdout + `data/domo.log` 双写（DOMO_LOG 覆盖路径、DOMO_LOG_LEVEL 调级别，默认 info）；记录 API 错误、任务状态流转、opencode spawn/退出、planner 解析失败原始输出——报错不再"消失"，可凭日志排障
- **Planner 输出解析加固**：围栏块→整串→首尾区间→全部 {×} 候选取最长有效 JSON；提示词强制输出纯净单 JSON；错误带 600 字符尾部、日志留 2000 字符全文
- **规划进度跨视图保持**：生成规划的长等待状态移入全局 store，切页再切回不丢
- 冒烟：介入端点负路径 5 例、解析单元测试 5 例全绿

## M2.5 — 使用打磨轮 4（✅ 已交付）

- **Agent 管理**：`PATCH/DELETE /api/agents/:id` + 组织视图 ✎ 编辑弹窗 / 🗑 删除确认；删除被非终态任务引用的 agent → 409 并列出任务清单
- **收件箱上下文强化**：open 升级项附 `context`（任务全貌 + `last_error` + 执行者/验收者摘要 + 执行端/验收端处理指引，读时计算兼容历史数据）；挂起/升级事件创建时即嵌入；「查看任务详情」一键跳转
- **裁决选项补全**：新增 `reassign_acceptor`（更换验收者，限验收退回升级态，成果保留、自动重派验收）；`reassign` 无空闲执行者改为 409 不再假成功，且支持指定 agent；UI 按 role 过滤候选（隔离执行者/决策者）
- **任务角色替换追加**：`PATCH /api/tasks/:id` 允许任意非终态换 executor/acceptor（在途会话先终止、占位清理）；文本编辑仍限开工前
- **真实库修复与迁移**：清空 5 个 mock agent（2 个被遗留测试任务引用 → 先随清理取消任务并结档 3 条遗留收件箱）；修复「deepseek-v4.1」无效 model 配置；DOMO 项目两个挂起任务分别按真实根因处置（换执行者 / 超时上调至 30min）后自动重派，调度器接管验证通过

## M2.5 — 使用打磨轮 5（✅ 已交付，随 DOMO 自举 M3 迭代暴露）

- **agent 超时全局上调**：全部 opencode agent `timeout_ms=3600000`（1 小时）；新建默认与后端 `DEFAULT_TIMEOUT_MS`/`PLAN_TIMEOUT_MS` 同步上调——此前 10 分钟硬超时是「M3 端到端冒烟」类重任务 10 余次失败的直接原因
- **失败归因修正**：`last_fail_reason`/收件箱 context 补齐 `acceptance.run_failed`（此前验收端限流失败被错标为 "execution failed"，误导用户诊断）；挂起原因新增 `acceptance_failure_limit`，context 附 `last_failure_type`，处理指引按真实环节（执行失败/验收基建失败/验收退回）三分
- **指数退避**：run 级失败连击 5s→15s→45s→120s→300s 封顶，压住免费层限流下的 500ms 级重试风暴
- **悬空分配双重防护**：agent 角色撤销时悬空检查 409；调度器派发前守卫（角色齐全 + DM 隔离），即使分配已落库也无法派发
- **报告生成 planner 回退**：叙述 planner 按登记顺序逐个尝试（限流/故障自动换下一个，日志留痕），项目完成时自动报告不再因单个 agent 被限流而失败；实测：首个候选限流秒败 → 回退候选成功产出报告
- 实战验证：DOMO 项目「M3 端到端冒烟」任务 22 次失败后按根因处置（执行者换 opencode-go/deepseek-v4.1-flash + 1h 超时；验收者换 opencode-go/glm-5.3-flash）恢复重派成功

## M2.5 — 使用打磨轮 6/7（✅ 已交付，12 项体验反馈）

**轮 6 · 快修包（bug/微交互）**
- **创建项目工作区修复**：`create_project` 此前完全忽略请求中的 workspace 字段（逼用户再编辑一次）→ 创建即落库
- 删除与点项目名重复的「介入 · 任务」按钮
- 项目总览按状态过滤 chips（全部/初始化/运行中/已暂停/已完成/已归档，含计数）
- 看板列头中文化；单列任务过多自动滚动（列内 `max-height + overflow-y`）
- agent 超时输入单位改为分钟（创建/编辑，内部换算 ms）
- 全站时间统一东八区：`fmtTime` 固定 `Asia/Shanghai` 并标注 `(+08)`

**轮 7 · 视图强化**
- **DAG 为任务视图默认**；布局改为自上而下（层号→Y、里程碑泳道→纵列、同层节点横向并排、依赖边垂直贝塞尔）；节点重排为「任务名 + 缩略行」（状态点·状态·执行者·依赖/产物数）
- **执行日志按尝试轮次拆分**：按 `agent.session_started/ended` 事件时间窗切分（不动表结构，历史日志天然适配），轮次间最新在前、轮次内保持时序；抽屉里轮次下拉切换（标注行数与起始时间）
- **审计事件倒序 + 分页 + 筛选**：`GET /events` 新增 `order/limit/offset/from/to/type/type_prefix`（分页响应带 total；不传 limit 兼容旧调用方）；审计视图提供项目下拉、9 类事件前缀 chips、时间范围、每页 50 翻页
- **全局任务跳转 TaskLink**：任务详情依赖列表、成员列表当前任务/队列明细等所有任务引用处可点击 → 切项目+开抽屉+进任务视图
- **成员列表队列明细**：负载格「排队 N」可展开为具体任务列表（TaskLink），验收指派一并聚合

## M2.5 — 使用打磨轮 8（✅ 已交付，11 项体验反馈）

- **项目目标定位澄清**：brief 此前被注入全部提示词（执行/验收/协商/规划/决策/报告）→ 按用户预期改为**仅规划类提示词携带**（生成规划/人工介入/协商）；执行与验收只依据任务描述与验收标准，决策者只依据决策请求上下文；报告中仍作为信息性展示
- **DAG 节点**：卡片宽度缩 20%（224→179）；缩略行改两行常显（状态·执行者 / 依赖数·产物数）
- **项目总览过滤改复选框组合**：各状态独立勾选，默认不勾「已完成」「已归档」（替代原「显示已归档」单开关）
- **审计筛选改复选框组合**；时间列不换行（列宽调整 + nowrap）
- **审计条目关联跳转**：项目名可点（→任务视图）；payload 中的 agent 关联为 AgentLink（→组织视图高亮）；agent 删除改为**软删除**（status='deleted' 保留行，运行池/编辑/派发均不可见不可用，审计快照可回溯）
- **审计条目关联详情**：展开显示 payload + 关联任务快照（状态/尝试/失败退回计数/验收标准）+ 执行/验收轮次时间线 + 日志尾部 20 行（懒加载任务详情）
- **任务抽屉事件时间线**：类型复选筛选 + 倒序 + 分页 15
- **收件箱已处理记录**：分页 15、最新在前
- **全站翻页单页长度统一 15**
- **状态标签悬停显示中文说明**（TASK_STATUS_DESC：如 executing→"执行者 agent 正在处理本任务"），不再显示英文名

## M2.5 — 使用打磨轮 9（✅ 已交付，6 项反馈）

- **DAG 布局方向切换**：图例栏「⇄ 改为左右布局 / ⇅ 改为上下布局」按钮，TB/LR 双向布局（泳道随方向切换纵列/横带，依赖边方向自适应）
- **审计关联详情改为右侧抽屉**（与任务详情抽屉同款交互）：事件 payload + 关联任务快照 + 会话轮次时间线（每轮开始/结束与结果）+ 日志尾部 20 行
- **审计项目列修正**：`project_id='-'` 为 agent 级事件（如 agent 创建/编辑/删除），显示「agent 级」不再渲染为项目链接——此前被误显示为「已删除项目」；已删除项目显示快照名且不可点
- **审计 agent 显示真名**：`GET /api/agents?all=1` 返回含软删除实例（名称回溯），AgentLink 直接显示 agent 名而非 `#短id`
- **翻页条统一组件 Pager**：所有分页列表（审计/时间线/收件箱已处理）在列表**前后各渲染一份**
- **数据库清理**：mock 时代测试项目 7 个 + 空壳误建项目 1 个全量清除（tasks/milestones/inbox/events/logs/guidance/reports + 落盘产物目录）；保留 domo 早期迭代归档与刚完成的 DOMO M3 全程记录

## M2.5 — 使用打磨轮 10（✅ 已交付）

- **规划台移入任务视图**：初始化项目在任务视图顶部呈现「规划台」——项目目标文本框（可编辑保存，作为初始化提示词与规划者对话协商）+ 执行者/验收者下拉（可指定生成的全部任务的 agent，缺省自动轮转）+ 生成规划（长等待全局状态）+ 确认规划；总览卡片改为「去规划台 →」入口（保留规划中 spinner）
- **plan 端点支持指定 agent**：body 新增 `executor_agent_id` / `acceptor_agent_id`（提前校验在派 planner **之前**：角色齐全、非决策者、执行≠验收；校验失败秒回 400 不浪费 LLM 调用）
- **session 事件审计富化**：`agent.session_started` 增 `purpose`（做什么启动：执行/验收/规划/介入/协商/裁决 + 任务名与轮次）与 `trigger`（scheduled/recovery/human）；`agent.session_ended` 增 `detail`（成功/失败原因/验收通过或退回理由/裁定结果）与 `output`（成功时的产物列表）；审计抽屉会话时间线同步展示

## M2.5 — 使用打磨轮 11（✅ 已交付）

- **生成规划可指定规划者**：规划台新增「规划者」下拉（opencode+planner 实例，缺省自动选择）；plan 端点 `planner_agent_id`（提前校验）
- **项目 agent 池**：项目级 `agent_pool`（JSON 数组，None=全部已配置 agent）——初始化阶段在规划台用复选框显式选择本项目的分工范围；约束范围=**新分配**（生成规划/人工介入/建任务的 execs/accs 池过滤 + 显式指定校验），已有任务指派不受影响；规划台三个角色下拉均只列池内实例
- **DAG 节点缩略行再收敛**：状态·依赖·产物单行常显（执行者名移至 hover）；TB 泳道标签可换行不截断（hover 显示全名）
- **修复潜伏 bug**：patch_project 不携带 workspace 字段时会把已设置的工作区清空 → 改为字段级动态 UPDATE（workspace/agent_pool 仅显式携带时更新）
- 说明：任务规划中的「阶段一/二/三」为 planner LLM 自主产出的**里程碑**分组（DAG 泳道即里程碑，gate 需人工确认），系统不强制阶段划分

## M2.5 — 使用打磨轮 12（✅ 已交付）

- **统一术语：阶段 → 里程碑**：plan_prompt 的示例名从「阶段一」改为「里程碑一 · <内容主题>」，并显式约束 LLM「里程碑命名以『里程碑』开头，不要用『阶段』等其它词」——此前 DAG 泳道出现『阶段』系 LLM 模仿提示词示例；总结报告叙述同步用『里程碑目标』
- **DAG hover 去冗余**：节点悬停只显示卡片上没有的内容（执行者、任务 id、描述摘要 120 字），不再重复标题/状态/依赖/产物

## M2.5 — 使用打磨轮 13（✅ 已交付，状态机调整）

- **确认规划 ≠ 立即运行**：确认后若仍有任务处于验收标准协商 → 项目保持 initializing，任务视图显示「验收标准协商中 · N/M 已完成」进度（待协商任务为可点链接）；调度器只派协商 worker，全部一致后自动转 running
- **协商过程可见**：任务卡新增「💬 协商中 · 第 N 轮」/「⚠ 协商失败（待裁决）」徽标；执行日志轮次选择器纳入协商/规划/决策会话（此前协商日志落在「其他/未归类」）；任务详情的「协商过程」区块（每轮提案/意见/定稿）此前已具备
- **项目暂停拆两态**：pausing（暂停中：冻结派发，等在途会话结束）→ 自动 paused；暂停中可「取消暂停（恢复运行）」；总览过滤与徽标、悬停说明同步

## M4 · 流程重构 + 并发隔离 + 工作区自动化（✅ 已交付）

**A 快修**
- 强制暂停：`pause {force:true}` 终止全部在途会话并立即 paused；UI「⚡ 强制暂停」+ 确认弹窗
- 默认工作区：无需手动指定——确认时自动 `~/.domo/workspace/<project_id>/`（DOMO_WORKSPACE_ROOT 可覆盖）；显式 local/git 仍优先

**B 任务生命周期重构**
- 新状态机：`ready → plan_drafting（执行者制定计划）→ plan_review（验收者审查）→ executing（按计划执行）→ awaiting_acceptance（按计划+产物验收）→ done`；计划审查打回→重拟（≥2 轮升级）、验收打回→**仅重新执行**（≥2 轮升级）
- 验收标准协商机制退场（协商模块保留供历史回放）；验收标准由规划者给出并随确认锁定
- 规划确认迭代：规划台展示任务/里程碑/目标详情，可填修改意见 → 规划者重新规划并**替换**（单事务）→ 循环直到确认通过
- agent 发起规划修改：执行/验收输出 `REPLAN:` 块 → replan 通道（决策者裁决，战略事项转人工）
- 任务计划持久化（`execution_plan`）+ 详情页展示；计划/执行/验收提示词分工明确

**C 并发隔离（沙盒）**
- worktree 一律建在 DOMO 管辖目录：`~/.domo/workspace/<prj>/wt/<tsk>/<repo>/`（分支 `domo/task-<tsk>`）；源仓库只在 merge 时被写入
- 多仓：confirm 扫描登记 `project.repos`；任务 `scope_repos`（planner 可声明，缺省主仓）；处理方式与单仓一致
- 非编程：`<prj>/tasks/<tsk>/` 独立子目录
- 验收通过 → 系统 merge（--no-ff）逐仓合入；冲突 → 决策者先裁（rebase/rework/manual）→ 失败转人工
- 违规检测：scope 外源仓库脏文件基线比对 → `sandbox.violation` 事件 + 收件箱
- 冒烟：3 任务并行全链路（计划→审查→执行→验收→done→项目完成）、计划打回修订、连续打回升级，全通过

**D 历史数据**
- 全库备份 `data/archive/domo-<ts>.db` 与产物目录归档；旧流程数据清空（agents 保留）；新流程从干净库开始



## M3 — 里程碑与重规划

- 里程碑模型 + gate（收件箱确认 + 里程碑窗口追加任务/调整规划）
  - **gate 两条裁决路径与里程碑窗口（✅ 已交付）**：确认 → 里程碑 `passed`、下游依赖分支自动解挂续跑并跑完；
    退回 → 里程碑 `failed` + 重开「里程碑窗口」（收件箱专属操作区）。窗口内可追加任务、调整后续规划依赖/字段/取消，
    逐项应用：新增任务默认 `acceptance_required=true`（未给标准自动补兜底标准）、依赖按任务标题解析（含同批新增）、
    新增/修改依赖做 DAG 环校验（成环项拒绝并汇报原因，其余项继续应用）；窗口成员全部完成后里程碑重入 `awaiting_confirm`，
    再次确认后放行下游。全部操作落事件审计（`milestone.window_opened`/`window_applied`/`window_resolved`、
    `task.created|updated` source=`milestone_window`）可在审计页完整回放。端到端冒烟 `scripts/smoke_milestone_window.sh`（61 项检查）。
- **验收标准协商（executor↔acceptor 多轮）（✅ 已交付）**
  - 任务 `planned`（PLANNED）阶段由 executor 与 acceptor agent 携带任务上下文与历史轮次（含上轮 acceptor 意见）
    多轮协商验收标准；一致 → 写 `acceptance_criteria` + `negotiation_status=agreed`，`planned_gate`
    放行进入 `ready`；**协商未达成前调度器不派发执行**（planned→ready 仅由 `criteria_agreed_auto` 放行）。
  - 达 `MAX_NEGOTIATION_ROUNDS=3` 仍不一致 → `negotiation_status=failed`，自动登记 `negotiation_failed`
    决策请求交决策者裁决（决策者缺失/故障 → `decision_maker_down`，任务挂起但不阻塞无关分支）；裁定写入
    最终标准并恢复流转（事件 `negotiation.settled`）。
  - 每轮写 `negotiation.round` 事件（role/proposal/agree/comment/criteria），任务详情「协商过程」区完整回放；
    `negotiation_round` 与协商状态经 SQLite 持久化，`kill -9` 重启后不丢失、从事件流重建历史并续跑；
    opencode 适配器为 executor/acceptor 各自复用协商会话（memory 连续）。
  - 端到端冒烟 `scripts/smoke_negotiation.sh`（68 项检查：一致放行 / 多轮往复 / 未达成不派发 / 对抗失败→裁决恢复 /
    重启状态不丢）。
- 动态重规划：决策者常规纠偏通道 + DAG 环校验（人→planner 探讨通道已由 M2.5 介入通道先行落地）
  - **决策者常规纠偏通道（✅ 已交付）**：executor/acceptor 上报规划问题（`POST /api/tasks/:id/replan`）
    → `replan.proposed` + kind=`replan` 决策请求 → 决策者复核；战术事项自决、战略事项（任务挂里程碑或
    请求触及里程碑/项目目标）落人工收件箱等裁决；批准写 `replan.approved` 并复用规划会话产出调整，
    新增任务默认带验收、插入 DAG 做环校验后进入调度；拒绝写 `replan.rejected`、通知上报者且原流程不动；
    批准后重启可从 `planning=pending` 续跑；暂停冻结自动规划派发、恢复后续跑。新增任务若只声明要求未给验收标准，
    落地时自动补一条兜底标准；批准后的规划产出若触及里程碑/项目目标，自动升级人工收件箱（不静默改里程碑）。
    端到端冒烟 `scripts/smoke_replan.sh`（78 项检查）。
- **前端三视图增强（✅ 已交付）**
  - 任务视图 DAG 图：节点=任务（按状态着色、按里程碑泳道分组/标注），边=`depends_on`（选中节点时高亮其上下游依赖边）；点击节点打开任务详情抽屉；SSE `task` 事件驱动节点状态与颜色实时刷新。看板 / DAG 呈现方式与选中任务提升到全局 store，切页再切回不丢。
  - 项目总览：每个里程碑展示状态徽章 + 进度条（已完成/成员任务数）；`awaiting_confirm` 的里程碑给出「待确认 · 去收件箱 →」入口，一键跳转并高亮定位对应的 `milestone_gate` 收件箱条目（离开收件箱后高亮自动清除）。
  - 组织视图负载：每个 agent 展示当前任务、队列长度（指派给该 agent 且 `planned/ready` 的任务数）、`max_parallel` 占用比与空闲余量，另有团队级角色分布。
  - 端到端数据契约冒烟 `scripts/smoke_web_views.sh`：DAG 节点/边与 `depends_on` 完全一致（无悬空依赖、里程碑分组可解析）、gate 跳转目标存在、3 路并行派发时各 agent 占用/空闲与队列正确、SSE `task/agent/milestone/inbox` 四类事件、静态托管 bundle 含三处增强标记。
- 项目总结报告
  - **项目总结报告（✅ 已交付）**：项目进入 `COMPLETED` 后调度器自动触发（幂等），planner 角色 agent 汇总
    事件审计、里程碑通过/退回、任务完成/退回/失败/挂起统计、验收记录与产物清单，生成含**里程碑汇报段落**的
    Markdown 总结报告，落库并写入 `<data>/projects/<prj_id>/summary-report.md`（另有 `summary-report.json`
    事实快照）；`GET/POST /api/projects/:id/report` 提供读取与手工触发，前端总览提供「查看总结报告」弹窗
    渲染入口（SSE `report` 实时刷新）。planner 失败 → 报告置 `failed` 且 `tracing::error!` 写入 `data/domo.log`，
    可手工重试；无 planner 时回退确定性叙述不阻断。端到端冒烟 `scripts/smoke_report.sh`（49 项检查）。
- **M3 端到端冒烟（✅ 已交付）**
  - `scripts/smoke_m3_e2e.sh`：用**真实 opencode**（glm 系 agent）组 8 实例团队池（planner/executor/acceptor/decision_maker 各 2），
    跑通完整正向链路——planner 产出含里程碑的规划 → 人确认 → executor↔acceptor 协商验收标准 → 执行与验收 →
    里程碑 gate 确认与退回窗口追加任务 → 决策者纠偏重规划 → 全部终态自动生成总结报告；并覆盖负路径——
    协商失败转决策者、成环依赖被拒、决策者缺席转收件箱、gate 只挂起下游分支（无关分支继续跑）、
    未确认 gate 阻断项目完成（全部任务终态但里程碑 `awaiting_confirm` 时项目不提前 `completed`/不提前生成报告，确认后才放行）。
  - 含 `kill -9` 重启恢复：里程碑/gate/协商状态/决策/报告经 SQLite 完整保留，中断流程从下一轮续跑至 `completed`。
  - 结果与关键日志归档 `data/smoke_m3_e2e/{summary.md,results.md,domo.log}`（对应运行目录另有 SQLite / 工作区 /
    `summary-report.md`），可复查、可重复执行；共 134 项检查（phase1 90 / phase2 44），连续五次运行检查集一致
    （`data/smoke_m3_e2e/STABILITY.md`）；用例清单见 `docs/SMOKE_M3_E2E.md`。

## M4 — 恢复与审计完善

- **三档恢复降级真实落地（依赖 OpenCode resume）（✅ 已交付）**
  - **真实会话 id 跨重启持久化**：执行派发从 CLI 输出解析到的 opencode 真实会话 id 立即落
    SQLite `session_map(logical, real, task_id, phase, attempt)`（不再只存 `state.sessions` 内存），
    `kill -9` 重启后事件/DB 记录与崩溃前一致。
  - **三档依序降级（DESIGN §10）**：启动期把「中断的 executing 任务」（当前 attempt 无失败记录）与
    「第三档人工挂起任务」标记为待恢复（`tasks.recovery_pending/tier/manual`），调度器按序推进——
    ① `native_resume`：`supports_resume==true` **且**该 attempt 真实 id 已持久化 →
    `opencode run --session <同一真实id>` 续作（**attempt 不自增、逻辑会话名不变**）；
    ② `handoff`：跳过/失败 → 结构化交接书（目标/已完成/剩余/约束/产物路径，事件 payload + 落盘
    `<data>/projects/<prj>/handoffs/`）开**新会话**续作（新 id ≠ 原 id）；
    ③ `manual`：两档均失败 → `suspended` + 收件箱条目（payload 同含两档失败原因，按任务去重不堆叠），
    下次重启重新进一轮三档（每轮重启至多一轮），成功即脱离挂起并自动关闭原收件箱条目（`auto_recovered`）。
  - **语义边界**：恢复档失败不计入「失败 3 次挂起」额度（按档位降级，不误伤挂起语义）；执行失败后的
    重试仍走原 attempt+1 全新派发（`trigger=retry`）；项目 `paused/pausing` 期间中断任务不自动恢复，
    resume 后按三档续跑；同一 task 在任何恢复路径下恒只有一个在途执行 worker/句柄（任务详情新增
    `live_handles` 与 `recovery{pending,tier,manual}` 字段）。
  - **审计契约**：`task.recovered` 事件固定携带 `tier∈{native_resume,handoff,manual}`、
    `outcome∈{succeeded,failed,skipped,suspended}`、`reason`、`task_id`、`attempt`、`session_id`
    （交接轮另附交接书全文与落盘路径，人工档附 `tier1_reason/tier2_reason`）；恢复流程每档各落一条
    （成功/失败/跳过均含），append-only，任务详情时间线与审计检索（`type=task.recovered`）可见；
    `agent.session_started/ended` 同步补 `real_session_id`（真实 id）与 `recovery_tier`。
  - 端到端冒烟 `scripts/smoke_recovery_tiers.sh`：真实 opencode + mock 混编团队池，覆盖
    原生恢复（口径 1/3/4）、交接重建（口径 2/5）、mock 跳过第一档（口径 6）、第一档强制失败降级（口径 7）、
    两档失败挂起 + 无关分支不受阻（口径 9）、SUSPENDED 重启再恢复 + 收件箱闭环（口径 10）、
    暂停语义不冲突（口径 11）、无重复并发 worker 采样（口径 12）、事件契约与审计可见（口径 8/13）；
    结果与关键日志归档 `data/smoke_recovery_tiers/{summary.md,results.md,domo.log}`，
    用例清单见 [docs/SMOKE_RECOVERY_TIERS.md](docs/SMOKE_RECOVERY_TIERS.md)。
- 审计详情：任务级时间线回放、事件过滤检索；**关联详情按事件类型逐族设计**（执行失败/验收退回/决策/报告/agent 生命周期各自的关联信息视图，当前通用形态为过渡方案，见 DESIGN §14）
- 稳定性：会话超时、僵尸会话清理、压力测试

## M5+ — 见 DESIGN.md §13（知识库、模板、更多适配器）
