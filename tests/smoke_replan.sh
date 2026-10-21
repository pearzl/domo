#!/usr/bin/env bash
# 重规划（人工收件箱）冒烟 —— 无决策者 agent、无协商（方案 C）。
#
# 覆盖：
#   1. executor/acceptor 上报规划问题 → 一条 open 的 replan 收件箱条目；
#   2. 人批准 → planning=pending → 规划落地 → 新任务创建，写 replan.applied；
#   3. 人否决 → 条目关闭、replan.rejected，原任务流程不动；
#   4. 项目暂停时批准只进入 planning=pending，不派发；恢复后才落地；
#   5. 全程不出现 decision.* / negotiation.* 事件。
#
# 用法：bash tests/smoke_replan.sh
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_REPLAN_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()' 2>/dev/null || echo 8792)}"
RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/domo_replan_smoke.XXXXXX")"
DB="$RUN_DIR/domo.db"; WS="$RUN_DIR/workspace"; LOG="$RUN_DIR/server.log"
mkdir -p "$WS"
cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then kill "$SERVER_PID" 2>/dev/null || true; wait "$SERVER_PID" 2>/dev/null || true; fi
  if [[ "${DOMO_KEEP:-0}" == "1" ]]; then echo "[replan-smoke] kept run dir: $RUN_DIR"; else rm -rf "$RUN_DIR"; fi
}
trap cleanup EXIT
(cd "$ROOT/server" && cargo build --quiet) || { echo "build failed"; exit 1; }
(
  cd "$ROOT/server"
  DOMO_DB="$DB" DOMO_PORT="$PORT" DOMO_LOG="$LOG" DOMO_LOG_LEVEL="${DOMO_LOG_LEVEL:-warn}" \
    DOMO_WORKSPACE_ROOT="$RUN_DIR/domo-ws" exec "$ROOT/server/target/debug/domo-server"
) >"$RUN_DIR/stdout.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 100); do curl -sf --max-time 1 "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1 && break; sleep 0.1; done
curl -sf --max-time 2 "http://127.0.0.1:$PORT/api/agents" >/dev/null || { echo "server not up"; tail -20 "$RUN_DIR/stdout.log"; exit 1; }

python3 - "$PORT" <<'PY'
import json, sys, time, urllib.error, urllib.request
PORT = sys.argv[1]; BASE = f"http://127.0.0.1:{PORT}/api"; CHECKS = []
def check(n, c, d=""):
    c = bool(c); CHECKS.append((n, c)); print(("  PASS  " if c else "  FAIL  ") + n + (("  -- " + str(d)) if d and not c else ""), flush=True)
def req(m, p, b=None):
    data = json.dumps(b).encode() if b is not None else None
    r = urllib.request.Request(BASE + p, data=data, method=m, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=20) as x: return x.status, json.load(x)
    except urllib.error.HTTPError as e:
        try: return e.code, json.load(e)
        except Exception: return e.code, {}
def wait(fn, timeout=40):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v: return v
        time.sleep(0.2)
    return None
def project(pid): return req("GET", f"/projects/{pid}")[1].get("project") or {}
def tasks(pid): return req("GET", f"/projects/{pid}")[1].get("tasks") or []
def task_by_title(pid, title): return next((t for t in tasks(pid) if t["title"] == title), None)
def inbox(): return req("GET", "/inbox")[1] or []
def open_replan(pid): return next((i for i in inbox() if i.get("status") == "open" and i.get("type") == "replan"
                                   and (i.get("payload") or {}).get("kind") == "replan_report"
                                   and i.get("project_id") == pid), None)
def event_types(pid): return [e.get("type") for e in req("GET", f"/events?project_id={pid}")[1] or []]

s, ex = req("POST", "/agents", {"name": "rp-exec", "adapter": "mock", "roles": ["executor"], "config": {"duration_ms": 50}})
s2, acc = req("POST", "/agents", {"name": "rp-acc", "adapter": "mock", "roles": ["acceptor"], "config": {"duration_ms": 50}})
check("agents registered (no decision maker)", s == 200 and s2 == 200, (ex, acc))
s, p = req("POST", "/projects", {"name": "重规划收件箱", "brief": "replan inbox"})
pid = p.get("id", "")
req("PATCH", f"/projects/{pid}", {"workspace": {"kind": "local", "path": "/tmp"}})
req("POST", f"/projects/{pid}/confirm", {})
check("project running", project(pid).get("status") == "running", project(pid))

# 非终态、无 worker 的阻塞任务：计划始终被打回 → escalated
s, gate = req("POST", f"/projects/{pid}/tasks", {
    "title": "GATE", "description": "[reject_plan_always] blocked",
    "executor_agent_id": ex["id"], "acceptor_agent_id": acc["id"],
})
gate = wait(lambda: (task_by_title(pid, "GATE") or {}).get("status") == "escalated" and task_by_title(pid, "GATE"), 30)
check("gate task escalated (non-terminal, no worker)", gate is not None, task_by_title(pid, "GATE"))

# 「为什么还没开始」+ 队列人工调序
s, blocked = req("POST", f"/projects/{pid}/tasks", {
    "title": "等待依赖任务", "description": "blocked", "depends_on": [gate["id"]],
    "executor_agent_id": ex["id"], "acceptor_agent_id": acc["id"],
})
bid = (blocked or {}).get("id", "")
time.sleep(0.5)
bt = next((t for t in tasks(pid) if t.get("id") == bid), None)
check("planned task explains why not started", bool((bt or {}).get("waiting_reason")) and "等待依赖" in (bt.get("waiting_reason") or ""), bt)
s, pr = req("PATCH", f"/tasks/{bid}", {"queue_priority": 7})
check("task queue priority set", s == 200 and (pr or {}).get("queue_priority") == 7, (s, pr))
first = tasks(pid)[0] if tasks(pid) else {}
check("task list honors queue priority", first.get("id") == bid, first.get("title"))

# ---- 批准路径 ----
s, rep = req("POST", f"/tasks/{gate['id']}/replan", {
    "reason": "拆分遗漏了准备步骤", "reported_by": "executor",
    "proposed_changes": [{"op": "add", "task": {"title": "准备A", "description": "准备", "depends_on": [],
                                                 "acceptance_required": True, "acceptance_criteria": "准备完成"}}],
})
check("report accepted", s == 200 and (rep.get("inbox") or {}).get("id"), rep)
item = (rep.get("inbox") or {})
check("open replan item in inbox", isinstance(item.get("id"), str) and (open_replan(pid) or {}).get("id") == item.get("id"), item)
s, r = req("POST", f"/inbox/{item.get('id')}/resolve", {"resolution": "approve", "note": "人工批准"})
check("human approve accepted", s == 200, (s, r))
item2 = wait(lambda: next((i for i in inbox() if i.get("id") == item.get("id")
                           and (i.get("payload") or {}).get("planning") == "applied"), None), 40)
check("planning applied", item2 is not None, next((i for i in inbox() if i.get("id") == item.get("id")), None))
new_a = wait(lambda: task_by_title(pid, "准备A"), 20)
check("new task landed", new_a is not None)
et = event_types(pid)
check("replan approved/started/applied events", all(x in et for x in ("replan.approved", "replan.planning_started", "replan.applied")), et)
check("no decision/negotiation events", not any(x.startswith(("decision.", "negotiation.")) for x in et), et)

# ---- 否决路径 ----
before = len(tasks(pid))
s, rep = req("POST", f"/tasks/{gate['id']}/replan", {"reason": "这次请求应被否决", "reported_by": "executor"})
item_d = (rep.get("inbox") or {})
s, r = req("POST", f"/inbox/{item_d.get('id')}/resolve", {"resolution": "deny", "note": "不需要"})
check("human deny accepted", s == 200, (s, r))
time.sleep(0.5)
check("deny creates no task", len(tasks(pid)) == before, (before, len(tasks(pid))))
check("replan.rejected event", "replan.rejected" in event_types(pid))

# ---- 暂停时批准：不派发，恢复后落地 ----
req("POST", f"/projects/{pid}/pause", {})
s, rep = req("POST", f"/tasks/{gate['id']}/replan", {"reason": "暂停期间的上报", "reported_by": "executor"})
item_p = (rep.get("inbox") or {})
req("POST", f"/inbox/{item_p.get('id')}/resolve", {"resolution": "approve", "note": "暂停期间批准"})
time.sleep(1.0)
evs = req("GET", f"/events?project_id={pid}")[1] or []
check("paused approve does not dispatch planning", not any(
    e.get("type") == "replan.planning_started" and (e.get("payload") or {}).get("inbox_id") == item_p.get("id") for e in evs))
check("paused approve keeps planning pending", any(
    i.get("id") == item_p.get("id") and (i.get("payload") or {}).get("planning") == "pending" for i in inbox()))
req("POST", f"/projects/{pid}/resume", {})
item_p2 = wait(lambda: next((i for i in inbox() if i.get("id") == item_p.get("id")
                             and (i.get("payload") or {}).get("planning") == "applied"), None), 40)
check("resume applies pending plan", item_p2 is not None, next((i for i in inbox() if i.get("id") == item_p.get("id")), None))

failed = [n for n, ok in CHECKS if not ok]
print(f"\n[replan-smoke] {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
sys.exit(1 if failed else 0)
PY
