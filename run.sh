#!/usr/bin/env bash
# Rebuild the web bundle and start the server.
#
# Usage:
#   ./run.sh                 # release build, then serve on http://127.0.0.1:8787
#   ./run.sh --debug         # debug build with faster compilation
#   ./run.sh -- --help       # arguments after -- are passed to `cargo run`
#
# Requires Node.js 20+ and Rust 1.91+ on PATH.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

CARGO_ARGS=(run)
if [[ "${1:-}" == "--debug" ]]; then
  shift
else
  CARGO_ARGS+=(--release)
fi

cd "$ROOT/web"
npm install --no-audit --no-fund
npm run build

cd "$ROOT/server"
exec cargo "${CARGO_ARGS[@]}" "$@"
