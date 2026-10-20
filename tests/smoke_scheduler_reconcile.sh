#!/usr/bin/env bash
# ARCH A2 调度收敛 —— kill -9 后启动对账冒烟（无模型依赖）。
#
# 覆盖：
#   1. 执行中的任务在 kill -9 后重启：不残留 executing（转 recovering），项目保持 paused；
#   2. 重启后 agent 占用清空，无幽灵任务；
#   3. 启动对账合并重复的 open 收件箱条目（只留一条，另一条标记 deduped）；
#   4. SIGTERM 优雅停机（脚本退出时清理进程）。
#
# 用法：bash tests/smoke_scheduler_reconcile.sh
#       DOMO_RECONCILE_SMOKE_PORT=xxxx bash tests/smoke_scheduler_reconcile.sh
# 退出码 0 = 全部通过；结束清理自己启动的进程与临时目录。
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_RECONCILE_SMOKE_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()' 2>/dev/null || echo 8796)}"
RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/domo_sched_reconcile.XXXXXX")"
DB="$RUN_DIR/domo.db"
WS="$RUN_DIR/workspace"
LOG="$RUN_DIR/server.log"
PID_FILE="$RUN_DIR/server.pid"
mkdir -p "$WS"
SERVER_PID=""

cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ "${DOMO_KEEP:-0}" == "1" ]]; then
    echo "[reconcile-smoke] kept run dir: $RUN_DIR"
  else
    rm -rf "$RUN_DIR"
  fi
}
trap cleanup EXIT

start_server() {
  (
    cd "$ROOT/server"
    DOMO_DB="$DB" DOMO_PORT="$PORT" DOMO_LOG="$LOG" \
      DOMO_LOG_LEVEL="${DOMO_LOG_LEVEL:-warn}" \
      DOMO_WORKSPACE_ROOT="$RUN_DIR/domo-ws" \
      exec "$ROOT/server/target/debug/domo-server"
  ) >"$RUN_DIR/stdout.log" 2>&1 &
  SERVER_PID=$!
  echo "$SERVER_PID" >"$PID_FILE"
  for _ in $(seq 1 100); do
    curl -sf --max-time 1 "http://127.0.0.1:$PORT/api/projects" >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  echo "[reconcile-smoke] server did not come up; log:"; tail -30 "$RUN_DIR/stdout.log"
  return 1
}

echo "[reconcile-smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[reconcile-smoke] build failed"; exit 1; }

echo "[reconcile-smoke] starting server on :$PORT (db $DB)"
start_server || exit 1

# 第一阶段：执行中暂停 → kill -9 → 插入重复收件箱。
python3 - "$PORT" "$WS" "$DB" "$PID_FILE" <<'PY' || exit 1
import json, os, signal, sqlite3, sys, time, urllib.error, urllib.request

PORT, WS, DB, PID_FILE = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
BASE = f"http://127.0.0.1:{PORT}/api"
CHECKS = []


def check(name, cond, detail=""):
    cond = bool(cond)
    CHECKS.append((name, cond))
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  -- " + str(detail)) if detail and not cond else ""), flush=True)


def req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(BASE + path, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=20) as resp:
            return resp.status, json.load(resp)
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.load(e)
        except Exception:
            return e.code, {}


def wait_until(fn, timeout=30):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(0.2)
    return None


def task_of(pid, tid):
    s, r = req("GET", f"/projects/{pid}")
    return next((t for t in (r or {}).get("tasks", []) if t.get("id") == tid), None)


def project(pid):
    s, r = req("GET", f"/projects/{pid}")
    return (r or {}).get("project") or {}


s, ex = req("POST", "/agents", {"name": "reconcile-exec", "adapter": "mock", "roles": ["executor"],
                                "capabilities": {"max_parallel": 1}, "config": {"duration_ms": 5000}})
s2, acc = req("POST", "/agents", {"name": "reconcile-acc", "adapter": "mock", "roles": ["acceptor"],
                                  "capabilities": {"max_parallel": 1}, "config": {"duration_ms": 50}})
check("create mock agents", s == 200 and s2 == 200, (ex, acc))

s, p = req("POST", "/projects", {"name": "重启对账", "brief": "kill -9 reconcile"})
pid = p.get("id", "")
req("PATCH", f"/projects/{pid}", {"workspace": {"kind": "local", "path": WS}})
s, _ = req("POST", f"/projects/{pid}/confirm", {})
check("project running", s == 200 and project(pid).get("status") == "running", project(pid))

s, t = req("POST", f"/projects/{pid}/tasks", {
    "title": "慢执行", "description": "kill -9 时仍在执行", "depends_on": [],
    "executor_agent_id": ex.get("id"), "acceptor_agent_id": acc.get("id"),
    "acceptance_required": True, "acceptance_criteria": "可验收",
})
tid = t.get("id", "")
check("task created", s == 200 and tid, t)
t = wait_until(lambda: (task_of(pid, tid) or {}).get("status") == "executing" and task_of(pid, tid), timeout=30)
check("task reaches executing", bool(t), task_of(pid, tid))
s, _ = req("POST", f"/projects/{pid}/pause", {})
check("pause requested while task is executing", s == 200, s)
check("project pausing before kill", project(pid).get("status") in {"pausing", "paused"}, project(pid))

with open(PID_FILE) as f:
    server_pid = int(f.read().strip())
os.kill(server_pid, signal.SIGKILL)
time.sleep(0.5)
check("server process gone", not os.path.exists(f"/proc/{server_pid}"))

# 模拟重复收件箱：重启对账应只保留一条 open。
c = sqlite3.connect(DB)
payload = json.dumps({"task_id": tid, "reason": "failure_limit"})
for i in ("inb_dup_1", "inb_dup_2"):
    c.execute("INSERT INTO inbox (id,project_id,\"type\",payload,status,created_at) VALUES (?,?,?,?, 'open', 1)",
              (i, pid, "escalation", payload))
c.commit()
c.close()

failed = [n for n, ok in CHECKS if not ok]
print(f"[reconcile-smoke] phase1 {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
sys.exit(1 if failed else 0)
PY

disown "$SERVER_PID" 2>/dev/null || true
echo "[reconcile-smoke] restarting server on same DB"
start_server || exit 1

# 第二阶段：验证重启对账结果。
python3 - "$PORT" <<'PY'
import json, sys, urllib.error, urllib.request

PORT = sys.argv[1]
BASE = f"http://127.0.0.1:{PORT}/api"
CHECKS = []


def check(name, cond, detail=""):
    cond = bool(cond)
    CHECKS.append((name, cond))
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  -- " + str(detail)) if detail and not cond else ""), flush=True)


def req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(BASE + path, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=20) as resp:
            return resp.status, json.load(resp)
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.load(e)
        except Exception:
            return e.code, {}


s, projects = req("GET", "/projects")
check("server restarted", s == 200 and isinstance(projects, list) and len(projects) == 1, (s, projects))
pid = projects[0]["id"]
s, d = req("GET", f"/projects/{pid}")
proj = (d or {}).get("project") or {}
tasks = (d or {}).get("tasks") or []
t = next((x for x in tasks if x.get("title") == "慢执行"), None)
check("project is paused after restart", proj.get("status") == "paused", proj)
check("interrupted task is recovering (not executing)", (t or {}).get("status") == "recovering", t)
check("interrupted task is not done", (t or {}).get("status") != "done", t)
s, agents = req("GET", "/agents")
check("agent occupancy cleared", all(not a.get("current_task_ids") for a in (agents or [])), agents)
s, inbox = req("GET", "/inbox")
dups = [i for i in (inbox or []) if (i.get("payload") or {}).get("task_id") == (t or {}).get("id")
        and (i.get("payload") or {}).get("reason") == "failure_limit"]
open_dups = [i for i in dups if i.get("status") == "open"]
resolved_dups = [i for i in dups if (i.get("resolution") or {}).get("resolution") == "deduped"]
check("duplicate inbox items merged", len(open_dups) == 1 and len(resolved_dups) >= 1, dups)

failed = [n for n, ok in CHECKS if not ok]
print(f"\n[reconcile-smoke] {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
sys.exit(1 if failed else 0)
PY
STATUS=$?
if [[ "$STATUS" != "0" ]]; then exit "$STATUS"; fi

# SIGTERM 优雅停机：进程应在 3s 内退出。
echo "[reconcile-smoke] checking SIGTERM graceful shutdown"
kill -TERM "$SERVER_PID" 2>/dev/null || true
for _ in $(seq 1 30); do
  kill -0 "$SERVER_PID" 2>/dev/null || break
  sleep 0.1
done
if kill -0 "$SERVER_PID" 2>/dev/null; then
  echo "  FAIL  graceful shutdown within 3s"
  exit 1
fi
echo "  PASS  graceful shutdown within 3s"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
