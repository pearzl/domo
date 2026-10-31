# Codex 与 WorkBuddy CLI 协议核对

> 记录 `codex` 与 `codebuddy` 在非交互模式下的真实输出协议，作为 DOMO 把两者接入为一等 Agent 时的解析依据。
> 文中的 JSONL 样例来自实际运行，只替换了本机路径、会话标识与密钥占位符；事件均由对应 CLI 实际输出，未补造。

## 0. 核对环境

| 项目 | Codex | WorkBuddy |
|---|---|---|
| 可执行文件 | `codex` | `codebuddy` |
| 版本 | `codex-cli 0.160.0` | `2.178.1` |
| 模型 | `qiniu/conatus-free`（免费） | `hy3`（免费额度） |
| 输出模式 | `codex exec --json` | `codebuddy -p --output-format stream-json --verbose` |
| 结果 | 工具调用、工具失败、最终消息、turn 用量均取得 | 工具调用、工具失败、最终消息、result 用量均取得 |

两台 CLI 都安装在 Linux x86_64 开发机。Codex 使用的供应商只提供 Chat Completions，不提供 Responses API；原生请求在 `/v1/responses` 返回 404。为核对 Codex 的完整事件流，核对运行在本地放了一个只做协议转译的 Chat Completions → Responses 转接层，模型调用、工具执行与 JSONL 输出都由 `codex` 进程自身完成。转接层不随仓库交付，也不参与自动化验收。

密钥通过环境变量 `QINIU_API_KEY` 提供，值不写入仓库、日志或本文。WorkBuddy 使用自身账号的免费额度，未读取或记录任何凭据。

---

## 1. Codex

### 1.1 可执行命令

```bash
codex --version
# codex-cli 0.160.0

printf '%s' '<prompt>' | codex exec --json \
  -m qiniu/conatus-free \
  --skip-git-repo-check \
  --sandbox workspace-write \
  --add-dir <extra-dir> \
  -C <cwd> \
  - \
  -o <cwd>/last.txt
```

提示词也可以直接作为位置参数传入；DOMO 的命令模板不写 `{prompt}` 时，runner 会把提示词写入子进程 stdin。

DOMO 的 CLI Agent 模板：

```json
["codex", "exec", "--json", "--skip-git-repo-check", "--sandbox", "workspace-write", "-C", "{cwd}", "--add-dir", "{job_dir}", "{model_args}", "-"]
```

模型参数模板：

```json
["-m", "{model}"]
```

`{job_dir}` 作为额外可写目录时，Agent 才能把 `result.json` 写到作业目录。若作业目录已经在 `{cwd}` 内，可省略该参数。

命令模板支持 `{prompt}`、`{job_dir}`、`{cwd}`、`{model}`、`{model_args}` 和 `{session_id}`。模板包含 `{session_id}` 时，DOMO 认为该 CLI 支持会话复用，继续轮次会把同一 ID 传给模板。

### 1.2 参数语义

以下语义与 `codex exec --help`（0.160.0）一致：

| 参数 | 语义 |
|---|---|
| `--json` | 把事件按 JSONL 打印到 stdout，一行一个事件。 |
| `-m, --model <MODEL>` | 指定该次调用的模型。 |
| `-s, --sandbox <SANDBOX_MODE>` | 选择模型生成命令的沙箱策略，可选 `read-only`、`workspace-write`、`danger-full-access`。 |
| `--add-dir <DIR>` | 在主工作区之外增加可写目录；权限跟随沙箱策略。 |
| `-C, --cd <DIR>` | 指定 Agent 的工作根目录。 |
| `[PROMPT]` 或 `-` | 不传提示词或传 `-` 时从 stdin 读取；stdin 有管道且同时传了提示词时，stdin 内容作为 `<stdin>` 块追加到提示词。 |
| `-o, --output-last-message <FILE>` | 把最后一条 Agent 消息写入文件；不是 DOMO 的角色结果协议。 |
| `--skip-git-repo-check` | 允许在非 Git 目录运行。 |
| `--dangerously-bypass-approvals-and-sandbox` | 跳过审批与沙箱；核对运行未使用。 |

实际核对覆盖了 `--json`、`-m`、`--sandbox workspace-write`、`--add-dir`、`-C`、`-` 读 stdin 和 `-o`。在本地 stub 返回固定命令的核对中，`--add-dir` 指向工作目录的兄弟目录后 `exec_command` 写入成功、退出码为 0；不传 `--add-dir` 时同一写入被只读沙箱拒绝。

### 1.3 输出格式

- stdout：每行一个 JSON 对象，事件之间没有额外分隔。
- stderr：CLI 的运行提示。stdin 不是终端时会出现 `Reading additional input from stdin...`；该行不是错误。
- 退出码：`turn.completed` 正常结束时为 0；`turn.failed` 时为 1。
- 每个事件都有 `type`。工具调用、消息等放在 `item` 字段，`item.type` 区分具体类型。

实测出现的事件：

| 事件 | 关键字段 | 含义 |
|---|---|---|
| `thread.started` | `thread_id` | 会话开始。 |
| `turn.started` | 无 | 一轮对话开始。 |
| `item.started` | `item.id`、`item.type`、`item.status` | 工具或消息开始；命令执行的 `exit_code` 此时为 `null`。 |
| `item.completed` | `item` | 工具或消息完成。 |
| `error` | `message` | 可重试错误或重连提示，可连续出现。 |
| `turn.completed` | `usage` | 一轮正常结束，带用量。 |
| `turn.failed` | `error.message` | 一轮失败，进程以非 0 退出。 |

实测出现的 `item.type`：

| `item.type` | 关键字段 | 含义 |
|---|---|---|
| `command_execution` | `command`、`aggregated_output`、`exit_code`、`status` | 执行 shell 命令。`status` 为 `in_progress`、`completed` 或 `failed`。 |
| `agent_message` | `text` | Agent 给用户的最终文本。 |
| `error` | `message` | CLI 级告警，例如自定义供应商缺少模型元数据。该 item 本身不使 turn 失败。 |

核对未触发 `file_change`、`mcp_tool_call`、`web_search`、`reasoning` 等 item；解析层遇到未知 `item.type` 时应降级为 `line`，不要猜测字段。

### 1.4 真实事件样例：正常 turn

以下样例来自一次真实模型运行：模型先执行失败命令，再写 `result.json`，最后给出文本。仅把线程标识替换为占位符。

```jsonl
{"type":"thread.started","thread_id":"<thread-id>"}
{"type":"item.completed","item":{"id":"item_0","type":"error","message":"Model metadata for `qiniu/conatus-free` not found. Defaulting to fallback metadata; this can degrade performance and cause issues."}}
{"type":"turn.started"}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'printf hello > note.txt; cat missing.txt'","aggregated_output":"","exit_code":null,"status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'printf hello > note.txt; cat missing.txt'","aggregated_output":"cat: missing.txt: No such file or directory\n","exit_code":1,"status":"failed"}}
{"type":"item.completed","item":{"id":"item_2","type":"command_execution","command":"/bin/bash -lc \"printf '%s' '{\\\"kind\\\":\\\"done\\\",\\\"summary\\\":\\\"probe\\\"}' > result.json && cat result.json && cat note.txt\"","aggregated_output":"{\"kind\":\"done\",\"summary\":\"probe\"}hello","exit_code":0,"status":"completed"}}
{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"`cat missing.txt` failed as expected, and `result.json` and `note.txt` are written."}}
{"type":"turn.completed","usage":{"input_tokens":21349,"cached_input_tokens":13952,"cache_write_input_tokens":0,"output_tokens":130,"reasoning_output_tokens":0}}
```

要点：

- 命令失败时 `item.completed` 仍会出现，`exit_code` 为 `1`，`status` 为 `failed`，`aggregated_output` 保留 stderr。turn 本身仍然成功，退出码为 0。
- `item_0` 的 `error` 是自定义供应商缺少内置模型元数据的告警，不是模型或网络失败。
- `turn.completed.usage` 是一轮的总用量，不按工具调用拆分。

### 1.5 真实事件样例：turn 失败

以下样例来自对只提供 Chat Completions 的供应商直接发起 Responses 请求，`/v1/responses` 返回 404。重试行 1/5 至 5/5 结构相同，这里保留首行与最终失败行。

```jsonl
{"type":"thread.started","thread_id":"<thread-id>"}
{"type":"item.completed","item":{"id":"item_0","type":"error","message":"Model metadata for `qiniu/conatus-free` not found. Defaulting to fallback metadata; this can degrade performance and cause issues."}}
{"type":"turn.started"}
{"type":"error","message":"Reconnecting... 1/5 (unexpected status 404 Not Found: {\"message\":\"not found or method not allowed\",\"status\":false}, url: https://api.qnaigc.com/v1/responses)"}
{"type":"error","message":"unexpected status 404 Not Found: {\"message\":\"not found or method not allowed\",\"status\":false}, url: https://api.qnaigc.com/v1/responses)"}
{"type":"turn.failed","error":{"message":"unexpected status 404 Not Found: {\"message\":\"not found or method not allowed\",\"status\":false}, url: https://api.qnaigc.com/v1/responses)"}}
```

进程退出码为 `1`，stdout 之外只有 `Reading additional input from stdin...`；没有 `turn.completed`，也没有 `usage`。失败原因取 `turn.failed.error.message` 或最后一行的 `error.message`。

### 1.6 事件到 DOMO 活动的映射

DOMO 的活动字段是 `kind`、`text`、`tool`、`target`、`status`、`error`，汇总字段是 `messages`、`tool_calls`、`tool_failures`、`steps`。Codex 事件的映射如下：

| Codex 事件 | DOMO `kind` | 字段映射 | 汇总 |
|---|---|---|---|
| `thread.started`、`turn.started` | 不产生活动 | — | — |
| `item.completed` 且 `item.type=agent_message` | `message` | `text=item.text` | `messages += 1` |
| `item.started` 且 `item.type=command_execution` | `tool` | `tool="exec_command"`，`target=item.command` 首行，`status="in_progress"` | `tool_calls += 1` |
| `item.completed` 且 `item.type=command_execution`、`status=completed` | `tool` | `tool="exec_command"`，`target=item.command` 首行，`status="completed"` | `tool_calls += 1` |
| `item.completed` 且 `item.type=command_execution`、`status=failed` 或 `exit_code != 0` | `tool` | `status="error"`，`error=item.aggregated_output` 首行 | `tool_calls += 1`、`tool_failures += 1` |
| `item.completed` 且 `item.type=error` | `line` | `text=item.message` | — |
| 顶层 `error` | `line` | `text=message` | — |
| `turn.failed` | `line` | `text=error.message` | — |
| `turn.completed` | 不产生活动 | 只取 `usage` | — |
| 无法识别的 `item.type` | `line` | `text=原始 JSON` | — |

`target` 取命令首行，并截断到 500 字符，避免把完整脚本写进时间线。`tool_failures` 只统计工具失败，不把 `turn.failed` 计入，后者由作业状态表达。

### 1.7 usage 字段映射

`turn.completed.usage` 的字段映射到 DOMO `UsageRecord`：

| Codex 字段 | DOMO 字段 | 说明 |
|---|---|---|
| `input_tokens` | `input_tokens` | 总输入，包含缓存命中部分。 |
| `cached_input_tokens` | `cache_read_tokens` | 总输入中的缓存读取量，作为明细保留，不重复累加。 |
| `cache_write_input_tokens` | `cache_write_tokens` | 缓存写入量。 |
| `output_tokens` | `output_tokens` | 总输出。 |
| `reasoning_output_tokens` | `reasoning_tokens` | 输出中的推理 token，作为明细保留。 |
| 无对应字段 | `cost` | 留 `None`；`codex exec` 不返回费用，不能填 0。 |
| 无对应字段 | `duration_ms` | 由作业 `started_at`/`finished_at` 计算。 |

没有 `turn.completed` 时整条用量记为未知，不把各字段置 0。DOMO 的聚合只消费已知用量。

### 1.8 result.json

- DOMO 在作业开始前删除 `result.json`，作业结束后从作业目录读取。Codex 不会自动写这个文件。
- `-o/--output-last-message` 只写最后一条 Agent 文本。核对运行中 `last.txt` 的内容是 `Wrote "extra" to ...`，不是角色协议要求的 JSON，不能把 `-o` 直接指向 `result.json`。
- 真实运行里，模型通过 `exec_command` 写入 `{"kind":"done","summary":"probe"}`，证明“提示词要求 + 可写作业目录”可以产出协议文件。
- `result.json` 可携带 `usage` 对象；DOMO 既从 `AgentRun.usage` 取用量，也从结果 JSON 的 `usage` 取用量。字段名与 `UsageRecord` 一致：`input_tokens`、`output_tokens`、`cache_read_tokens`、`cache_write_tokens`、`reasoning_tokens`、`cost`、`duration_ms`。
- DOMO 的结果解析允许 JSON 前后有说明文字或 `json` 代码围栏，但内容必须是合法 JSON 对象。
- 退出码为 0 但没有 `result.json` 时，DOMO 在同一作业内最多追加 5 轮继续提示，并提示执行者可以派生子任务；OpenCode 复用同一会话，其他支持会话的 Agent 优先复用会话。5 轮后仍缺少结果按协议失败结算并阻塞，不切换 Agent。
- Codex 从首次运行的 `thread.started.thread_id` 取会话标识，继续轮次使用 `codex exec resume <thread_id>`。

执行者角色的最小结果：

```json
{"kind":"done","summary":"完成的工作摘要","usage":{"input_tokens":21349,"output_tokens":130,"cache_read_tokens":13952,"cache_write_tokens":0,"reasoning_tokens":0}}
```

### 1.9 失败判定规则

| 条件 | 判定 |
|---|---|
| 进程退出码非 0 | 进程失败，进入 DOMO 的失败分类。 |
| 出现 `turn.failed` | 作业失败，原因取 `error.message`；即使退出码缺失也按失败处理。 |
| 退出码为 0 但没有 `turn.completed` | 视为协议不完整，按失败处理。 |
| `item.type=command_execution` 且 `exit_code != 0` | 只记工具失败，不直接使作业失败；是否失败由后续 `result.json` 或最终 turn 决定。 |
| 退出码为 0、`turn.completed` 正常，但没有 `result.json` | 在同一作业内最多追加 5 轮继续提示；仍未写则按协议失败处理并阻塞，不换 Agent。 |
| `result.json` 不是合法 JSON 或不符合角色协议 | 归入协议失败。 |
| 错误文本含 `429`、`rate limit`、`quota` 等 | 归入限流，按重试信息设置冷却。 |
| 错误文本含 `401`、`403`、`invalid api key`、`invalid_request` 等 | 归入永久错误，不重试。 |
| 进程失败、超时或错误文本含 `502`、`503`、`504`、`timeout` 等 | 归入供应商波动，按重试次数处理。 |

`item.type=error` 的告警不能单独判失败；核对中的模型元数据告警与正常 `turn.completed` 同时出现，退出码为 0。

### 1.10 已知限制与未实测项

- Codex 0.160.0 只接受 `wire_api = "responses"`；配置 `wire_api = "chat"` 会直接报错 `wire_api = "chat" is no longer supported`。只提供 Chat Completions 的供应商不能直接接入。
- 供应商 `api.qnaigc.com` 没有 `/v1/responses`，原生请求稳定返回 404。完整事件流通过本地协议转接层取得；供应商兼容 Responses API 时不需要转接层。
- 自定义供应商不在 Codex 内置模型元数据中时，首条 `item.completed` 会带 `error` 告警。该告警可忽略，但会占用一个 item。
- ChatGPT 账号登录方式未实测：核对网络无法访问 `chatgpt.com`，请求在连接阶段失败。该失败没有进入 turn，不能作为协议样例。
- `file_change`、`mcp_tool_call`、`web_search`、`reasoning`、`todo_list` 等 item 未实测，解析层按未知类型降级为 `line`。
- `--dangerously-bypass-approvals-and-sandbox` 未实测；核对只使用 `workspace-write`。
- `--add-dir` 的可写性依赖沙箱策略；在 `read-only` 下只读，未实测写入。
- stdin 不是终端时 stderr 固定出现 `Reading additional input from stdin...`，解析器不要把 stderr 非空当作失败。

---

## 2. WorkBuddy（可执行文件 `codebuddy`）

### 2.1 可执行命令

```bash
codebuddy --version
# 2.178.1

printf '%s' '<prompt>' | codebuddy -p \
  --output-format stream-json \
  --verbose \
  --dangerously-skip-permissions \
  --model hy3 \
  --add-dir <extra-dir> \
  --no-session-persistence
```

`-p/--print` 进入非交互模式；不传位置参数时提示词从 stdin 读取。核对运行全部使用 stdin 传提示词。

DOMO 的 CLI Agent 模板：

```json
["codebuddy", "-p", "--output-format", "stream-json", "--verbose", "--dangerously-skip-permissions", "--no-session-persistence", "--add-dir", "{job_dir}", "{model_args}"]
```

模型参数模板：

```json
["--model", "{model}"]
```

`--no-session-persistence` 阻止 CLI 在用户目录写会话文件；DOMO 的作业目录已经保存提示词与输出，不需要 CLI 再存一份。

### 2.2 参数语义

以下语义与 `codebuddy --help`（2.178.1）一致：

| 参数 | 语义 |
|---|---|
| `-p, --print` | 打印响应后退出，用于管道。 |
| `--output-format <format>` | 只对 `--print` 生效，可选 `text`、`json`、`stream-json`；`stream-json` 实时输出事件。 |
| `--verbose` | 覆盖配置中的详细模式。 |
| `-y, --dangerously-skip-permissions` | 跳过权限询问；`HIGH/CRITICAL` 风险仍会询问。 |
| `--model <model>` | 指定模型，帮助信息列出了账号可用模型。 |
| `--add-dir <directories...>` | 增加工具可访问的目录。 |
| `--no-session-persistence` | 会话只留在内存，不写入磁盘。 |
| `--input-format <format>` | 只对 `--print` 生效，可选 `text`、`stream-json`。 |

`--dangerously-skip-permissions` 并不保证完全无交互：帮助信息明确 `HIGH/CRITICAL` 风险仍会询问。DOMO 的自动化命令应避免触发高风险操作，否则进程可能停在询问上直到超时。

实际核对覆盖了 stdin 提示词、`--model hy3`、`--add-dir`、`--dangerously-skip-permissions` 和 `--no-session-persistence`。`--add-dir` 指向工作目录的兄弟目录时，模型通过 `Bash` 成功写入文件。

### 2.3 输出格式

- stdout：每行一个 JSON 对象，首个事件是 `system/init`，最后一个事件是 `result`。
- stderr：成功与模型参数错误时都为空；错误信息在 `result.errors` 和 assistant 文本里。
- 退出码：正常完成为 0；模型参数错误同样为 0，错误只体现在 `result.is_error`。不能只靠退出码判断 WorkBuddy 作业成败。
- 每条事件都有 `type`、`session_id`。assistant/user 事件的消息体在 `message`，内容块在 `message.content`。

实测事件：

| 事件 | 关键字段 | 含义 |
|---|---|---|
| `system` + `subtype=init` | `session_id`、`model`、`permissionMode`、`tools`、`slash_commands` | 会话初始化。`tools` 是完整工具清单，体积较大。 |
| `system` + `subtype=status` | `status` | 状态通知，通常为 `null`。 |
| `assistant` | `message.content[]`、`message.usage` | 模型输出。内容块有 `thinking`、`text`、`tool_use`。 |
| `user` | `message.content[]` | 工具结果。内容块类型为 `tool_result`。 |
| `result` | `subtype`、`is_error`、`result`、`usage`、`modelUsage`、`duration_ms`、`total_cost_usd`、`permission_denials` | 会话终结与汇总。 |

`tool_use` 块的字段是 `id`、`name`、`input`；`tool_result` 块的字段是 `tool_use_id`、`content`、`is_error`，并带 `_meta.rawResponse` 和 `_meta.renderer`。

### 2.4 真实事件样例：正常会话

以下样例来自一次真实运行：模型读取 `task.txt`，执行会失败的 `cat missing.txt`，写 `result.json`，最后给出文本。仅替换本机路径、会话标识和工具调用标识。

```jsonl
{"type":"system","subtype":"init","session_id":"<session-id>","model":"hy3","permissionMode":"bypassPermissions","apiKeySource":"copilot.tencent.com"}
{"type":"assistant","session_id":"<session-id>","message":{"role":"assistant","content":[{"type":"tool_use","id":"<tool-id-1>","name":"Read","input":{"file_path":"<workspace>/task.txt"}}]}}
{"type":"assistant","session_id":"<session-id>","message":{"role":"assistant","content":[{"type":"tool_use","id":"<tool-id-2>","name":"Bash","input":{"command":"cat missing.txt","description":"Attempt to read missing.txt (expected to fail)"}}]}}
{"type":"user","session_id":"<session-id>","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"<tool-id-2>","content":[{"type":"text","text":"Stdout: (empty)\nStderr: cat: missing.txt: No such file or directory\n\nExit Code: 1\nSignal: (none)"}],"is_error":false,"_meta":{"rawResponse":{"exitCode":1,"signal":null,"interrupted":false,"sandboxDenied":false,"is_error":true,"error":"cat: missing.txt: No such file or directory\n"}}}]}}
{"type":"assistant","session_id":"<session-id>","message":{"role":"assistant","content":[{"type":"text","text":"task.txt contained \"alpha\" and `cat missing.txt` failed as expected; result.json has been written."}]}}
{"type":"result","subtype":"success","is_error":false,"duration_ms":17660,"duration_api_ms":17660,"num_turns":9,"total_cost_usd":0,"usage":{"input_tokens":73243,"output_tokens":274,"cache_creation_input_tokens":0,"cache_read_input_tokens":49088,"cache_creation":null,"server_tool_use":null,"service_tier":null},"modelUsage":{"hy3":{"inputTokens":24155,"outputTokens":274,"cacheReadInputTokens":49088,"cacheCreationInputTokens":0,"contextWindow":192000,"maxOutputTokens":64000}},"permission_denials":[],"result":"task.txt contained \"alpha\" and `cat missing.txt` failed as expected; result.json has been written."}
```

要点：

- `tool_result.is_error` 在 shell 退出码为 1 时仍是 `false`；真正的失败标记在 `_meta.rawResponse.is_error`（此处为 `true`）和 `_meta.rawResponse.exitCode`（此处为 `1`）。
- 工具失败不会使会话失败。`result.subtype` 仍为 `success`，退出码为 0。
- `result.usage` 是跨全部请求的累计值；`modelUsage.hy3.inputTokens` 是最后一次请求的上下文统计，与逐条 assistant 的 `input_tokens` 不完全相等，两者不能相加。

### 2.5 真实事件样例：模型参数错误

以下样例来自 `--model definitely-not-a-model`。进程退出码为 `0`，错误只出现在 `result` 事件和 assistant 文本中。

```jsonl
{"type":"system","subtype":"init","session_id":"<session-id>","model":"definitely-not-a-model","permissionMode":"bypassPermissions","apiKeySource":"copilot.tencent.com"}
{"type":"assistant","session_id":"<session-id>","message":{"role":"assistant","content":[{"type":"text","text":"400 model [definitely-not-a-model] service info not found"}],"model":"definitely-not-a-model"}}
{"type":"result","subtype":"error_during_execution","is_error":true,"duration_ms":3431,"duration_api_ms":1534,"num_turns":2,"total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0},"permission_denials":[],"errors":["400 model [definitely-not-a-model] service info not found"]}
```

`errors` 数组保留完整错误，包括账号可用模型列表。DOMO 只取首行作为失败原因，避免把整段模型清单写进事件。

### 2.6 事件到 DOMO 活动的映射

| WorkBuddy 事件 | DOMO `kind` | 字段映射 | 汇总 |
|---|---|---|---|
| `system/init`、`system/status` | 不产生活动 | 只读取会话元数据 | — |
| `assistant` 且内容块 `thinking` | `step` | `text=thinking` 首行 | `steps += 1` |
| `assistant` 且内容块 `text` | `message` | `text=块.text` | `messages += 1` |
| `assistant` 且内容块 `tool_use` | `tool` | `tool=name`，`target` 依次取 `input.file_path`、`input.path`、`input.command` 首行、`input.description`，`status="pending"` | `tool_calls += 1` |
| `user` 且内容块 `tool_result`，成功 | `tool` | 按 `tool_use_id` 更新对应工具项，`status="completed"` | — |
| `user` 且内容块 `tool_result`，`_meta.rawResponse.is_error=true` 或 `exitCode != 0` 或文本含非 0 `Exit Code` | `tool` | `status="error"`，`error` 取 stderr 首行或 `_meta.rawResponse.error` 首行 | `tool_failures += 1` |
| `result` 且 `is_error=true` | `line` | `text=errors[0]` 首行 | — |
| `result` 且 `is_error=false` | 不产生活动 | 只取用量与最终文本 | — |
| 无法识别的事件或内容块 | `line` | `text=原始 JSON` | — |

解析时按 `tool_use_id` 建索引，再回填工具状态，不依赖事件到达顺序。`tool_use_id` 是模型供应商生成的不透明字符串，不参与展示。

### 2.7 usage 字段映射

`result` 事件的字段映射到 DOMO `UsageRecord`：

| WorkBuddy 字段 | DOMO 字段 | 说明 |
|---|---|---|
| `usage.input_tokens` | `input_tokens` | 跨轮累计输入。 |
| `usage.output_tokens` | `output_tokens` | 跨轮累计输出。 |
| `usage.cache_read_input_tokens` | `cache_read_tokens` | 缓存读取量。 |
| `usage.cache_creation_input_tokens` | `cache_write_tokens` | 缓存写入量；免费模型为 0。 |
| 无对应字段 | `reasoning_tokens` | 结果协议不单列推理 token，留 `None`。 |
| `total_cost_usd` | `cost` | 免费模型为 `0`，这是已知费用，不是未知。 |
| `duration_ms` | `duration_ms` | 会话总耗时；`duration_api_ms` 只用于诊断。 |

`assistant.message.usage` 是单次请求的用量，前面的请求会从 0 开始，最后一条才带上完整上下文；用它累加会重复计数。只取 `result` 事件的汇总值。

`modelUsage` 是每个模型的最终上下文统计。`usage.input_tokens`（样例 73243）是累计请求输入，`modelUsage.hy3.inputTokens`（样例 24155）是最后请求的上下文；DOMO 记账取前者。

### 2.8 result.json

- WorkBuddy 不自动写 `result.json`；真实运行中模型通过 `Write` 工具写出 `{"kind":"done","summary":"read task and observed the missing file"}`。
- 若作业目录不在 `{cwd}` 内，命令模板必须带 `--add-dir "{job_dir}"`，否则 `Write` 可能被权限拒绝。
- 结果协议与 Codex 共用同一套 DOMO 解析：执行者读 `kind`/`summary`/`reason`，验收者读 `pass`/`reason`/`issues`。
- `result.json` 可带 `usage` 对象，字段与 `UsageRecord` 一致。CLI runner 不解析事件流；缺少该对象时用量记为未知，解析层读取事件流后可把汇总值填入 `AgentRun.usage`。
- 退出码为 0 但没有 `result.json` 时，DOMO 在同一作业内最多追加 5 轮继续提示，并提示执行者可以派生子任务；5 轮后仍缺少结果按协议失败结算并阻塞，不切换 Agent。
- WorkBuddy 从首次运行的 `system/init.session_id` 取会话标识，继续轮次使用 `--resume <session_id>`。

### 2.9 失败判定规则

| 条件 | 判定 |
|---|---|
| `result.is_error=true` | 作业失败，原因取 `errors[0]` 首行；即使退出码为 0 也按失败处理。 |
| `result.subtype` 不是 `success` | 作业失败，`error_during_execution` 表示执行期错误。 |
| 进程退出码非 0 | 进程失败，进入 DOMO 的失败分类。 |
| 退出码为 0 且 `result.is_error=false`，但没有 `result.json` | 在同一作业内最多追加 5 轮继续提示；仍未写则按协议失败处理并阻塞，不换 Agent。 |
| `result.json` 不是合法 JSON 或不符合角色协议 | 归入协议失败。 |
| `tool_result` 的 `_meta.rawResponse.is_error=true` 或 `exitCode != 0` | 只记工具失败，不直接使会话失败。 |
| `permission_denials` 非空 | 工具调用被拒绝；会话可能仍为成功，需在作业事件中提示权限不足。 |
| 错误文本含 `429`、`rate limit`、`quota` 等 | 归入限流，按重试信息设置冷却。 |
| 错误文本含 `400`、`401`、`403`、`invalid api key`、`model ... not found` 等 | 归入永久错误，不重试。 |

模型参数错误样例的 `subtype=error_during_execution`、`is_error=true`、退出码 `0`，说明“退出码成功”不能作为 WorkBuddy 作业成功的判据。

### 2.10 已知限制与未实测项

- `tool_result.is_error` 与 shell 退出码不一致；判定工具失败必须读 `_meta.rawResponse.is_error`、`_meta.rawResponse.exitCode` 或文本里的 `Exit Code`。
- 模型参数错误时进程退出码为 0；作业失败只能从 `result` 事件或缺失/非法的 `result.json` 判断。
- 无效的 `--output-format` 值被忽略并回退到 `text`，退出码仍为 0；参数校验不能依赖退出码。
- `--dangerously-skip-permissions` 对 `HIGH/CRITICAL` 风险仍会询问；非交互运行可能卡在询问上，最终由 DOMO 超时或卡死检测终止。
- 默认会写会话文件；自动化使用 `--no-session-persistence`，避免在用户目录留下会话与日志。
- `system/init` 的 `tools` 与 `slash_commands` 列表很长；活动投影必须丢弃这两个字段，否则单个事件会占用大量内存。
- 免费模型存在全局限流。核对中 `qiniu/conatus-free` 曾返回 `429 free_campaign_global_limit`；WorkBuddy 侧的限流错误未实测。
- WorkBuddy 的网络错误、鉴权失败与超时未实测；样例只覆盖成功会话与模型参数错误。
- `--input-format stream-json` 未实测；DOMO 只使用 `text` 提示词输入。

---

## 3. 接入检查清单

| 检查项 | Codex | WorkBuddy |
|---|---|---|
| 命令模板 | `codex exec --json ... -` | `codebuddy -p --output-format stream-json --verbose --dangerously-skip-permissions ...` |
| 提示词通道 | 不写 `{prompt}`，走 stdin；或用 `-` | 不写 `{prompt}`，走 stdin |
| 工作目录 | `-C {cwd}` | 进程 `cwd` 由 runner 设置 |
| 额外可写目录 | `--add-dir {job_dir}` | `--add-dir {job_dir}` |
| 成功判据 | 退出码 0 + `turn.completed` + 合法 `result.json` | 退出码 0 + `result.is_error=false` + 合法 `result.json` |
| 失败原因 | `turn.failed.error.message` 或进程错误文本 | `result.errors[0]` 或进程错误文本 |
| 工具失败 | `command_execution.exit_code != 0` | `_meta.rawResponse.is_error` / `exitCode` |
| 用量 | `turn.completed.usage` | `result.usage` |
| 费用 | 不提供，记未知 | `total_cost_usd` |
| 会话文件 | 用 `--ephemeral` 可避免持久化 | 用 `--no-session-persistence` |

自动化验收必须使用 mock Agent，不依赖外网、真实账号或免费额度；真实模型只用于人工核对协议，相关输出不入库。
