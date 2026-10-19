#!/usr/bin/env bash
# M3 动态重规划「常规纠偏」通道 —— 端到端冒烟（mock agent）。
#
# 对着真实后端核对验收标准：
#   1. 批准路径：executor 上报规划问题 → 决策者批准 → 规划落地 → 新任务出现在看板、
#      进入调度，且默认带可检验验收标准；
#   2. 拒绝路径：决策者否决 → 请求关闭、写 replan.rejected、原任务流程继续到 done，
#      不产生任何新任务；
#   3. 环校验：调整项若给已有任务引入依赖环 → 该项被跳过并给出 cycle 原因，
#      其余项照常落地，DAG 保持无环；
#   4. 战略升级：触及里程碑的重规划落库即 escalated，进入人工收件箱（决策者不得自决）；
#      人工批准后同样走规划落地；
#   5. 审计可回放：replan.proposed / replan.approved / replan.rejected / replan.applied
#      全部出现在 GET /api/events；
#   6. 幂等：同一任务已有未决重规划请求时重复上报返回 deduplicated=true。
#
# 用法：bash scripts/smoke_replan.sh   （退出码 0 = 全部通过）
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_REPLAN_SMOKE_PORT:-8796}"
TMP_ROOT="${TMPDIR:-/tmp}"
RUN_DIR="$(mktemp -d "$TMP_ROOT/domo_replan_smoke.XXXXXX")"
DB="$RUN_DIR/domo.db"
WS="$RUN_DIR/workspace"
LOG="$RUN_DIR/server.log"
mkdir -p "$WS"

echo "[replan-smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[replan-smoke] server build failed"; exit 1; }

if [[ ! -f "$ROOT/web/dist/index.html" ]]; then
  echo "[replan-smoke] building web (no dist)…"
  (cd "$ROOT/web" && npm install --silent && npm run build --silent) || { echo "[replan-smoke] web build failed"; exit 1; }
fi

echo "[replan-smoke] running driver (port $PORT, db $DB)"
python3 - "$PORT" "$DB" "$WS" "$LOG" "$ROOT" <<'PY'
import atexit, json, os, signal, subprocess, sys, time, urllib.request, urllib.error

PORT, DB, WS, LOG, ROOT = sys.argv[1:6]
BASE = f"http://127.0.0.1:{PORT}/api"
SERVER_BIN = os.path.join(ROOT, "server", "target", "debug", "domo-server")

CHECKS = []

def check(name, cond, detail=""):
    cond = bool(cond)
    CHECKS.append((name, cond))
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  -- " + str(detail)) if (detail and not cond) else ""), flush=True)

def req(method, path, body=None, timeout=60):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(BASE + path, data=data, method=method,
                               headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as e:
        try: payload = json.loads(e.read().decode())
        except Exception: payload = {}
        return e.code, payload
    except Exception as e:
        return 0, {"error": str(e)}

def get(p, timeout=60): return req("GET", p, timeout=timeout)
def post(p, b=None, timeout=60): return req("POST", p, b if b is not None else {}, timeout=timeout)
def patch(p, b, timeout=60): return req("PATCH", p, b, timeout=timeout)

def wait_until(fn, timeout=30, interval=0.3):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(interval)
    return None

def start_server():
    env = dict(os.environ, DOMO_DB=DB, DOMO_PORT=PORT, DOMO_LOG=LOG, DOMO_LOG_LEVEL="warn")
    with open(os.path.join(os.path.dirname(DB), "stdout.log"), "ab") as out:
        return subprocess.Popen([SERVER_BIN], env=env, stdout=out, stderr=subprocess.STDOUT,
                                cwd=os.path.dirname(ROOT))

def kill9(proc):
    proc.send_signal(signal.SIGKILL)
    try: proc.wait(timeout=10)
    except Exception: pass

SERVER = start_server()
atexit.register(lambda: kill9(SERVER))
if not wait_until(lambda: get("/agents")[0] == 200, 30):
    print("[replan-smoke] server did not come up; see", LOG)
    sys.exit(1)

# ---------- helpers ----------
def events(pid):
    s, b = get(f"/events?project_id={pid}")
    return b if s == 200 else []

def etypes(pid):
    return [e["type"] for e in events(pid)]

def ev_with(pid, typ):
    for e in events(pid):
        if e["type"] == typ:
            return e
    return None

def tasks(pid):
    s, b = get(f"/projects/{pid}")
    return b.get("tasks", []) if s == 200 else []

def task_by_title(pid, title):
    return next((t for t in tasks(pid) if t["title"] == title), None)

def task_detail(tid):
    s, b = get(f"/tasks/{tid}")
    return b.get("task", b) if s == 200 else {}

def decision_state(did):
    # 决策没有独立 GET；从项目事件与收件箱间接判断。这里用上报返回的 id 去收件箱/事件核对。
    for e in events_all():
        if e.get("payload", {}).get("decision_id") == did:
            pass
    return None

def events_all():
    s, b = get("/events")
    return b if s == 200 else []

def find_event(pid, typ, pred=lambda p: True):
    for e in events(pid):
        if e["type"] == typ and pred(e.get("payload") or {}):
            return e
    return None

def inbox_open(pid=None, typ=None):
    s, b = get("/inbox")
    if s != 200: return []
    out = [i for i in b if i.get("status") == "open"]
    if pid: out = [i for i in out if i.get("project_id") == pid]
    if typ: out = [i for i in out if i.get("type") == typ]
    return out

def new_project(name):
    s, p = post("/projects", {"name": name, "brief": "replan smoke"})
    assert s == 200, p
    pid = p["id"]
    patch(f"/projects/{pid}", {"workspace": {"kind": "local", "path": WS}})
    post(f"/projects/{pid}/confirm", {})
    return pid

def add_task(pid, title, desc="", deps=None, milestone=None, executor=None, acceptor=None,
             criteria="可独立复核的验收标准"):
    body = {
        "title": title,
        "description": desc,
        "depends_on": deps or [],
        "acceptance_required": True,
        "acceptance_criteria": criteria,
        "negotiation_status": "agreed",
    }
    if milestone: body["milestone_id"] = milestone
    if executor: body["executor_agent_id"] = executor
    if acceptor: body["acceptor_agent_id"] = acceptor
    s, t = post(f"/projects/{pid}/tasks", body)
    assert s == 200, (title, t)
    return t["id"]

def report(tid, body):
    return post(f"/tasks/{tid}/replan", body)

def wait_ruled(pid, did, decision="approve", timeout=20):
    def probe():
        ev = find_event(pid, "decision.ruled", lambda p: p.get("decision_id") == did)
        if not ev: return None
        return ev if ev["payload"].get("decision") == decision else None
    return wait_until(probe, timeout)

# ---------- agents ----------
s, ex = post("/agents", {"name": "exec", "adapter": "mock", "roles": ["executor"],
                         "capabilities": {"max_parallel": 6, "supports_resume": False},
                         "config": {"duration_ms": 2500}})
s, ac = post("/agents", {"name": "acc", "adapter": "mock", "roles": ["acceptor"],
                         "capabilities": {"max_parallel": 6, "supports_resume": False},
                         "config": {"duration_ms": 400}})
s, dm = post("/agents", {"name": "dm", "adapter": "mock", "roles": ["decision_maker"],
                         "capabilities": {"max_parallel": 2, "supports_resume": False},
                         "config": {"duration_ms": 200}})
# 环场景需要任务在规划落地期间保持非终态
s, slow = post("/agents", {"name": "exec-slow", "adapter": "mock", "roles": ["executor"],
                           "capabilities": {"max_parallel": 6, "supports_resume": False},
                           "config": {"duration_ms": 9000}})
EXEC, ACC, DM, SLOW = ex.get("id"), ac.get("id"), dm.get("id"), slow.get("id")
check("registered mock executor/acceptor/decision_maker", all([EXEC, ACC, DM, SLOW]))

# ================================================================ 1) approve
print("\n1) tactical approve → new task lands, scheduled, with acceptance criteria")
P1 = new_project("replan approve")
T1 = add_task(P1, "Tactical base", "executing baseline", executor=EXEC, acceptor=ACC)

s, r1 = report(T1, {
    "reason": "依赖顺序不合理，需要补充一条前置梳理任务",
    "detail": "baseline 与后续任务之间缺少需求梳理步骤",
    "reported_by": "executor",
    "proposed_changes": [{
        "op": "add",
        "task": {
            "title": "Replan added A",
            "description": "由重规划补充的需求梳理任务",
            "depends_on": [],
            "acceptance_required": True,
            "acceptance_criteria": "产出需求要点清单并通过验收",
        },
    }],
})
D1 = (r1.get("decision") or {}).get("id", "")
check("report accepted as tactical", s == 200 and r1.get("scope") == "tactical", r1)
check("decision registered pending", (r1.get("decision") or {}).get("status") == "pending", r1)

# 幂等：未决期间重复上报不应新建决策
s, r1b = report(T1, {"reason": "重复上报", "reported_by": "executor"})
check("duplicate report is deduplicated", s == 200 and r1b.get("deduplicated") is True
      and (r1b.get("decision") or {}).get("id") == D1, r1b)

check("decision maker approved", bool(wait_ruled(P1, D1, "approve")), "no approve ruling")
check("replan.approved audited", bool(find_event(P1, "replan.approved",
      lambda p: p.get("decision_id") == D1)))

app_ev = wait_until(lambda: find_event(P1, "replan.applied",
                    lambda p: p.get("decision_id") == D1), 30)
check("replan.applied audited", bool(app_ev), "planning never applied")
nt = wait_until(lambda: task_by_title(P1, "Replan added A"), 20)
check("new task visible on the board", bool(nt), [t["title"] for t in tasks(P1)])
if nt:
    check("new task carries acceptance criteria", bool(nt.get("acceptance_criteria")), nt)
    sched = wait_until(lambda: (task_detail(nt["id"]).get("status") in ("ready", "executing", "done")), 20)
    check("new task entered scheduling", bool(sched), task_detail(nt["id"]).get("status"))

check("replan.planning_started audited", bool(find_event(P1, "replan.planning_started",
      lambda p: p.get("decision_id") == D1)))

# ================================================================ 2) reject
print("\n2) reject path → request closed, original task keeps flowing, no new tasks")
P2 = new_project("replan reject")
T2 = add_task(P2, "Reject base", "baseline that must keep running", executor=EXEC, acceptor=ACC)
before = {t["title"] for t in tasks(P2)}

s, r2 = report(T2, {
    "reason": "[reject] 这个规划问题不成立，请不要调整",
    "detail": "e2e rejection branch",
    "reported_by": "acceptor",
    "proposed_changes": [{
        "op": "add",
        "task": {"title": "Should not exist", "acceptance_required": True,
                 "acceptance_criteria": "never created"},
    }],
})
D2 = (r2.get("decision") or {}).get("id", "")
check("report registered for rejection", s == 200 and bool(D2), r2)
check("decision maker denied", bool(wait_ruled(P2, D2, "deny")), "no deny ruling")
check("replan.rejected audited", bool(find_event(P2, "replan.rejected",
      lambda p: p.get("decision_id") == D2)))
check("no replan.approved for rejected request",
      not find_event(P2, "replan.approved", lambda p: p.get("decision_id") == D2))
check("no planning started for rejected request",
      not find_event(P2, "replan.planning_started", lambda p: p.get("decision_id") == D2))

# 原任务流程继续（不受拒绝影响）
done2 = wait_until(lambda: task_detail(T2).get("status") == "done", 30)
check("original task still reaches done", bool(done2), task_detail(T2).get("status"))
after = {t["title"] for t in tasks(P2)}
check("rejected request created no tasks",
      "Should not exist" not in after and not any(t.startswith("重规划补充：") for t in after),
      sorted(after - before))
logs2 = " ".join(l.get("line", "") for l in task_detail(T2).get("logs", []))
check("reporter notified about rejection", "拒绝" in logs2, logs2[-300:])

# ================================================================ 3) cycle
print("\n3) cycle guard → cyclic adjustment skipped, DAG stays acyclic")
P3 = new_project("replan cycle")
TA = add_task(P3, "Cycle A", "slow task A", executor=SLOW, acceptor=ACC)
TB = add_task(P3, "Cycle B", "slow task B", executor=SLOW, acceptor=ACC)

s, r3 = report(TB, {
    "reason": "尝试调整两个任务的前后依赖",
    "reported_by": "executor",
    "proposed_changes": [
        {"op": "update", "task_id": TA, "fields": {"depends_on": ["Cycle B"]}},
        {"op": "update", "task_id": TB, "fields": {"depends_on": ["Cycle A"]}},
    ],
})
D3 = (r3.get("decision") or {}).get("id", "")
check("cycle scenario decision registered", s == 200 and bool(D3), r3)
check("cycle scenario approved", bool(wait_ruled(P3, D3, "approve")), "no approve ruling")
app3 = wait_until(lambda: find_event(P3, "replan.applied",
                    lambda p: p.get("decision_id") == D3), 30)
check("cycle scenario applied pass ran", bool(app3))
if app3:
    applied = app3["payload"].get("applied") or []
    skipped = app3["payload"].get("skipped") or []
    check("non-cyclic item applied", any(x.get("task_id") == TA for x in applied), applied)
    check("cyclic item skipped with cycle reason",
          any("cycle" in str(x.get("reason", "")).lower() for x in skipped), skipped)

tb = task_detail(TB)
ta = task_detail(TA)
check("cycled task kept its dependencies (no cycle persisted)", tb.get("depends_on") == [], tb.get("depends_on"))
check("DAG remains acyclic", tb["id"] not in (ta.get("depends_on") or [])
      or ta["id"] not in (tb.get("depends_on") or []),
      {"A": ta.get("depends_on"), "B": tb.get("depends_on")})

# ================================================================ 4) milestone → human
print("\n4) milestone-touching replan escalates to the human inbox")
P4 = new_project("replan milestone")
s, m = post(f"/projects/{P4}/milestones", {"name": "M1", "goal": "phase one", "effect": "ready"})
M1 = m.get("id")
T4 = add_task(P4, "Milestone base", "work under milestone", milestone=M1,
              executor=SLOW, acceptor=ACC)

s, r4 = report(T4, {
    "reason": "里程碑内的规划需要补充支撑任务",
    "detail": "strategic escalation branch",
    "reported_by": "executor",
})
D4 = (r4.get("decision") or {}).get("id", "")
check("milestone replan classified strategic", s == 200 and r4.get("scope") == "strategic", r4)
check("decision escalated (not self-ruled)", (r4.get("decision") or {}).get("status") == "escalated", r4)

item = wait_until(lambda: next((i for i in inbox_open(P4, "replan")
                                if (i.get("payload") or {}).get("decision_id") == D4), None), 10)
check("campaign appears in human inbox", bool(item), [i.get("type") for i in inbox_open(P4)])
check("no agent ruling before human action",
      not find_event(P4, "decision.ruled", lambda p: p.get("decision_id") == D4))

if item:
    s, _ = post(f"/inbox/{item['id']}/resolve", {"resolution": "approve", "note": "e2e human approval"})
    check("human approval accepted", s == 200, s)
    ruled4 = wait_ruled(P4, D4, "approve", 20)
    check("human ruling recorded", bool(ruled4) and ruled4["payload"].get("by") == "human", ruled4)
    app4 = wait_until(lambda: find_event(P4, "replan.applied",
                       lambda p: p.get("decision_id") == D4), 30)
    check("human-approved replan applied", bool(app4))
    fallback = wait_until(lambda: next((t for t in tasks(P4)
                                        if t["title"].startswith("重规划补充：")), None), 20)
    check("human-approved replan produced a new task", bool(fallback),
          [t["title"] for t in tasks(P4)])
    if fallback:
        check("human-approved new task carries acceptance criteria",
              bool(fallback.get("acceptance_criteria")), fallback)
        check("human-approved new task entered scheduling",
              bool(wait_until(lambda: task_detail(fallback["id"]).get("status") in
                              ("ready", "executing", "done"), 20)))
    check("human inbox item resolved",
          any(i["id"] == item["id"] and i.get("status") == "resolved" for i in get("/inbox")[1]))

# ================================================================ 5) audit replay
print("\n5) audit trail replay is complete")
all_types = set()
for p in (P1, P2, P3, P4):
    all_types.update(etypes(p))
for ev in ("replan.proposed", "replan.approved", "replan.rejected",
           "replan.applied", "decision.requested", "decision.ruled"):
    check(f"audit event present: {ev}", ev in all_types, sorted(all_types))
check("proposed/rejected/approved/applied are distinct, replayable events",
      {"replan.proposed", "replan.rejected", "replan.approved", "replan.applied"} <= all_types)

# ---------- summary ----------
failed = [n for n, ok in CHECKS if not ok]
print()
if failed:
    print(f"[replan-smoke] {len(failed)} check(s) FAILED:")
    for n in failed:
        print("  -", n)
    sys.exit(1)
print(f"[replan-smoke] ALL {len(CHECKS)} CHECKS PASSED")
PY

RC=$?
if [[ $RC -ne 0 ]]; then
  echo "[replan-smoke] failed; artifacts kept at $RUN_DIR"
  exit $RC
fi
echo "[replan-smoke] OK (artifacts: $RUN_DIR)"
