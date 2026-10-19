#!/usr/bin/env bash
# 兼容入口：M3 动态重规划「常规纠偏」端到端验收套件位于 tests/smoke_replan.sh。
# 保留 scripts/ 路径以兼容既有调用方式。
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec bash "$ROOT/tests/smoke_replan.sh" "$@"
