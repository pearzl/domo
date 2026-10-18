#!/usr/bin/env bash
# M3 里程碑 gate 确认/退回 + 里程碑窗口重规划 —— 端到端验收冒烟（mock agents）。
#
# 覆盖本任务的验收标准：
#   1. gate 确认：里程碑内全部任务 DONE → awaiting_confirm + 收件箱 milestone_gate；
#      确认通过 → 里程碑 passed，下游跨里程碑依赖分支自动恢复派发并跑完（DONE）；
#   2. gate 退回：收件箱 milestone_gate 退回 → 里程碑 failed + 重开「里程碑窗口」
#      （一条 kind=replan / payload.kind=milestone_window 的收件箱项），下游保持冻结；
#   3. 窗口内重规划：追加的新任务默认 acceptance_required=true、默认挂到该里程碑、
#      被调度执行到 DONE；依赖按任务标题解析（既有任务 + 同批新增）；
#   4. 环校验：故意构造的成环依赖调整被拒绝且给出原因（skipped），非成环调整正常应用；
#   5. 收件箱：gate 项在退回后变为 resolved；窗口项在应用后变为 resolved；
#   6. 窗口内成员全部完成后里程碑重新进入 awaiting_confirm，人工确认后下游解挂续跑；
#   7. 审计：milestone.window_opened / milestone.window_applied / task.created(source) /
#      task.updated(source) / milestone.passed / milestone.gate_opened 可完整回放。
#
# 用法：bash scripts/smoke_milestone_window.sh   （退出码 0 = 全部通过）
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_SMOKE_PORT:-8793}"
TMP_ROOT="${TMPDIR:-/tmp}"
RUN_DIR="$(mktemp -d "$TMP_ROOT/domo_mst_window_smoke.XXXXXX")"
DB="$RUN_DIR/domo.db"
WS="$RUN_DIR/workspace"
LOG="$RUN_DIR/server.log"
mkdir -p "$WS"

cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "[smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[smoke] build failed"; exit 1; }

echo "[smoke] starting server on :$PORT (db $DB)"
(
  cd "$ROOT/server"
  DOMO_DB="$DB" DOMO_PORT="$PORT" DOMO_LOG="$LOG" DOMO_LOG_LEVEL=info \
    exec "$ROOT/server/target/debug/domo-server"
) >"$RUN_DIR/stdout.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 60); do
  if curl -sf "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1; then break; fi
  sleep 0.25
done
if ! curl -sf "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1; then
  echo "[smoke] server did not come up; log:"; tail -30 "$RUN_DIR/stdout.log"
  exit 1
fi

python3 - "$PORT" "$WS" <<'PY'
import json, re, sys, time, urllib.request, urllib.error

PORT, WS = sys.argv[1], sys.argv[2]
BASE = f"http://127.0.0.1:{PORT}/api"
CHECKS = []

def check(name, cond, detail=""):
    cond = bool(cond)
    CHECKS.append((name, cond))
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  -- " + str(detail)) if (detail and not cond) else ""), flush=True)

def req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(BASE + path, data=data, method=method,
                               headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=25) as resp:
            return resp.status, json.load(resp)
    except urllib.error.HTTPError as e:
        try:
            payload = json.load(e)
        except Exception:
            payload = {}
        return e.code, payload
    except Exception as e:
        return 0, {"error": str(e)}

def post(p, b=None): return req("POST", p, b)
def get(p): return req("GET", p)

def get_raw(path):
    """Fetch a text resource (SPA index / bundle) without JSON parsing."""
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{PORT}{path}", timeout=15) as resp:
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, ""
    except Exception:
        return 0, ""

def wait_until(fn, timeout=45, interval=0.3):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(interval)
    return None

def project(pid):
    s, b = get(f"/projects/{pid}")
    return b if s == 200 else {}

def tasks_of(pid): return project(pid).get("tasks", [])
def milestones_of(pid): return project(pid).get("milestones", [])
def task_by_id(pid, tid): return next((t for t in tasks_of(pid) if t["id"] == tid), None)
def task_by_title(pid, title): return next((t for t in tasks_of(pid) if t["title"] == title), None)
def milestone(pid, mid): return next((m for m in milestones_of(pid) if m["id"] == mid), None)

def events(pid=None):
    s, b = get(f"/events?project_id={pid}" if pid else "/events")
    return b if s == 200 else []
def event_types(pid=None): return [e["type"] for e in events(pid)]
def event_payloads(pid, typ):
    return [e.get("payload", {}) for e in events(pid) if e["type"] == typ]

def inbox():
    s, b = get("/inbox")
    return b if s == 200 else []
def inbox_item(iid):
    return next((i for i in inbox() if i["id"] == iid), None)
def open_gate(pid, mid):
    return next((i for i in inbox() if i.get("status") == "open" and i.get("type") == "milestone_gate"
                 and i.get("payload", {}).get("milestone_id") == mid), None)
def open_window(pid, mid):
    return next((i for i in inbox() if i.get("status") == "open" and i.get("type") == "replan"
                 and i.get("payload", {}).get("kind") == "milestone_window"
                 and i.get("payload", {}).get("milestone_id") == mid), None)

# ---------------------------------------------------------------- setup
print("[smoke] setup: agents + project")
s, ex = post("/agents", {"name": "exec", "adapter": "mock", "roles": ["executor"],
                         "capabilities": {"max_parallel": 5}, "config": {"duration_ms": 100}})
s, acc = post("/agents", {"name": "acc", "adapter": "mock", "roles": ["acceptor"],
                          "capabilities": {"max_parallel": 5}, "config": {"duration_ms": 100}})
check("agents registered (exec/acceptor)", all(a.get("id") for a in (ex, acc)), (ex, acc))

s, proj = post("/projects", {"name": "里程碑窗口冒烟", "brief": "M3 milestone gate + replan window"})
pid = proj.get("id")
req("PATCH", f"/projects/{pid}", {"workspace": {"kind": "local", "path": WS}})
post(f"/projects/{pid}/confirm", {})
check("project running", project(pid).get("project", {}).get("status") == "running", project(pid).get("project"))

def mk_milestone(name, goal):
    s, m = post(f"/projects/{pid}/milestones", {"name": name, "goal": goal, "effect": "ready"})
    return m

def mk_task(title, milestone_id=None, deps=None, desc="", acceptance=True, executor=None):
    body = {"title": title, "description": desc, "executor_agent_id": (executor or ex)["id"],
            "acceptor_agent_id": acc["id"], "acceptance_required": acceptance, "depends_on": deps or []}
    if milestone_id: body["milestone_id"] = milestone_id
    s, t = post(f"/projects/{pid}/tasks", body)
    return s, t

# ---------------------------------------------------------------- 1) confirm → downstream resumes
print("[smoke] 1) confirm: gate pass releases the downstream branch")
M1 = mk_milestone("M1-确认路径", "first phase")
s, A = mk_task("M1-A", milestone_id=M1["id"])
s, B = mk_task("M1-B", deps=[A["id"]])  # cross-milestone downstream of M1
check("M1 tasks created (A member, B downstream)", bool(A.get("id")) and bool(B.get("id")), (A, B))
g1 = wait_until(lambda: open_gate(pid, M1["id"]))
check("M1 gate opened once member tasks are done", g1 is not None, milestone(pid, M1["id"]))
check("M1 status awaiting_confirm", (milestone(pid, M1["id"]) or {}).get("status") == "awaiting_confirm",
      milestone(pid, M1["id"]))
check("M1 milestone.gate_opened audit event",
      any(p.get("milestone_id") == M1["id"] for p in event_payloads(pid, "milestone.gate_opened")))
time.sleep(1.0)
check("downstream B frozen while the gate is open",
      (task_by_id(pid, B["id"]) or {}).get("status") in ("planned", "ready"),
      (task_by_id(pid, B["id"]) or {}).get("status"))
s, _ = post(f"/inbox/{g1['id']}/resolve", {"resolution": "pass", "note": "人工确认 M1"})
check("gate confirmed (200)", s == 200, s)
check("M1 passed", (milestone(pid, M1["id"]) or {}).get("status") == "passed", milestone(pid, M1["id"]))
check("milestone.passed audit event", "milestone.passed" in event_types(pid))
check("gate inbox item resolved", (inbox_item(g1["id"]) or {}).get("status") == "resolved",
      inbox_item(g1["id"]))
b_done = wait_until(lambda: (task_by_id(pid, B["id"]) or {}).get("status") == "done")
check("downstream B auto-resumed and completed after confirm", b_done is not None,
      (task_by_id(pid, B["id"]) or {}).get("status"))
check("confirm path never opens a replanning window", open_window(pid, M1["id"]) is None)

# ---------------------------------------------------------------- 2) reject → window opens
print("[smoke] 2) reject: gate fail → milestone failed + replanning window opened")
M2 = mk_milestone("M2-退回路径", "second phase")
s, C = mk_task("M2-C", milestone_id=M2["id"])
s, D = mk_task("M2-D", deps=[C["id"]])  # downstream of M2, stays frozen
check("M2 tasks created (C member, D downstream)", bool(C.get("id")) and bool(D.get("id")), (C, D))
g2 = wait_until(lambda: open_gate(pid, M2["id"]))
check("M2 gate opened", g2 is not None, milestone(pid, M2["id"]))
s, res = post(f"/inbox/{g2['id']}/resolve", {"resolution": "reject", "note": "退回：需要补充准备任务"})
check("gate rejected (200)", s == 200, (s, res))
check("M2 failed after reject", (milestone(pid, M2["id"]) or {}).get("status") == "failed",
      milestone(pid, M2["id"]))
check("milestone.failed audit event", "milestone.failed" in event_types(pid))
check("gate item resolved after reject", (inbox_item(g2["id"]) or {}).get("status") == "resolved",
      inbox_item(g2["id"]))
win = wait_until(lambda: open_window(pid, M2["id"]), 15)
check("milestone replanning window opened (open replan inbox item)", win is not None, inbox())
check("milestone.window_opened audit event", "milestone.window_opened" in event_types(pid))
time.sleep(1.0)
check("downstream D stays frozen while M2 is failed",
      (task_by_id(pid, D["id"]) or {}).get("status") in ("planned", "ready"),
      (task_by_id(pid, D["id"]) or {}).get("status"))

# ---------------------------------------------------------------- 3) apply window changes
print("[smoke] 3) window: append tasks (default acceptance) + adjust downstream planning")
# probe: applying with no open window must 409 (M1 already passed, has no window)
s, probe = post(f"/milestones/{M1['id']}/window/apply", {"changes": []})
check("window apply rejected without an open window (409)", s == 409, (s, probe))

changes = [
    # valid add: default acceptance_required=true, becomes a member of M2
    {"op": "add", "task": {"title": "窗口-补做", "description": "补齐准备步骤", "depends_on": [],
                           "acceptance_criteria": "准备产物可检验"}},
    # valid add
    {"op": "add", "task": {"title": "窗口-环A", "description": "环用例A", "depends_on": []}},
    # valid add depending on a same-batch task, resolved by title
    {"op": "add", "task": {"title": "窗口-环B", "description": "环用例B", "depends_on": ["窗口-环A"]}},
    # valid non-cyclic adjustment of downstream planning (D is non-terminal)
    {"op": "update", "task_id": D["id"], "fields": {"description": "下游任务：按窗口结果复核"}},
]
s, applied = post(f"/milestones/{M2['id']}/window/apply", {"changes": changes, "note": "追加准备任务并调整下游"})
check("window apply accepted (200)", s == 200, (s, applied))
check("all 4 window changes applied, none skipped",
      len(applied.get("applied", [])) == 4 and applied.get("skipped") == [], applied)
check("window item resolved after apply", (inbox_item(win["id"]) or {}).get("status") == "resolved",
      inbox_item(win["id"]))
check("milestone.window_applied audit event", "milestone.window_applied" in event_types(pid))

task_a = wait_until(lambda: task_by_title(pid, "窗口-补做"), 15)
task_ringa = wait_until(lambda: task_by_title(pid, "窗口-环A"), 15)
task_ringb = wait_until(lambda: task_by_title(pid, "窗口-环B"), 15)
check("window tasks landed in the project",
      all(t is not None for t in (task_a, task_ringa, task_ringb)), (task_a, task_ringa, task_ringb))
if task_a:
    check("appended task defaults acceptance_required=true", task_a.get("acceptance_required") is True,
          task_a.get("acceptance_required"))
    check("appended task belongs to the rejected milestone", task_a.get("milestone_id") == M2["id"],
          task_a.get("milestone_id"))
if task_ringb and task_ringa:
    check("same-batch dependency resolved by title (B depends on A)",
          task_ringb.get("depends_on") == [task_ringa["id"]],
          (task_ringb.get("depends_on"), task_ringa["id"]))
check("non-cyclic update applied to downstream task",
      (task_by_id(pid, D["id"]) or {}).get("description") == "下游任务：按窗口结果复核",
      (task_by_id(pid, D["id"]) or {}).get("description"))
created_src = event_payloads(pid, "task.created")
check("task.created audit carries source=milestone_window",
      any(p.get("source") == "milestone_window" and p.get("title") == "窗口-补做" for p in created_src),
      [p.get("source") for p in created_src][-8:])
updated_src = event_payloads(pid, "task.updated")
check("task.updated audit carries source=milestone_window",
      any(p.get("source") == "milestone_window" for p in updated_src), updated_src[-4:])

# ---------------------------------------------------------------- 4) window tasks run + re-gate
print("[smoke] 4) window tasks execute, then milestone re-enters awaiting_confirm")
done_a = wait_until(lambda: (task_by_title(pid, "窗口-补做") or {}).get("status") == "done")
check("appended window task executed to done", done_a is not None,
      (task_by_title(pid, "窗口-补做") or {}).get("status"))
regated = wait_until(lambda: (milestone(pid, M2["id"]) or {}).get("status") == "awaiting_confirm", 60)
check("M2 re-enters awaiting_confirm after the window is worked", regated is not None,
      milestone(pid, M2["id"]))
g2b = wait_until(lambda: open_gate(pid, M2["id"]), 15)
check("a fresh M2 gate item is presented", g2b is not None, inbox())
if g2b:
    post(f"/inbox/{g2b['id']}/resolve", {"resolution": "pass", "note": "窗口后确认 M2"})
check("M2 passed after window work", (milestone(pid, M2["id"]) or {}).get("status") == "passed",
      milestone(pid, M2["id"]))
d_done = wait_until(lambda: (task_by_id(pid, D["id"]) or {}).get("status") == "done")
check("M2 downstream D auto-resumed and completed after confirm", d_done is not None,
      (task_by_id(pid, D["id"]) or {}).get("status"))

# ---------------------------------------------------------------- 5) cycle guard
print("[smoke] 5) cycle guard: a cyclic dependency adjustment is refused with a reason; the rest applies")
M4 = mk_milestone("M4-环校验", "cycle")
s, P = mk_task("M4-P", milestone_id=M4["id"])
s, Q = mk_task("M4-Q", deps=[P["id"]])
# a slow executor (registered only now, so it does not disturb the earlier window's
# executor rotation) keeps a couple of probe tasks non-terminal for the cycle guard.
s, slow = post("/agents", {"name": "slow", "adapter": "mock", "roles": ["executor"],
                           "capabilities": {"max_parallel": 5}, "config": {"duration_ms": 300000}})
check("slow executor registered for the cycle probe", bool(slow.get("id")), slow)
# two live (non-terminal) probe tasks: R1 runs long on the slow executor, R2 waits on R1.
s, R1 = mk_task("M4-探针1", desc="long running probe", executor=slow)
s, R2 = mk_task("M4-探针2", deps=[R1["id"]], desc="waits on probe1", executor=slow)
r1_live = wait_until(lambda: (task_by_id(pid, R1["id"]) or {}).get("status") == "executing", 20)
check("cycle probe task is live (non-terminal)", r1_live is not None,
      (task_by_id(pid, R1["id"]) or {}).get("status"))
g4 = wait_until(lambda: open_gate(pid, M4["id"]))
check("M4 gate opened", g4 is not None, milestone(pid, M4["id"]))
post(f"/inbox/{g4['id']}/resolve", {"resolution": "reject", "note": "退回做环校验"})
win4 = wait_until(lambda: open_window(pid, M4["id"]), 15)
check("M4 window opened", win4 is not None, inbox())
r1_deps_before = (task_by_id(pid, R1["id"]) or {}).get("depends_on")
r2_deps_before = (task_by_id(pid, R2["id"]) or {}).get("depends_on")
s, r4 = post(f"/milestones/{M4['id']}/window/apply", {"changes": [
    # deps are given by task title: R1 -> R2 -> R1 closes a cycle and must be refused
    {"op": "update", "task_id": R1["id"], "fields": {"depends_on": ["M4-探针2"]}},
    {"op": "add", "task": {"title": "M4-新增", "description": "非成环新增", "depends_on": ["M4-P"]}},
], "note": "成环拒绝、非成环应用"})
check("M4 window apply accepted (200)", s == 200, (s, r4))
applied4 = r4.get("applied", [])
skipped4 = r4.get("skipped", [])
check("cycle adjustment skipped with an explicit reason",
      len(skipped4) == 1 and "cycle" in json.dumps(skipped4).lower(), skipped4)
check("non-cyclic adjustment applied alongside the rejection",
      len(applied4) == 1 and applied4[0].get("op") == "add", applied4)
check("cyclic task dependency left unchanged (no partial write)",
      (task_by_id(pid, R1["id"]) or {}).get("depends_on") == r1_deps_before,
      (task_by_id(pid, R1["id"]) or {}).get("depends_on"))
check("dependent task dependency unchanged",
      (task_by_id(pid, R2["id"]) or {}).get("depends_on") == r2_deps_before,
      (task_by_id(pid, R2["id"]) or {}).get("depends_on"))
m4_new = wait_until(lambda: task_by_title(pid, "M4-新增"), 15)
check("non-cyclic appended task landed and is a member", m4_new is not None
      and m4_new.get("milestone_id") == M4["id"], m4_new)

# ---------------------------------------------------------------- 6) audit replay
print("[smoke] 6) audit — the append + adjustment chain is fully replayable")
types = set(event_types(pid))
for ev in ["milestone.gate_opened", "milestone.passed", "milestone.failed",
           "milestone.window_opened", "milestone.window_applied"]:
    check(f"audit event present: {ev}", ev in types)
w_applied = event_payloads(pid, "milestone.window_applied")
check("window_applied payload records applied + skipped",
      any("applied" in p and "skipped" in p for p in w_applied), w_applied[:1])
win_res = inbox_item(win["id"]) if win else None
check("window inbox resolution records applied/skipped",
      win_res is not None and isinstance(win_res.get("resolution"), dict)
      and "applied" in win_res["resolution"], win_res and win_res.get("resolution"))

# ---------------------------------------------------------------- 7) frontend operability
print("[smoke] 7) frontend — the shipped bundle carries the milestone-window inbox operations")
s_html, html = get_raw("/")
check("GET / serves the built SPA", s_html == 200 and 'id="root"' in html, s_html)
bundle = ""
for p in re.findall(r'src="([^"]+\.js)"', html):
    st, txt = get_raw(p if p.startswith("/") else "/" + p)
    if st == 200:
        bundle += txt
for marker, label in [("里程碑窗口", "窗口条目专属操作区"),
                      ("应用窗口方案", "窗口方案应用按钮"),
                      ("调整后续规划", "窗口内调整后续规划"),
                      ("待应用方案", "逐项待应用方案列表")]:
    check(f"bundle 含「{label}」标记：{marker}", marker in bundle)

failed = [n for n, ok in CHECKS if not ok]
print()
print(f"[smoke] {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
if failed:
    print("[smoke] FAILED:")
    for n in failed:
        print("  - " + n)
    sys.exit(1)
print("[smoke] ALL PASS")
PY
RC=$?

if [[ $RC -ne 0 ]]; then
  echo "[smoke] server log tail:"
  tail -40 "$LOG" 2>/dev/null || true
fi
exit $RC
