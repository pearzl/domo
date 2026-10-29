#!/usr/bin/env bash
# 旧数据库迁移测试。
#
# 覆盖范围：
#   1. 目标确认/规划阶段合并为项目分析阶段；
#   2. confirm/goal 作业角色合并为 analysis；
#   3. goal_confirm_mode / goal_proposal / goal_confirmed_at 列重命名为
#      confirm_mode / proposal / confirmed_at；
#   4. 旧 ready 方案被清空，已确认时间保持不变。
#
# 约定：只启动一次服务完成迁移，不派发作业；临时数据全部放在 ${TMPDIR:-/tmp}。
# 退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-migration.$$"
PORT="${DOMO_MIGRATION_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)}"
SERVER_PID=""

cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

mkdir -p "$TMP_ROOT/data"

python3 - "$TMP_ROOT/data/domo.db" <<'PY'
import sqlite3
import sys

db = sqlite3.connect(sys.argv[1])
db.executescript(
    """
CREATE TABLE projects (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, goal TEXT NOT NULL,
    repo_path TEXT NOT NULL, base_branch TEXT NOT NULL, branch TEXT NOT NULL,
    workspace TEXT NOT NULL, stage TEXT NOT NULL, status TEXT, blocked_reason TEXT,
    report TEXT, report_feedback TEXT, replan_reason TEXT, replan_source TEXT,
    replan_requested_at TEXT, goal_confirm_mode TEXT NOT NULL DEFAULT 'manual',
    goal_proposal TEXT, goal_confirmed_at TEXT,
    completion_mode TEXT NOT NULL DEFAULT 'tasks', merged_at TEXT, merged_commit TEXT,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
    role TEXT NOT NULL, agent_id TEXT, status TEXT NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 0, prompt TEXT NOT NULL,
    result TEXT, stdout TEXT, stderr TEXT, error TEXT, exit_code INTEGER, pid INTEGER,
    failure_kind TEXT, input_tokens INTEGER, output_tokens INTEGER,
    cache_read_tokens INTEGER, cache_write_tokens INTEGER, reasoning_tokens INTEGER,
    cost REAL, duration_ms INTEGER, usage_known INTEGER NOT NULL DEFAULT 0,
    started_at TEXT, finished_at TEXT, created_at TEXT NOT NULL
);
INSERT INTO projects VALUES
 ('p1','OldGoal','g1','/tmp/repo','main','b1','/tmp/w1','goal_confirm','blocked','等待用户答复或确认目标',NULL,NULL,NULL,NULL,NULL,'manual','{"kind":"ready","summary":"s","goal":"g1","assumptions":[],"acceptance":[],"plan":[],"verification":[],"skip_confirmation":false}',NULL,'tasks',NULL,NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
 ('p2','OldPlanning','g2','/tmp/repo','main','b2','/tmp/w2','planning','paused',NULL,NULL,NULL,'why',NULL,'2026-01-01T00:00:00Z','auto',NULL,'2026-01-01T00:00:00Z','tasks',NULL,NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
 ('p3','OldExecuting','g3','/tmp/repo','main','b3','/tmp/w3','executing','paused',NULL,NULL,NULL,NULL,NULL,NULL,'manual',NULL,'2026-01-01T00:00:00Z','tasks',NULL,NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
INSERT INTO jobs (id, project_id, role, status, prompt, created_at) VALUES
 ('j1','p1','confirm','succeeded','x','2026-01-01T00:00:00Z'),
 ('j2','p2','goal','succeeded','x','2026-01-01T00:00:00Z');
"""
)
db.commit()
PY

echo "==> 构建后端"
cargo build --manifest-path "$ROOT/server/Cargo.toml" >/dev/null

echo "==> 启动 DOMO 完成迁移 (port $PORT)"
DOMO_DATA_ROOT="$TMP_ROOT/data" \
DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
DOMO_LOG_ROOT="$TMP_ROOT/log" \
DOMO_SERVER_PORT="$PORT" \
"$ROOT/server/target/debug/domo-server" >"$TMP_ROOT/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 120); do
  if curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null; then
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "服务启动失败" >&2
    tail -80 "$TMP_ROOT/server.log" >&2
    exit 1
  fi
  sleep 0.1
done
curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null

kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""

python3 - "$TMP_ROOT/data/domo.db" <<'PY'
import sqlite3
import sys

db = sqlite3.connect(sys.argv[1])
columns = {row[1] for row in db.execute("PRAGMA table_info('projects')")}
assert {"confirm_mode", "proposal", "confirmed_at"} <= columns, columns
assert not ({"goal_confirm_mode", "goal_proposal", "goal_confirmed_at"} & columns), columns

rows = {
    row[0]: row[1:]
    for row in db.execute(
        "SELECT id, stage, confirm_mode, proposal, confirmed_at FROM projects ORDER BY id"
    )
}
assert rows["p1"][0] == "analysis", rows
assert rows["p1"][1] == "manual", rows
assert rows["p1"][2] is None, rows
assert rows["p1"][3] is None, rows
assert rows["p2"][0] == "analysis", rows
assert rows["p2"][1] == "auto", rows
assert rows["p2"][3] == "2026-01-01T00:00:00Z", rows
assert rows["p3"][0] == "executing", rows

roles = {row[0]: row[1] for row in db.execute("SELECT id, role FROM jobs")}
assert roles == {"j1": "analysis", "j2": "analysis"}, roles

print("旧数据库迁移通过")
PY

echo "==> 通过：$(basename "$0")"
