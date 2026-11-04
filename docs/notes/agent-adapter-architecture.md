# Agent 适配层架构

> 说明 DOMO 服务端 Agent 适配层的当前契约、注册方式与接入步骤。适配层位于 `server/src/agents/`，只负责把平台需要的业务能力映射到具体 Agent 类型；作业生命周期、调度、活动流和对外接口由其他模块承担。

## 1. 分层位置

| 模块 | 职责 |
|---|---|
| `server/src/agents/adapter.rs` | `AgentAdapter` 契约、上下文与结果类型、`JobUsage` 语义。 |
| `server/src/agents/mod.rs` | 适配器注册表、静态泛型分发、面向调用方的门面函数。 |
| `server/src/agents/mock.rs` | 进程内 mock Agent。 |
| `server/src/agents/opencode.rs` | OpenCode CLI Agent。 |
| `server/src/agents/codex.rs` | Codex CLI Agent。 |
| `server/src/agents/workbuddy.rs` | WorkBuddy CLI Agent。 |
| `server/src/agents/cli.rs` | 自定义命令模板 Agent。 |
| `server/src/agents/process.rs` | 外部进程启动、输出采集、超时与进程组终止。 |
| `server/src/runner/` | 作业生命周期、续轮、看门狗、结果解析与持久化。 |
| `server/src/api.rs` | HTTP 请求校验与响应组装。 |
| `server/src/activity.rs` | 作业活动流格式识别与投影。 |
| `server/src/scheduler.rs` | 作业调度与 Agent 可用性判定。 |

适配层不持有数据库连接，也不组装 HTTP 响应。调用方把 `AgentRow`、作业描述和超时传入 `server/src/agents/mod.rs` 的门面函数，拿回 `AgentRun` 或 `JobUsage`。

## 2. 四类业务能力

平台把 Agent 调用归纳为四类业务能力。每类能力对应一组 `AgentAdapter` 方法，并由 `server/src/agents/mod.rs` 的门面函数转发。

| 业务能力 | 平台需要解决的问题 | trait 方法 | 门面函数 | 调用方 |
|---|---|---|---|---|
| 配置可用性 | 该类型是否必须提供命令模板；当前配置能否启动 | `requires_command_template`、`validate` | `requires_command_template`、`validate_config` | `server/src/api.rs` 的 `require_command_if_needed`；`server/src/agents/mod.rs` 的 `run_with` |
| 作业会话 | 新作业使用哪个会话标识；续轮是否沿用已有会话 | `session_for_job` | `session_for_job` | `server/src/runner/mod.rs` 的 `run_job` |
| 执行一轮 | 用什么参数、从哪个通道投递提示词、如何采集进程结果 | `run_turn` | `execute` | `server/src/runner/agent.rs` 的 `run_agent_job` |
| 作业用量结算 | 最后一轮结束后，用量取逐轮记录还是适配器账本 | `finish_job` | `finish_job` | `server/src/runner/mod.rs` 的 `run_job` |

### 2.1 配置可用性

`requires_command_template` 回答“该类型是否必须配置命令模板”，供 API 在创建或修改 Agent 时给出固定文案的错误。`validate` 回答“当前配置能否运行”，在 API 校验和每轮执行前各调用一次。

`server/src/api.rs` 的 `validate_kind` 只查询 `agents::kinds()`；`require_command_if_needed` 先查 `agents::requires_command_template`，再调用 `agents::validate_config`。`server/src/api.rs` 不引用任何 `AGENT_*` 常量，也不按 Agent 类型写分支。

### 2.2 作业会话

`session_for_job(job_id, existing)` 返回该作业应使用的会话标识。`existing` 是上一轮运行报告的会话；返回 `None` 表示该类型不需要会话标识。`server/src/runner/mod.rs` 在作业开始时调用 `agents::session_for_job(&agent, &job.id)` 初始化 `AgentSpec.session_id`，续轮沿用上一轮 `AgentRun.session_id`。

### 2.3 执行一轮

`run_turn` 接收 `TurnContext`，完成参数展开、提示词投递、进程运行和结果采集，返回 `AgentRun`。适配器拥有本类型的参数与投递细节；`server/src/runner/agent.rs` 只负责超时、卡死检测、续轮和结果文件解析。

`TurnContext` 包含：

| 字段 | 含义 |
|---|---|
| `config` | `AgentConfig`，来自 Agent 配置行的类型、模型、命令模板与模型参数。 |
| `spec` | `AgentSpec`，该作业的标识、角色、工作目录、作业目录、提示词、原始提示词、会话标识、续轮标记和 `JobProgress`。 |
| `timeout` | 该轮运行的时间上限。 |

`AgentRun` 包含：

| 字段 | 含义 |
|---|---|
| `stdout`、`stderr` | 该轮运行的输出文本。 |
| `exit_code` | 进程退出码；进程内 mock 返回 `Some(0)`。 |
| `timed_out`、`stalled` | 超时与卡死标记。 |
| `session_id` | 本类型报告或派生的会话标识。 |
| `usage` | 该轮可解析到的用量；没有时为 `None`。 |

`AgentRun::process_failure()` 在 `timed_out`、`stalled` 或 `exit_code != Some(0)` 时返回真，供结算和失败分类使用。

### 2.4 作业用量结算

`finish_job` 在最后一轮结束后调用一次，返回 `JobUsage`。`JobUsage` 定义在 `server/src/agents/adapter.rs`：

| 取值 | 语义 |
|---|---|
| `JobUsage::PerTurn` | 保留逐轮 `AgentRun.usage`；`server/src/runner/agent.rs` 在续轮之间用 `merge_usage` 合并。 |
| `JobUsage::Authoritative(Option<UsageRecord>)` | 用适配器账本值替换逐轮记录；`None` 表示账本明确没有可用量。 |

`finish_job` 的默认实现返回 `JobUsage::PerTurn`。`OpencodeAdapter` 覆盖该默认实现：`finalize_plan(process_failed, shutting_down)` 决定 `Skip`、`FetchOnly` 或 `SettleThenFetch`，再通过 `crate::opencode::fetch_usage` 与 `crate::opencode::settle` 取得 `Authoritative` 结果。

`FinishContext` 包含 `agent`、`job_id`、`session_id`、`process_failed`、`shutting_down`。`server/src/runner/mod.rs` 用 `FinishContext` 调用 `agents::finish_job`，遇到 `Authoritative` 时整体替换 `AgentRun.usage`，包括替换为 `None`。

结算后的用量由 `server/src/runner/lifecycle.rs` 写入作业行：优先使用 `AgentRun.usage`，缺失时读取结果文件的 `usage` 字段（`UsageRecord::from_result`）。适配器不直接写数据库。

## 3. AgentAdapter 契约

`AgentAdapter` 定义在 `server/src/agents/adapter.rs`，约束为 `Default + Send + Sync + 'static`，以关联常量 `KIND` 作为注册表键。

| 方法 | 签名 | 默认实现 | 责任 |
|---|---|---|---|
| `KIND` | `const KIND: &'static str` | 无 | 注册表键，也是 API 接受的类型值。 |
| `requires_command_template` | `fn requires_command_template(config: &AgentConfig) -> bool` | 无 | 声明该类型是否必须配置命令模板。 |
| `validate` | `fn validate(config: &AgentConfig) -> Result<()>` | 无 | 拒绝无法运行的配置。 |
| `session_for_job` | `fn session_for_job(job_id: &str, existing: Option<&str>) -> Option<String>` | 无 | 派生或沿用作业会话标识。 |
| `run_turn` | `fn run_turn<'a>(&'a self, context: TurnContext<'a>) -> impl Future<Output = Result<AgentRun>> + Send + 'a` | 无 | 执行一轮并返回 `AgentRun`。 |
| `finish_job` | `fn finish_job<'a>(&'a self, context: FinishContext<'a>) -> impl Future<Output = JobUsage> + Send + 'a` | `JobUsage::PerTurn` | 结算作业用量。 |

关联类型与上下文类型均由 `server/src/agents/mod.rs` 重新导出：

| 类型 | 定义位置 | 用途 |
|---|---|---|
| `AgentAdapter` | `server/src/agents/adapter.rs` | 适配器契约。 |
| `AgentConfig` | `server/src/agents/adapter.rs` | Agent 配置快照。 |
| `AgentSpec` | `server/src/agents/adapter.rs` | 单次作业描述。 |
| `AgentRun` | `server/src/agents/adapter.rs` | 单轮运行结果。 |
| `JobProgress` | `server/src/agents/adapter.rs` | stdout/stderr 活跃度与字节计数。 |
| `TurnContext` | `server/src/agents/adapter.rs` | `run_turn` 输入。 |
| `FinishContext` | `server/src/agents/adapter.rs` | `finish_job` 输入。 |
| `JobUsage` | `server/src/agents/adapter.rs` | 用量结算语义。 |
| `UsageRecord` | `server/src/usage.rs` | token、费用与耗时明细。 |

## 4. 注册表与静态泛型分发

`server/src/agents/mod.rs` 用 `adapter_registry!` 宏声明唯一的适配器清单：

```rust
adapter_registry!(
    MockAdapter,
    OpencodeAdapter,
    CodexAdapter,
    WorkbuddyAdapter,
    CliAdapter,
);
```

清单顺序就是 `kinds()` 的返回顺序，也是 API 错误文案中类型选项的顺序。宏从同一份清单生成：

| 生成项 | 作用 |
|---|---|
| `KINDS` | 所有 `<Adapter as AgentAdapter>::KIND` 的静态切片。 |
| `dispatch_requires_command_template` | 按 `kind` 静态转发到对应适配器的 `requires_command_template`。 |
| `dispatch_validate_config` | 按 `kind` 静态转发到对应适配器的 `validate`。 |
| `dispatch_execute` | 按 `agent.kind` 静态转发到 `run_with::<Adapter>`。 |

对外门面：

| 函数 | 行为 |
|---|---|
| `kinds()` | 返回 `KINDS`，供 API 与测试枚举全部类型。 |
| `requires_command_template(kind)` | 查询注册表；未知类型返回 `false`，由调用方先做类型校验。 |
| `validate_config(kind, command)` | 查询注册表；未知类型返回 `Ok(())`，由调用方先做类型校验。 |
| `execute(agent, spec, timeout)` | 按 `agent.kind` 分发；未知类型返回“未知 Agent 类型”错误。 |
| `session_for_job(agent, job_id)` | 按 `agent.kind` 调用对应适配器的 `session_for_job`；未知类型返回 `None`。 |
| `finish_job(context)` | 按 `context.agent.kind` 调用对应适配器的 `finish_job`；未知类型返回 `JobUsage::PerTurn`。 |

`run_with::<A>` 是静态泛型分发的落点：

```rust
async fn run_with<A: AgentAdapter>(
    config: &AgentConfig,
    spec: &AgentSpec,
    timeout: Duration,
) -> Result<AgentRun> {
    A::validate(config)?;
    let adapter = A::default();
    adapter.run_turn(TurnContext { config, spec, timeout }).await
}
```

分发不依赖 trait object，也不在 `server/src/runner/mod.rs` 或 `server/src/api.rs` 中按类型写分支。`server/src/runner/mod.rs` 只调用 `agents::session_for_job`、`agents::execute` 和 `agents::finish_job`；`server/src/api.rs` 只调用 `agents::kinds`、`agents::requires_command_template` 和 `agents::validate_config`。

`KINDS`、配置校验与执行分发由宏生成；`session_for_job` 和 `finish_job` 两个门面按同一份清单逐项匹配 `KIND`，位置同在 `server/src/agents/mod.rs`。适配器需要专门会话派生或非默认结算时，在该文件的对应门面中登记分支；其余类型沿用 `None` 或 `PerTurn`。

## 5. 五类适配器差异

| 维度 | `mock` | `opencode` | `codex` | `workbuddy` | `cli` |
|---|---|---|---|---|---|
| 适配器 | `MockAdapter`（`server/src/agents/mock.rs`） | `OpencodeAdapter`（`server/src/agents/opencode.rs`） | `CodexAdapter`（`server/src/agents/codex.rs`） | `WorkbuddyAdapter`（`server/src/agents/workbuddy.rs`） | `CliAdapter`（`server/src/agents/cli.rs`） |
| 参数来源 | 进程内 `run_mock(spec)`；读取 `job_dir/context.json`；`DOMO_MOCK_DELAY_MS`、`DOMO_MOCK_EXECUTOR_DELAY_MS`、`DOMO_MOCK_USAGE` 等环境变量 | `opencode_args` 由 `AgentConfig.model` 和 `session_id` 生成；可执行文件来自 `crate::opencode::bin()` | `codex_args` 或 `codex_resume_args`；模型来自 `AgentConfig.model`；配置了命令模板时转交 `CliAdapter` | `workbuddy_args` 或 `workbuddy_resume_args`；模型来自 `AgentConfig.model`；配置了命令模板时转交 `CliAdapter` | `AgentConfig.command` 命令模板逐项展开，首项为可执行文件；`AgentConfig.model_args` 参与 `{model_args}` 展开 |
| 提示词投递 | 不读取 `AgentSpec.prompt`，以 `context.json` 为输入 | stdin | stdin；续轮由 `prompt_for_run` 决定是否拼接原始提示词 | stdin；续轮由 `prompt_for_run` 决定是否拼接原始提示词 | 模板含 `{prompt}` 时作为位置参数，否则写入 stdin；续轮同样经 `prompt_for_run` |
| 会话恢复 | `session_for_job` 返回 `None`，每作业独立 | `session_for_job` 优先沿用非空 `existing`，否则用 `crate::opencode::session_id(job_id)`；`run_turn` 以 `--session` 传入 | 优先沿用非空 `existing`，否则用 `derived_session_id(job_id)`；续轮用 `codex_resume_args` 的 `exec resume`；`parse_codex_session_id` 从 `thread.started` 读取 `thread_id` | 优先沿用非空 `existing`，否则用 `derived_session_id(job_id)`；续轮用 `workbuddy_resume_args` 的 `--resume`；`parse_workbuddy_session_id` 从事件读取 `session_id` | 优先沿用非空 `existing`，否则用 `derived_session_id(job_id)`；模板或模型参数含 `{session_id}` 时视为支持恢复，`AgentRun.session_id` 回填该值 |
| usage 来源 | `DOMO_MOCK_USAGE=1` 时把 `usage` 写入 `result.json`；`AgentRun.usage` 为 `None`，`finish_job` 用默认 `PerTurn` | `finish_job` 按 `finalize_plan` 决策：`Skip` 返回 `PerTurn`；`FetchOnly` 只取账本；`SettleThenFetch` 先 `settle` 再取账本；结果为 `Authoritative` | `parse_codex_usage` 从 `turn.completed` 的 `usage` 生成 `UsageRecord`，写入 `AgentRun.usage`；`finish_job` 用默认 `PerTurn` | `parse_workbuddy_usage` 优先取 `result` 事件的 `usage`，缺失时取最后一条 `assistant` 消息的 `usage`，并读取 `total_cost_usd`；写入 `AgentRun.usage`；`finish_job` 用默认 `PerTurn` | 适配器不产生用量；`AgentRun.usage` 为 `None`，`finish_job` 用默认 `PerTurn`；结果文件的 `usage` 由 `server/src/runner/lifecycle.rs` 读取 |
| 命令模板要求 | 否 | 否 | 否；配置非空时作为自定义模板执行 | 否；配置非空时作为自定义模板执行 | 是；`validate` 拒绝空模板，错误文案为“CLI Agent 必须配置命令模板” |
| 内置二进制覆盖 | 无，进程内运行 | `DOMO_OPENCODE_BIN`，默认 `opencode` | `DOMO_CODEX_BIN`，默认 `codex` | `DOMO_WORKBUDDY_BIN`，默认 `codebuddy` | 无环境变量覆盖；可执行文件取自命令模板首项 |

补充说明：

- `mock` 的 `AgentRun.session_id` 为 `None`，`server/src/agents/mod.rs` 的 `session_for_job` 对 `AGENT_MOCK` 返回 `None`。
- `codex` 与 `workbuddy` 的 `requires_command_template` 返回 `false`；命令模板非空时，两者的 `run_turn` 都把本轮交给 `CliAdapter`，模板为空时使用各自的内置参数。
- `cli` 的 `prompt_for_run(spec, can_resume)` 在无法恢复会话时把 `original_prompt` 与续轮提示词拼接，保证新进程先看到原始目标。
- `derived_session_id(job_id)` 从作业标识的十六进制片段派生稳定 UUID 形会话标识；`job_id` 不含足够十六进制字符时回退为 `ses_domo<job_id>`。

## 6. 接入新 Agent 类型

以下步骤覆盖“实现适配器、登记、契约测试、端到端用例”。完成登记后，`execute`、API 类型校验和注册表契约测试都会识别该类型，`server/src/runner/mod.rs` 与 `server/src/api.rs` 不需要按类型分支。

### 6.1 实现 `AgentAdapter`

1. 在 `server/src/models.rs` 按现有命名加入类型常量，例如 `pub const AGENT_EXAMPLE: &str = "example";`；`KIND` 引用该常量。
2. 在 `server/src/agents/` 下建立 `example` 模块并定义适配器：

   ```rust
   #[derive(Debug, Default)]
   pub struct ExampleAdapter;

   impl AgentAdapter for ExampleAdapter {
       const KIND: &'static str = crate::models::AGENT_EXAMPLE;

       fn requires_command_template(_config: &AgentConfig) -> bool {
           false
       }

       fn validate(_config: &AgentConfig) -> Result<()> {
           Ok(())
       }

       fn session_for_job(job_id: &str, existing: Option<&str>) -> Option<String> {
           Some(
               existing
                   .map(str::to_string)
                   .unwrap_or_else(|| derived_session_id(job_id)),
           )
       }

       async fn run_turn(&self, context: TurnContext<'_>) -> Result<AgentRun> {
           run_process(
               &bin_override("DOMO_EXAMPLE_BIN", "example"),
               &example_args(&context.config.model, context.spec),
               context.spec,
               context.timeout,
               Some(context.spec.prompt.clone().into_bytes()),
           )
           .await
       }
   }
   ```

3. 按类型的真实行为实现 `requires_command_template`、`validate`、`session_for_job`、`run_turn`。需要权威用量账本时覆盖 `finish_job` 并返回 `JobUsage::Authoritative`；否则沿用默认 `PerTurn`。
4. 复用 `server/src/agents/` 内的公共能力：`run_process` 负责进程启动、输出采集与进程组终止；`bin_override` 读取 `DOMO_<NAME>_BIN`；`derived_session_id` 派生会话标识；`prompt_for_run` 处理续轮提示词；`json_i64` 读取 JSON 数值。
5. 命令模板可选的类型可以像 `CodexAdapter`、`WorkbuddyAdapter` 一样，在 `run_turn` 检测到非空 `config.command` 时转交 `CliAdapter`。

### 6.2 在注册表登记

1. 在 `server/src/agents/mod.rs` 增加模块声明与导出：

   ```rust
   mod example;
   pub use example::ExampleAdapter;
   ```

2. 把 `ExampleAdapter` 加入 `adapter_registry!` 清单。`KINDS`、`dispatch_requires_command_template`、`dispatch_validate_config`、`dispatch_execute` 由该清单生成，`kinds()`、API 类型校验和 `execute` 随之识别新类型。

   ```rust
   adapter_registry!(
       MockAdapter,
       OpencodeAdapter,
       CodexAdapter,
       WorkbuddyAdapter,
       CliAdapter,
       ExampleAdapter,
   );
   ```

3. 若该类型的作业需要专门会话派生，在 `server/src/agents/mod.rs` 的 `session_for_job` 中按 `KIND` 登记分支；若 `finish_job` 需要非默认结算，在同一文件的 `finish_job` 中登记分支。仅使用默认 `PerTurn` 且 `session_for_job` 返回 `None` 的类型不需要这两处分支。

### 6.3 补契约测试

1. 在 `server/src/agents/mod.rs` 的 `registry_lists_each_adapter_kind_once` 中加入新适配器类型，并同步 `kinds()` 的数量断言。该测试保证每个 `KIND` 只出现一次且注册表覆盖全部适配器。
2. 在该适配器模块的 `#[cfg(test)]` 模块覆盖：参数拼装、提示词投递、会话派生与续轮、usage 解析、`validate` 与 `requires_command_template`。
3. `server/src/api.rs` 的 `validate_kind_accepts_every_registered_type`、`validate_kind_rejects_unknown_type_and_lists_options`、`only_the_cli_adapter_requires_a_command_template` 从 `agents::kinds()` 取值，登记后自动覆盖新类型；只有命令模板要求与错误文案相关断言需要按新类型调整。
4. 运行 `cargo test`（工作目录 `server/`）与 `cargo clippy --all-targets -- -D warnings`。

### 6.4 补端到端用例

1. 在 `tests/test_agent_adapters.py` 的假 CLI 模式上加入新类型：用 `write_fake_cli` 生成假可执行文件，通过 `DOMO_<NAME>_BIN` 注入，假 CLI 从 stdin 读取提示词并向 `DOMO_JOB_DIR/result.json` 写入结果。
2. 断言作业成功、产物进入交付分支、`usage_known` 与假 CLI 输出一致、`/api/jobs/{id}/activity` 返回预期活动流，以及未知类型在进程启动前被 API 拒绝。
3. 需要真实 CLI 协议核对时，按 `docs/notes/agent-cli-protocols.md` 记录事件格式，并把解析函数放在对应适配器模块内。
4. 运行 `python3 tests/test_agent_adapters.py`；涉及会话复用的类型同时运行 `python3 tests/test_session_reuse.py`。

### 6.5 接入检查表

| 检查项 | 位置 |
|---|---|
| `KIND` 常量 | `server/src/models.rs`、适配器模块 |
| 四类能力方法 | 适配器模块的 `impl AgentAdapter` |
| 注册表清单 | `server/src/agents/mod.rs` 的 `adapter_registry!` |
| 会话与结算门面分支（按需） | `server/src/agents/mod.rs` 的 `session_for_job`、`finish_job` |
| 注册表契约测试 | `server/src/agents/mod.rs` 的 `registry_lists_each_adapter_kind_once` |
| API 校验覆盖 | `server/src/api.rs` 的 `agents::kinds()` 循环测试 |
| 端到端假 CLI | `tests/test_agent_adapters.py` |

## 7. 不纳入适配层的内容

| 内容 | 归属 | 说明 |
|---|---|---|
| activity 格式识别 | `server/src/activity.rs` | 活动流按 `FORMAT_OPENCODE`、`FORMAT_CODEX`、`FORMAT_WORKBUDDY`、`FORMAT_TEXT` 识别，从作业 transcript 投影；适配器不声明格式，也不参与事件映射。新 CLI 的 JSONL 结构需要活动视图时，在 `server/src/activity.rs` 增加格式解析。 |
| 调度策略 | `server/src/scheduler.rs` | 作业排队、优先级、Agent 可用性窗口与用量上限由调度器判定；适配器只回答配置是否可用和单轮如何运行。 |
| 数据库契约 | `server/src/db.rs`、`server/src/models.rs` | 表结构、`AgentRow` 字段与作业用量列由数据层维护；适配器通过 `AgentConfig::from_row` 读取配置，不直接访问数据库。 |
| HTTP 契约 | `server/src/api.rs` | 请求字段、响应结构、状态码与错误文案由 API 层维护；适配层只提供类型清单与配置校验结果。 |
| 作业生命周期 | `server/src/runner/` | 工作区准备、续轮、看门狗、结果文件解析、失败分类与用量持久化由 runner 维护；适配器不决定续轮次数，也不写作业状态。 |
| 进程监督 | `server/src/agents/process.rs` | 进程启动、输出上限、超时与进程组终止是共享机制；适配器只提供可执行文件、参数、工作目录和 stdin。 |

## 8. 契约定位

```bash
grep -n 'trait AgentAdapter' server/src/agents/adapter.rs
grep -n 'adapter_registry!' server/src/agents/mod.rs
grep -n 'pub fn kinds\|pub fn requires_command_template\|pub fn validate_config\|pub async fn execute\|pub fn session_for_job\|pub async fn finish_job' server/src/agents/mod.rs
grep -n 'enum JobUsage' server/src/agents/adapter.rs
grep -n 'struct FinishContext' server/src/agents/adapter.rs
```

## 注册表分发约束

会话与用量结算分发同样由 `adapter_registry!` 生成；接入适配器只需实现 trait 并登记一次，不在门面中手写类型分支。本文关于手工登记 `session_for_job` 与 `finish_job` 的步骤不适用于当前实现。当前技术约束以 [TECH](../TECH.md) 为准。
