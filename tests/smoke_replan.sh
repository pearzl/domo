#!/usr/bin/env bash
# M3 动态重规划·决策者「常规纠偏」通道 —— 端到端验收冒烟（mock agents）。
#
# 覆盖本任务的五条验收标准：
#   1. 批准路径：executor/acceptor 上报规划问题 → replan.proposed → 决策者批准
#      （replan.approved）→ 复用规划链路产出调整 → 新任务带验收标准出现在任务列表
#      并被调度执行到 DONE；
#   2. 拒绝路径：决策者否决 → replan.rejected、请求关闭、通知上报者；原任务流程不受影响，
#      不产生任何新任务；
#   3. 环校验：上报的依赖调整若引入环 → 落地时被拒（replan.applied.skipped 记录原因，
#      任务依赖保持原样）；
#   4. 战略升级：触及里程碑/项目目标（任务挂里程碑）的请求不自动裁决，
#      落人工收件箱（type=replan）等人裁决；人工批准后才进入规划落地；
#   5. 可审计：replan.proposed/approved/rejected/planning_started/applied 与
#      decision.requested/ruled/escalated 事件全程可回放。
#
# 另含回归：暂停冻结「自动」规划派发——人工裁决立即生效（planning=pending），
# 但 planner 派发延后到恢复后续跑，与 exec/accept 派发的冻结语义一致。
#
# 用法：bash scripts/smoke_replan.sh   （退出码 0 = 全部通过）
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_SMOKE_PORT:-8792}"
RUN_DIR="$(mktemp -d /tmp/opencode/domo_replan_smoke.XXXXXX)"
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
import json, sys, time, urllib.request, urllib.error

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
        with urllib.request.urlopen(r, timeout=20) as resp:
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
def patch(p, b): return req("PATCH", p, b)
def get(p): return req("GET", p)

def wait_until(fn, timeout=30, interval=0.3):
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
def task_by_id(pid, tid): return next((t for t in tasks_of(pid) if t["id"] == tid), None)
def task_by_title(pid, title): return next((t for t in tasks_of(pid) if t["title"] == title), None)

def decisions(pid):
    s, b = get(f"/projects/{pid}/decisions")
    return b if s == 200 else []

def decision(pid, did):
    return next((d for d in decisions(pid) if d["id"] == did), None)

def events(pid=None):
    p = f"/events?project_id={pid}" if pid else "/events"
    s, b = get(p)
    return b if s == 200 else []

def event_types(pid=None): return [e["type"] for e in events(pid)]

def open_replan_inbox():
    s, b = get("/inbox")
    items = b if s == 200 else []
    return [i for i in items if i.get("status") == "open" and i.get("type") == "replan"]

def task_detail(tid):
    s, b = get(f"/tasks/{tid}")
    return b if s == 200 else {}

# ---------------------------------------------------------------- setup
print("[smoke] setup: agents + project")
s, ex = post("/agents", {"name": "exec", "adapter": "mock", "roles": ["executor"],
                         "capabilities": {"max_parallel": 5}, "config": {"duration_ms": 100}})
s, slow = post("/agents", {"name": "slow", "adapter": "mock", "roles": ["executor"],
                           "capabilities": {"max_parallel": 5}, "config": {"duration_ms": 300000}})
s, acc = post("/agents", {"name": "acc", "adapter": "mock", "roles": ["acceptor"],
                          "capabilities": {"max_parallel": 5}, "config": {"duration_ms": 100}})
s, dm = post("/agents", {"name": "judge", "adapter": "mock", "roles": ["decision_maker"],
                         "capabilities": {"max_parallel": 5}, "config": {"duration_ms": 50}})
check("agents registered (exec/slow/acc/decision_maker)",
      all(a.get("id") for a in (ex, slow, acc, dm)), (ex, slow, acc, dm))

s, proj = post("/projects", {"name": "重规划冒烟", "brief": "M3 replan tactical/strategic channel"})
pid = proj.get("id")
patch(f"/projects/{pid}", {"workspace": {"kind": "local", "path": WS}})
post(f"/projects/{pid}/confirm", {})
check("project running", project(pid).get("project", {}).get("status") == "running", project(pid).get("project"))

# a long-running task keeps reporting tasks non-terminal, so they stay reportable.
s, G = post(f"/projects/{pid}/tasks", {"title": "SLOW-GATE", "description": "stays executing",
                                       "executor_agent_id": slow["id"], "acceptor_agent_id": acc["id"]})
check("slow gate task created", s == 200 and G.get("id"), (s, G))

# ---------------------------------------------------------------- 1) approve (reporter changes)
print("[smoke] 1) tactical approve — reporter-proposed new task carries acceptance & gets scheduled")
s, R1 = post(f"/projects/{pid}/tasks", {"title": "R1", "description": "report target",
                                        "executor_agent_id": ex["id"], "acceptor_agent_id": acc["id"],
                                        "depends_on": [G["id"]]})
check("reporting task R1 created", s == 200 and R1.get("id"), (s, R1))
s, rep = post(f"/tasks/{R1['id']}/replan", {
    "reason": "依赖顺序不合理：执行前缺少环境准备步骤",
    "reported_by": "executor",
    "detail": "当前任务直接开工，缺少可复用的准备产物",
    "proposed_changes": [{
        "op": "add",
        "task": {"title": "重规划-准备A", "description": "补齐准备步骤", "depends_on": [],
                 "acceptance_required": True, "acceptance_criteria": "产出准备说明文件且可检验"},
    }],
})
check("A replan report accepted (200)", s == 200 and rep.get("decision", {}).get("id"), (s, rep))
check("A classified as tactical", rep.get("scope") == "tactical", rep)
r1_dec = rep.get("decision", {}).get("id")
check("A replan.proposed audit event", "replan.proposed" in event_types(pid))
check("A decision.requested audit event", "decision.requested" in event_types(pid))
check("A no human-inbox item for a tactical report", len(open_replan_inbox()) == 0, open_replan_inbox())

d = wait_until(lambda: (decision(pid, r1_dec) or {}).get("status") == "ruled" and decision(pid, r1_dec), 30)
check("A decision auto-ruled by the decision maker", d is not None, "not ruled within 30s")
if d:
    ruling = d.get("ruling") or {}
    check("A ruling = approve by agent", ruling.get("decision") == "approve" and ruling.get("by") == "agent", ruling)

new_a = wait_until(lambda: task_by_title(pid, "重规划-准备A"), 30)
check("A new task appeared in the project (kanban)", new_a is not None, "new task not found within 30s")
if new_a:
    check("A new task carries acceptance_required=true", new_a.get("acceptance_required") is True, new_a.get("acceptance_required"))
    check("A new task carries acceptance criteria", bool((new_a.get("acceptance_criteria") or "").strip()), new_a.get("acceptance_criteria"))
created_ev = next((e for e in events(pid) if e["type"] == "task.created"
                   and e.get("payload", {}).get("source") == "replan"
                   and e.get("payload", {}).get("title") == "重规划-准备A"), None)
check("A task.created event with source=replan", created_ev is not None)
done_a = wait_until(lambda: (task_by_title(pid, "重规划-准备A") or {}).get("status") == "done", 30)
check("A new task dispatched & completed (scheduled)", done_a is not None,
      (task_by_title(pid, "重规划-准备A") or {}).get("status"))
check("A replan.approved audit event", "replan.approved" in event_types(pid))
check("A replan.planning_started audit event", "replan.planning_started" in event_types(pid))
check("A replan.applied audit event", "replan.applied" in event_types(pid))

# ---------------------------------------------------------------- 2) approve (default fallback)
print("[smoke] 2) tactical approve — default fallback produces an accepted new task")
s, R2 = post(f"/projects/{pid}/tasks", {"title": "R2", "description": "fallback target",
                                        "executor_agent_id": ex["id"], "acceptor_agent_id": acc["id"],
                                        "depends_on": [G["id"]]})
check("reporting task R2 created", s == 200 and R2.get("id"), (s, R2))
s, rep = post(f"/tasks/{R2['id']}/replan", {"reason": "现有拆分遗漏了准备环节", "reported_by": "acceptor"})
r2_dec = rep.get("decision", {}).get("id")
d = wait_until(lambda: (decision(pid, r2_dec) or {}).get("status") == "ruled" and decision(pid, r2_dec), 30)
check("B decision auto-ruled", d is not None, "not ruled")
new_b = wait_until(lambda: task_by_title(pid, "重规划补充：R2"), 30)
check("B fallback task created with acceptance", new_b is not None and new_b.get("acceptance_required") is True,
      new_b)
if new_b:
    check("B fallback task carries acceptance criteria", bool((new_b.get("acceptance_criteria") or "").strip()),
          new_b.get("acceptance_criteria"))

# ---------------------------------------------------------------- 3) reject
print("[smoke] 3) reject — request closed, reporter notified, original task untouched")
s, R3 = post(f"/projects/{pid}/tasks", {"title": "R3", "description": "reject target",
                                        "executor_agent_id": ex["id"], "acceptor_agent_id": acc["id"],
                                        "depends_on": [G["id"]]})
check("reporting task R3 created", s == 200 and R3.get("id"), (s, R3))
before_tasks = len(tasks_of(pid))
before_status = task_by_id(pid, R3["id"]).get("status")
s, rep = post(f"/tasks/{R3['id']}/replan", {"reason": "上报的规划问题其实不成立 [reject] 仅为探测",
                                            "reported_by": "executor"})
r3_dec = rep.get("decision", {}).get("id")
d = wait_until(lambda: (decision(pid, r3_dec) or {}).get("status") == "ruled" and decision(pid, r3_dec), 30)
check("C decision ruled", d is not None, "not ruled")
if d:
    ruling = d.get("ruling") or {}
    check("C ruling = deny by agent", ruling.get("decision") == "deny" and ruling.get("by") == "agent", ruling)
    check("C effect = replan_rejected", (ruling.get("effect") or {}).get("effect") == "replan_rejected", ruling)
check("C replan.rejected audit event", "replan.rejected" in event_types(pid))
time.sleep(1.0)
check("C no new task created on the reject path", len(tasks_of(pid)) == before_tasks,
      (before_tasks, len(tasks_of(pid))))
check("C original task flow unaffected", task_by_id(pid, R3["id"]).get("status") == before_status,
      (before_status, task_by_id(pid, R3["id"]).get("status")))
logs = task_detail(R3["id"]).get("logs", [])
check("C reporter notified (rejection log on the task)",
      any("重规划请求被决策者拒绝" in l.get("line", "") for l in logs), logs[-3:])

# ---------------------------------------------------------------- 4) cycle rejection
print("[smoke] 4) cycle — a dependency adjustment introducing a cycle is rejected")
s, C2 = post(f"/projects/{pid}/tasks", {"title": "C2", "description": "cycle base",
                                        "executor_agent_id": ex["id"], "acceptor_agent_id": acc["id"],
                                        "depends_on": [G["id"]]})
s, C1 = post(f"/projects/{pid}/tasks", {"title": "C1", "description": "cycle target",
                                        "executor_agent_id": ex["id"], "acceptor_agent_id": acc["id"],
                                        "depends_on": [C2["id"]]})
check("cycle tasks created (C1 -> C2)", bool(C1.get("id")) and bool(C2.get("id")), (C1, C2))
c2_deps_before = task_by_id(pid, C2["id"]).get("depends_on")
s, rep = post(f"/tasks/{C1['id']}/replan", {
    "reason": "调整依赖顺序（会成环，应被拒绝）",
    "reported_by": "executor",
    "proposed_changes": [{"op": "update", "task_id": C2["id"], "fields": {"depends_on": ["C1"]}}],
})
cycle_dec = rep.get("decision", {}).get("id")
d = wait_until(lambda: (decision(pid, cycle_dec) or {}).get("status") == "ruled" and decision(pid, cycle_dec), 30)
check("D decision ruled", d is not None, "not ruled")
applied_ev = wait_until(lambda: next((e for e in events(pid) if e["type"] == "replan.applied"
                                      and e.get("payload", {}).get("decision_id") == cycle_dec), None), 30)
check("D replan.applied emitted", applied_ev is not None)
if applied_ev:
    skipped = applied_ev.get("payload", {}).get("skipped", [])
    check("D cycle change skipped with a cycle reason",
          any("cycle" in json.dumps(s).lower() for s in skipped), skipped)
    check("D no change applied", applied_ev.get("payload", {}).get("applied") == [],
          applied_ev.get("payload"))
check("D C2 dependency unchanged",
      task_by_id(pid, C2["id"]).get("depends_on") == c2_deps_before,
      (c2_deps_before, task_by_id(pid, C2["id"]).get("depends_on")))

# ---------------------------------------------------------------- 5) strategic escalation
print("[smoke] 5) strategic — milestone-touching request escalates to the human inbox")
s, M = post(f"/projects/{pid}/milestones", {"name": "M1", "goal": "phase one", "effect": "ready"})
check("milestone created", s == 200 and M.get("id"), (s, M))
s, S1 = post(f"/projects/{pid}/tasks", {"title": "S1", "description": "milestone task",
                                        "milestone_id": M["id"],
                                        "executor_agent_id": ex["id"], "acceptor_agent_id": acc["id"],
                                        "depends_on": [G["id"]]})
check("milestone-linked reporting task created", s == 200 and S1.get("id"), (s, S1))
s, rep = post(f"/tasks/{S1['id']}/replan", {
    "reason": "里程碑目标覆盖范围需要调整",
    "reported_by": "acceptor",
    "proposed_changes": [{"op": "add", "task": {"title": "里程碑窗口-新增", "description": "窗口补充",
                                                "depends_on": [], "acceptance_required": True,
                                                "acceptance_criteria": "窗口补充产物可检验"}}],
})
check("E report accepted", s == 200 and rep.get("decision", {}).get("id"), (s, rep))
check("E classified as strategic", rep.get("scope") == "strategic", rep)
s1_dec = rep.get("decision", {}).get("id")
check("E decision immediately escalated", (decision(pid, s1_dec) or {}).get("status") == "escalated",
      decision(pid, s1_dec))
item = wait_until(lambda: next((i for i in open_replan_inbox()
                                if i.get("payload", {}).get("decision_id") == s1_dec), None), 15)
check("E open replan item in the human inbox", item is not None, open_replan_inbox())
check("E decision.escalated audit event", "decision.escalated" in event_types(pid))
time.sleep(2.0)
check("E strategic request NOT auto-ruled by the agent",
      (decision(pid, s1_dec) or {}).get("status") == "escalated", decision(pid, s1_dec))
check("E no planning started before human ruling",
      not any(e["type"] == "replan.planning_started" and e.get("payload", {}).get("decision_id") == s1_dec
              for e in events(pid)))

if item:
    s, res = post(f"/inbox/{item['id']}/resolve", {"resolution": "approve", "note": "人工批准里程碑窗口调整"})
    check("E human resolves the escalated replan (200)", s == 200, (s, res))
    d2 = wait_until(lambda: ((decision(pid, s1_dec) or {}).get("status") == "ruled"
                             and (decision(pid, s1_dec).get("ruling") or {}).get("by") == "human"
                             and decision(pid, s1_dec)), 30)
    check("E decided by human after inbox approval", d2 is not None, decision(pid, s1_dec))
    new_e = wait_until(lambda: task_by_title(pid, "里程碑窗口-新增"), 30)
    check("E human-approved replan lands a new task", new_e is not None, "new task missing")

# ---------------------------------------------------------------- 6) audit replay
print("[smoke] 6) audit — full replan chain replayable from events")
types = set(event_types(pid))
for ev in ["replan.proposed", "replan.approved", "replan.rejected",
           "replan.planning_started", "replan.applied",
           "decision.requested", "decision.ruled", "decision.escalated"]:
    check(f"F audit event present: {ev}", ev in types)
r1_events = [e["type"] for e in task_detail(R1["id"]).get("events", [])]
check("F reporting task timeline contains replan.proposed", "replan.proposed" in r1_events, r1_events)

# ---------------------------------------------------------------- 7) pause freezes planning
print("[smoke] 7) pause — approved replan defers planner dispatch until resume")
s, M2 = post(f"/projects/{pid}/milestones", {"name": "M2", "goal": "pause window", "effect": "ready"})
check("pause: milestone created", s == 200 and M2.get("id"), (s, M2))
s, S2 = post(f"/projects/{pid}/tasks", {"title": "S2", "description": "pause-linked task",
                                        "milestone_id": M2["id"],
                                        "executor_agent_id": ex["id"], "acceptor_agent_id": acc["id"],
                                        "depends_on": [G["id"]]})
check("pause: milestone-linked reporting task created", s == 200 and S2.get("id"), (s, S2))
s, _ = post(f"/projects/{pid}/pause", {})
check("pause: project paused", project(pid).get("project", {}).get("status") == "paused",
      project(pid).get("project"))
s, rep = post(f"/tasks/{S2['id']}/replan", {
    "reason": "暂停期间上报的里程碑窗口调整",
    "reported_by": "acceptor",
    "proposed_changes": [{"op": "add", "task": {"title": "暂停窗口-新增", "description": "暂停窗口补充",
                                                "depends_on": [], "acceptance_required": True,
                                                "acceptance_criteria": "暂停窗口产物可检验"}}],
})
p_dec = rep.get("decision", {}).get("id")
check("pause: strategic report escalated (human-decided during pause)", rep.get("scope") == "strategic", rep)
item = wait_until(lambda: next((i for i in open_replan_inbox()
                                if i.get("payload", {}).get("decision_id") == p_dec), None), 15)
check("pause: replan item in the human inbox", item is not None, open_replan_inbox())
if item:
    s, _ = post(f"/inbox/{item['id']}/resolve", {"resolution": "approve", "note": "暂停期间人工批准"})
    check("pause: human approves while paused (ruling is not frozen)", s == 200)
    d3 = wait_until(lambda: ((decision(pid, p_dec) or {}).get("status") == "ruled"
                             and (decision(pid, p_dec).get("ruling") or {}).get("by") == "human"
                             and decision(pid, p_dec)), 30)
    check("pause: approved with planning=pending while still paused", d3 is not None, decision(pid, p_dec))
    if d3:
        check("pause: effect planning=pending", ((d3.get("ruling") or {}).get("effect") or {}).get("planning") == "pending",
              d3.get("ruling"))
time.sleep(2.0)
check("pause: no planning started while paused",
      not any(e["type"] == "replan.planning_started" and e.get("payload", {}).get("decision_id") == p_dec
              for e in events(pid)))
check("pause: no new task landed while paused", task_by_title(pid, "暂停窗口-新增") is None,
      task_by_title(pid, "暂停窗口-新增"))
post(f"/projects/{pid}/resume", {})
check("pause: project resumed", project(pid).get("project", {}).get("status") == "running",
      project(pid).get("project"))
started = wait_until(lambda: any(e["type"] == "replan.planning_started"
                                 and e.get("payload", {}).get("decision_id") == p_dec
                                 for e in events(pid)), 30)
check("pause: planning resumes after resume", started is not None)
new_p = wait_until(lambda: task_by_title(pid, "暂停窗口-新增"), 30)
check("pause: deferred task lands after resume", new_p is not None, "new task missing after resume")

# ---------------------------------------------------------------- 8) acceptance default
print("[smoke] 8) acceptance default — reporter omits criteria, landed task still gets one")
s, R8 = post(f"/projects/{pid}/tasks", {"title": "R8", "description": "no-criteria target",
                                        "executor_agent_id": ex["id"], "acceptor_agent_id": acc["id"],
                                        "depends_on": [G["id"]]})
check("G reporting task R8 created", s == 200 and R8.get("id"), (s, R8))
s, rep = post(f"/tasks/{R8['id']}/replan", {
    "reason": "拆分遗漏了准备步骤（上报者未给出验收标准）",
    "reported_by": "executor",
    "proposed_changes": [{"op": "add", "task": {"title": "重规划-无标准", "description": "缺验收标准",
                                                 "depends_on": [], "acceptance_required": True}}],
})
check("G report accepted", s == 200 and rep.get("decision", {}).get("id"), (s, rep))
check("G classified as tactical", rep.get("scope") == "tactical", rep)
r8_dec = rep.get("decision", {}).get("id")
d = wait_until(lambda: (decision(pid, r8_dec) or {}).get("status") == "ruled" and decision(pid, r8_dec), 30)
check("G decision ruled", d is not None, "not ruled")
new_g = wait_until(lambda: task_by_title(pid, "重规划-无标准"), 30)
check("G new task still lands", new_g is not None, "new task missing")
if new_g:
    check("G acceptance_required defaults to true", new_g.get("acceptance_required") is True,
          new_g.get("acceptance_required"))
    check("G acceptance criteria defaulted (non-empty)",
          bool((new_g.get("acceptance_criteria") or "").strip()), new_g.get("acceptance_criteria"))

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
