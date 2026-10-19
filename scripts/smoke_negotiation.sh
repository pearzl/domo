#!/usr/bin/env bash
# 兼容入口：M3 验收标准协商端到端冒烟位于 tests/smoke_negotiation.sh。
# 保留 scripts/ 路径以兼容既有调用方式。
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec bash "$ROOT/tests/smoke_negotiation.sh" "$@"
