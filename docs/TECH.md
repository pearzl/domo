# DOMO 技术方案

> 本文详细说明 DOMO 的技术方案和工程取舍。（本文由人工维护，AI不得擅自修改）

## 技术选型和部署形态

DOMO 被实现为一个本地 Web 应用。能快速提供简单易用的图形界面的同时，还能很好的支持 WSL 等场景。

后端：Rust（Axum + Tokio + SQLite）；前端：React + Vite + TypeScript。


## 工作区与数据

工作区根默认在 `~/.domo/workspace/`，可由 `DOMO_WORKSPACE_ROOT` 覆盖。

数据默认存在放 `~/.domo/data/`, 可由 `DOMO_DATA_ROOT` 覆盖。

数据默认存在放 `~/.domo/log/`, 可由 `DOMO_LOG_ROOT` 覆盖。