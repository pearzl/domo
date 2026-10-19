#!/usr/bin/env bash
# M3 验收标准协商（DESIGN §7.1）—— 端到端验收冒烟（mock agents）。
#
# 覆盖验收标准：
#   1. 正常任务：executor↔acceptor 协商一致 → acceptance_criteria 非空、
#      negotiation_status=agreed，任务进入 READY 并跑到 DONE；任务详情可回放轮次记录；
#   2. 多轮协商：第 1 轮被拒 → 携带上轮意见修订 → 第 2 轮一致（negotiation_round>=2）；
#   3. 对抗验收者（拒绝任何标准）：协商失败 → negotiation_status=failed 且自动登记
#      decision(kind=negotiation_failed)；未裁定前不派发执行；决策者裁定后恢复流转至 DONE；
#   4. 未协商完成不派发执行：协商 agreed 事件必须先于任何 execute 会话；
#   5. 跨进程重启：kill -9 后 pending 协商状态保留，恢复调度后协商从持久化状态继续并达成一致。
#
# 用法：bash tests/smoke_negotiation.sh   （退出码 0 = 全部通过）
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_SMOKE_PORT:-8793}"
TMP_ROOT="${TMPDIR:-/tmp}"
RUN_DIR="$(mktemp -d "$TMP_ROOT/domo_negotiation.XXXXXX")"
DB="$RUN_DIR/domo.db"
WS="$RUN_DIR/workspace"
LOG="$RUN_DIR/server.log"
STATE="$RUN_DIR/state.json"
PY="$RUN_DIR/smoke.py"
mkdir -p "$WS"

SERVER_PID=""
cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill -9 "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

start_server() {
  echo "[neg-smoke] starting server on :$PORT (db $DB)"
  (
    cd "$ROOT/server"
    DOMO_DB="$DB" DOMO_PORT="$PORT" DOMO_LOG="$LOG" DOMO_LOG_LEVEL=info \
      exec "$ROOT/server/target/debug/domo-server"
  ) >"$RUN_DIR/stdout.log" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 120); do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "[neg-smoke] server exited early; stdout tail:"; tail -40 "$RUN_DIR/stdout.log"; return 1
    fi
    if curl -sf "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1; then return 0; fi
    sleep 0.25
  done
  echo "[neg-smoke] server did not come up; log:"; tail -40 "$RUN_DIR/stdout.log"
  return 1
}

cat >"$PY" <<'PY'
import json, sys, time, urllib.request, urllib.error

PHASE, PORT, STATE_PATH = sys.argv[1], sys.argv[2], sys.argv[3]
BASE = f"http://127.0.0.1:{PORT}/api"
CHECKS = []

def check(name, cond, detail=""):
    cond = bool(cond)
    CHECKS.append((name, cond))
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  -- " + str(detail)) if (detail and not cond) else ""), flush=True)

def req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(BASE + path, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=20) as resp:
            return resp.status, json.load(resp)
    except urllib.error.HTTPError as e:
        try: payload = json.load(e)
        except Exception: payload = {}
        return e.code, payload
    except Exception as e:
        return 0, {"error": str(e)}

post = lambda p, b=None: req("POST", p, b or {})
get  = lambda p: req("GET", p)
patch = lambda p, b: req("PATCH", p, b)

def wait_until(fn, timeout=30, interval=0.25):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v: return v
        time.sleep(interval)
    return None

def detail(tid):
    s, b = get(f"/tasks/{tid}")
    return b if s == 200 else None

def task_of(tid):
    d = detail(tid)
    return d.get("task") if d else None

def events_of(tid):
    d = detail(tid)
    return d.get("events", []) if d else []

def first_event_id(tid, predicate):
    for e in events_of(tid):
        if predicate(e):
            return e.get("id")
    return None

def decisions(pid, kind):
    s, b = get(f"/projects/{pid}/decisions?x=1&kind={kind}")
    return b if s == 200 else []

def new_project(name):
    s, p = post("/projects", {"name": name, "brief": "M3 验收标准协商冒烟"})
    pid = p.get("id")
    patch(f"/projects/{pid}", {"workspace": {"kind": "local", "path": sys.argv[4]}})
    post(f"/projects/{pid}/confirm")
    return pid

def create_task(pid, title, desc, ex, acc):
    s, t = post(f"/projects/{pid}/tasks", {
        "title": title, "description": desc, "acceptance_required": True,
        "executor_agent_id": ex, "acceptor_agent_id": acc})
    assert s == 200, (title, t)
    return t

def save_state(d):
    with open(STATE_PATH, "w") as f:
        json.dump(d, f)

def phase1():
    print("[neg-smoke] 1) agents")
    s, ex = post("/agents", {"name": "neg-exec", "adapter": "mock", "roles": ["executor"],
                             "capabilities": {"max_parallel": 4, "supports_resume": False},
                             "config": {"duration_ms": 200}})
    s, ac = post("/agents", {"name": "neg-acc", "adapter": "mock", "roles": ["acceptor"],
                             "capabilities": {"max_parallel": 4, "supports_resume": False},
                             "config": {"duration_ms": 200}})
    s, dm = post("/agents", {"name": "neg-dm", "adapter": "mock", "roles": ["decision_maker"],
                             "config": {"duration_ms": 200}})
    EXEC, ACC, DM = ex.get("id"), ac.get("id"), dm.get("id")
    check("registered mock executor/acceptor/decision_maker", all([EXEC, ACC, DM]), (ex, ac, dm))
    if not all([EXEC, ACC, DM]):
        return 1

    # ---------------- 1) 正常协商：一致 → agreed → done ----------------
    print("[neg-smoke] 2) normal negotiation reaches agreement, then runs to done")
    P = new_project("协商-正常")
    T = create_task(P, "正常协商任务", "产出 health-check 报告", EXEC, ACC)
    # 创建默认进入协商
    check("acceptance-required task defaults to negotiation pending",
          task_of(T["id"]).get("negotiation_status") == "pending", task_of(T["id"]).get("negotiation_status"))
    agreed = wait_until(lambda: task_of(T["id"]).get("negotiation_status") == "agreed" and task_of(T["id"]), 40)
    check("negotiation reached agreed", agreed is not None, task_of(T["id"]).get("negotiation_status"))
    if agreed:
        check("agreed task carries non-empty acceptance_criteria", bool(agreed.get("acceptance_criteria")),
              agreed.get("acceptance_criteria"))
        check("negotiation_round >= 1", (agreed.get("negotiation_round") or 0) >= 1, agreed.get("negotiation_round"))
    d = detail(T["id"])
    rounds = (d or {}).get("negotiation", {}).get("rounds", [])
    check("task detail exposes negotiation round records", len(rounds) >= 1, rounds)
    check("negotiation.round events recorded",
          any(e["type"] == "negotiation.round" for e in (d or {}).get("events", [])), None)
    check("negotiation.agreed event recorded",
          any(e["type"] == "negotiation.agreed" for e in (d or {}).get("events", [])), None)
    # 未协商完成不得派发执行：agreed 事件必须先于任何 execute 会话
    agreed_id = first_event_id(T["id"], lambda e: e["type"] == "negotiation.agreed")
    exec_id = first_event_id(T["id"], lambda e: e["type"] == "agent.session_started"
                             and (e.get("payload") or {}).get("phase") == "execute")
    check("execution session starts only after negotiation agreed",
          agreed_id is not None and exec_id is not None and agreed_id < exec_id, (agreed_id, exec_id))
    done = wait_until(lambda: task_of(T["id"]).get("status") == "done" and task_of(T["id"]), 60)
    check("task runs to done after agreement", done is not None, task_of(T["id"]).get("status"))

    # ---------------- 2) 多轮协商 ----------------
    print("[neg-smoke] 3) multi-round negotiation ([negotiate-round2])")
    P2 = new_project("协商-多轮")
    T2 = create_task(P2, "多轮协商任务", "[negotiate-round2] 需要两轮才达成一致", EXEC, ACC)
    agreed2 = wait_until(lambda: task_of(T2["id"]).get("negotiation_status") == "agreed" and task_of(T2["id"]), 40)
    check("multi-round negotiation reaches agreed", agreed2 is not None, task_of(T2["id"]).get("negotiation_status"))
    if agreed2:
        check("multi-round negotiation_round >= 2", (agreed2.get("negotiation_round") or 0) >= 2, agreed2.get("negotiation_round"))
    r2 = (detail(T2["id"]) or {}).get("negotiation", {}).get("rounds", [])
    check("multi-round history has >= 2 rounds", len(r2) >= 2, r2)
    check("a rejected acceptor round is on record",
          any(x.get("role") == "acceptor" and x.get("agree") is False for x in r2), r2)

    # ---------------- 3) 对抗验收者：协商失败 → 决策者裁决 → 恢复 ----------------
    print("[neg-smoke] 4) adversarial acceptor -> failed -> adjudicated -> recovered")
    P3 = new_project("协商-对抗")
    T3 = create_task(P3, "对抗协商任务", "[negotiate-reject] 拒绝任何验收标准", EXEC, ACC)
    failed = wait_until(lambda: task_of(T3["id"]).get("negotiation_status") == "failed" and task_of(T3["id"]), 40)
    check("adversarial negotiation failed", failed is not None, task_of(T3["id"]).get("negotiation_status"))
    dec = wait_until(lambda: next((d for d in decisions(P3, "negotiation_failed")), None), 20)
    check("negotiation_failed decision auto-registered", dec is not None, None)
    # 未裁定前不得派发执行
    st = task_of(T3["id"]).get("status")
    check("failed negotiation stays out of execution", st in ("planned", "suspended"), st)
    exec3 = first_event_id(T3["id"], lambda e: e["type"] == "agent.session_started"
                           and (e.get("payload") or {}).get("phase") == "execute")
    check("no execution session before adjudication", exec3 is None, exec3)
    ruled = wait_until(lambda: next((d for d in decisions(P3, "negotiation_failed") if d["status"] == "ruled"), None), 30)
    check("decision maker ruled the failed negotiation", ruled is not None, None)
    settled = wait_until(lambda: task_of(T3["id"]).get("negotiation_status") == "agreed" and task_of(T3["id"]), 40)
    check("ruling settles negotiation to agreed", settled is not None, task_of(T3["id"]).get("negotiation_status"))
    check("negotiation.settled event recorded",
          any(e["type"] == "negotiation.settled" for e in events_of(T3["id"])), None)
    done3 = wait_until(lambda: task_of(T3["id"]).get("status") == "done", 60)
    check("task completes after adjudication", done3 is not None, task_of(T3["id"]).get("status"))

    # ---------------- 4) 跨重启持久化：paused 项目里保留 pending 协商现场 ----------------
    print("[neg-smoke] 5) pending negotiation survives process restart")
    P4 = new_project("协商-重启")
    post(f"/projects/{P4}/pause")  # 冻结派发：协商不得开工
    time.sleep(0.6)
    T4 = create_task(P4, "重启续跑任务", "验证 pending 协商状态跨重启保留", EXEC, ACC)
    time.sleep(0.8)
    check("paused project keeps task pending (not started)", task_of(T4["id"]).get("negotiation_status") == "pending",
          task_of(T4["id"]).get("negotiation_status"))
    neg_started = first_event_id(T4["id"], lambda e: e["type"] == "agent.session_started"
                                 and (e.get("payload") or {}).get("phase") == "negotiate")
    check("no negotiation worker ran while paused", neg_started is None, neg_started)
    save_state({"P": P, "T": T["id"], "P4": P4, "T4": T4["id"]})
    return 0

def phase2():
    st = json.load(open(STATE_PATH))
    T, P4, T4 = st["T"], st["P4"], st["T4"]
    print("[neg-smoke] 6) after kill -9 restart: state preserved, negotiation resumes")
    check("pre-restart task still agreed", task_of(T).get("negotiation_status") == "agreed", task_of(T).get("negotiation_status"))
    check("pre-restart negotiation history preserved",
          len((detail(T) or {}).get("negotiation", {}).get("rounds", [])) >= 1, None)
    proj4 = get(f"/projects/{P4}")[1].get("project", {})
    check("paused project persisted across restart", proj4.get("status") == "paused", proj4.get("status"))
    check("paused pending task persisted across restart",
          task_of(T4).get("negotiation_status") == "pending", task_of(T4).get("negotiation_status"))
    s, _ = post(f"/projects/{P4}/resume")
    check("project resumed", s == 200, s)
    agreed4 = wait_until(lambda: task_of(T4).get("negotiation_status") == "agreed" and task_of(T4), 40)
    check("negotiation resumes after restart and reaches agreed", agreed4 is not None, task_of(T4).get("negotiation_status"))
    check("resumed negotiation keeps round records",
          len((detail(T4) or {}).get("negotiation", {}).get("rounds", [])) >= 1, None)
    done4 = wait_until(lambda: task_of(T4).get("status") == "done", 60)
    check("resumed task completes", done4 is not None, task_of(T4).get("status"))
    return 0

rc = phase1() if PHASE == "phase1" else phase2()
failed = [n for n, ok in CHECKS if not ok]
print()
print(f"[neg-smoke] {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
if failed:
    print("[neg-smoke] FAILED:")
    for n in failed:
        print("  - " + n)
    sys.exit(1)
print("[neg-smoke] ALL PASS")
sys.exit(rc or 0)
PY

echo "[neg-smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[neg-smoke] build failed"; exit 1; }

start_server || exit 1
python3 "$PY" phase1 "$PORT" "$STATE" "$WS"
RC1=$?

echo "[neg-smoke] kill -9 server (pid $SERVER_PID) and restart…"
kill -9 "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
start_server || exit 1
python3 "$PY" phase2 "$PORT" "$STATE" "$WS"
RC2=$?

RC=$(( RC1 + RC2 ))
if [[ $RC -ne 0 ]]; then
  echo "[neg-smoke] server log tail:"; tail -60 "$LOG" 2>/dev/null || true
fi
exit $RC
