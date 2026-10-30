# DOMO

> 组建你的 AI 团队，自动交付可靠结果。

DOMO 是一个面向个人使用、本地部署的多 Agent 协作平台。

你只需要与 AI 讨论清楚目标，之后 AI 团队会自动完成“项目分析 -> 执行 -> 验收”全流程；你可以随时看到进展，也能随时叫停和纠偏；所有过程可审计、可回溯。

## 快速开始

> 要求 Node.js 20+、Rust 1.91+

```bash
# 1. 构建前端
cd web && npm install && npm run build && cd ..

# 2. 构建并启动后端（同时托管前端静态资源）
cd server && cargo run --release
# 打开启动日志中打印的地址，默认 http://127.0.0.1:8787
# 默认端口被占用时自动改用相邻空闲端口；显式设置 DOMO_SERVER_PORT 时端口冲突会直接报错
```


## 主要特性

- **项目分析与方案确认**：规划者阅读仓库、澄清目标，给出包含任务拆分、依赖和验收标准的方案，确认后才开始执行。
- **自动交付**：任务按依赖关系（DAG）自动推进，前置完成自动启动后继。
- **人工介入**：你可以随时暂停项目或任务，对 AI 进行指导，或要求重新规划后续步骤。
- **团队分工**：团队由三类角色组成。**规划者**负责项目分析和任务拆解，**执行者**负责执行，**验收者**负责核查。
- **动态调度**：可根据任务不同使用不同的 Agent 和模型，支持按优先级的顺序调度、错误退避和限额保护。
- **审计与回溯**：整个执行过程，包括每次对 Agent 调用的输入与输出有完整记录。
- **自备 Agent**：内置支持 OpenCode、Codex CLI、WorkBuddy（CodeBuddy Code CLI，可执行文件 `codebuddy`）和自定义 CLI 四类 Agent。前三类内置默认命令，只需填写名称与模型；自定义 CLI 通过命令模板描述参数，模板占位符包括 `{prompt}`、`{job_dir}`、`{cwd}`、`{model}`、`{model_args}`，未写 `{prompt}` 时提示词从 stdin 传入。可执行文件不在 `PATH` 中时用 `DOMO_CODEX_BIN`、`DOMO_WORKBUDDY_BIN` 指向文件路径，例如 `DOMO_CODEX_BIN=/opt/bin/codex DOMO_WORKBUDDY_BIN=/opt/bin/codebuddy cd server && cargo run --release`。
- **Git 仓库优先**：通过 worktree 进行隔离，支持同一工程下多个项目并行。
- **完全本地**：数据、日志和产物都保存在本地。


## 文档

| 文档 | 内容 |
|---|---|
| [docs/DESIGN.md](docs/DESIGN.md) | 产品设计：主要功能介绍，设计思路说明 |
| [docs/DEV.md](docs/DEV.md) | 开发模式和约定：目录、文档职责、测试与变更要求 |

## 技术栈

- 后端：Rust (Axum + Tokio + rusqlite/SQLite)
- 前端：React + Vite + TypeScript

## 许可证

MIT
