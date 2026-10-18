#!/usr/bin/env bash
# M3 验收标准协商（executor↔acceptor 多轮）—— 端到端验收冒烟。
#
# 覆盖任务验收标准：
#   1. 正常任务协商一致后进入 READY 且 acceptance_criteria 非空；
#      任务详情返回协商轮次记录（negotiation.rounds，executor 提案 + acceptor 审核）。
#   2. 多轮协商：[negotiate-round2] 首轮反对、次轮修订后一致，轮次历史完整保留。
#   3. 协商未达成前调度器不派发该任务（planned 且 pending 时不得进入 executing；
#      planned→ready 只在 criteria_agreed_auto 放行后发生）。
#   4. 对抗用例（acceptor 拒绝任何标准 [negotiate-reject]）→ negotiation_status=failed，
#      任务转决策者裁决，裁定后恢复流转至 DONE。
#   5. 重启（kill -9）后协商状态不丢失：failed / pending 的 negotiation_status 与已完成
#      轮次均持久化，协商历史从事件流重建；决策者通道恢复后继续裁定并跑完。
#
# 用法：bash scripts/smoke_negotiation.sh   （退出码 0 = 全部通过）
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_SMOKE_PORT:-8792}"
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
  echo "[smoke] starting server on :$PORT (db $DB)"
  (
    cd "$ROOT/server"
    DOMO_DB="$DB" DOMO_PORT="$PORT" DOMO_LOG="$LOG" DOMO_LOG_LEVEL=info \
      exec "$ROOT/server/target/debug/domo-server"
  ) >"$RUN_DIR/stdout.log" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 80); do
    if curl -sf "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1; then return 0; fi
    sleep 0.25
  done
  echo "[smoke] server did not come up; log:"; tail -30 "$RUN_DIR/stdout.log"
  return 1
}

echo "[smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[smoke] build failed"; exit 1; }

cat >"$PY" <<'PY'
import json, sys, time, urllib.request, urllib.error

PHASE, PORT, WS, STATE = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
BASE = f"http://127.0.0.1:{PORT}/api"
CHECKS = []

def check(name, cond, detail=""):
    cond = bool(cond)
    CHECKS.append((name, cond))
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  -- " + str(detail)) if (detail and not cond) else ""), flush=True)

def req(method, path, body=None, timeout=25):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(BASE + path, data=data, method=method,
                               headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            return resp.status, json.load(resp)
    except urllib.error.HTTPError as e:
        try: payload = json.load(e)
        except Exception: payload = {}
        return e.code, payload
    except Exception as e:
        return 0, {"error": str(e)}

def post(p, b=None): return req("POST", p, b if b is not None else {})
def patch(p, b): return req("PATCH", p, b)
def get(p): return req("GET", p)
def delete(p): return req("DELETE", p)

def wait_until(fn, timeout=30, interval=0.3):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(interval)
    return None

def task_of(tid):
    s, b = get(f"/tasks/{tid}")
    return b.get("task") if s == 200 else None

def detail_of(tid):
    s, b = get(f"/tasks/{tid}")
    return b if s == 200 else {}

def event_types():
    s, b = get("/events")
    return [e["type"] for e in b] if s == 200 else []

def decisions(pid, **q):
    qs = "".join(f"&{k}={v}" for k, v in q.items())
    s, b = get(f"/projects/{pid}/decisions?x=1{qs}")
    return b if s == 200 else []

def agreed(tid):
    t = task_of(tid)
    return t if t and t.get("negotiation_status") == "agreed" else None

def released(tid):
    """Negotiation agreed AND the scheduler has actually released it from planned."""
    t = task_of(tid)
    return t if t and t.get("negotiation_status") == "agreed" and t.get("status") != "planned" else None

def failed(tid):
    t = task_of(tid)
    return t if t and t.get("negotiation_status") == "failed" else None


if PHASE == "phase1":
    print("[smoke] 1) agents")
    s, ex = post("/agents", {"name": "neg-exec", "adapter": "mock", "roles": ["executor"], "config": {"duration_ms": 50}})
    check("executor agent registered", s == 200 and ex.get("id"), f"{s} {ex}")
    s, ac = post("/agents", {"name": "neg-acc", "adapter": "mock", "roles": ["acceptor"], "config": {"duration_ms": 50}})
    check("acceptor agent registered", s == 200 and ac.get("id"), f"{s} {ac}")
    s, dm = post("/agents", {"name": "neg-judge", "adapter": "mock", "roles": ["decision_maker"], "config": {"duration_ms": 50}})
    check("decision_maker agent registered", s == 200 and dm.get("id"), f"{s} {dm}")

    print("[smoke] 2) project bootstrap")
    s, proj = post("/projects", {"name": "协商冒烟", "brief": "M3 negotiation smoke"})
    check("project created", s == 200 and proj.get("id"), f"{s} {proj}")
    pid = proj["id"]
    s, _ = patch(f"/projects/{pid}", {"workspace": {"kind": "local", "path": WS}})
    check("workspace set", s == 200, s)
    s, _ = post(f"/projects/{pid}/confirm", {})
    check("project confirmed (running)", s == 200, s)

    # Sentinel: a long-running execution keeps the project `running` so the whole
    # script is not short-circuited by automatic project COMPLETED/ARCHIVED.
    s, slow = post("/agents", {"name": "neg-slow", "adapter": "mock", "roles": ["executor"], "config": {"duration_ms": 120000}})
    check("sentinel executor agent registered", s == 200 and slow.get("id"), f"{s} {slow}")
    s, t_sent = post(f"/projects/{pid}/tasks", {
        "title": "常驻执行任务", "description": "占位长任务，保持项目 running",
        "acceptance_criteria": "长期运行", "executor_agent_id": slow["id"], "acceptor_agent_id": ac["id"]})
    check("sentinel task created", s == 200 and t_sent.get("id"), f"{s} {t_sent}")
    sent_id = t_sent.get("id")

    print("[smoke] 3) C1 — normal task agrees in one round and enters READY")
    s, t = post(f"/projects/{pid}/tasks", {
        "title": "正常协商任务", "description": "常规任务，首轮一致",
        "acceptance_criteria": "功能可用且无阻塞缺陷",
        "executor_agent_id": ex["id"], "acceptor_agent_id": ac["id"]})
    agree_id = t.get("id")
    check("C1 task created entering negotiation (planned+pending)",
          s == 200 and agree_id and t.get("negotiation_status") == "pending" and t.get("status") == "planned",
          f"{s} {t}")
    t = wait_until(lambda: released(agree_id), 30)
    check("C1 negotiation agreed and released from planned",
          t is not None and t.get("status") in ("ready", "executing", "awaiting_acceptance", "done"), t)
    check("C1 acceptance_criteria non-empty after agreement",
          t and bool((t.get("acceptance_criteria") or "").strip()), t and t.get("acceptance_criteria"))
    d = detail_of(agree_id)
    neg = d.get("negotiation") or {}
    rounds = neg.get("rounds") or []
    check("C1 task detail exposes negotiation block", bool(neg), neg)
    check("C1 max_rounds == 3", neg.get("max_rounds") == 3, neg.get("max_rounds"))
    check("C1 negotiation rounds recorded (>=2 entries)", len(rounds) >= 2, len(rounds))
    check("C1 rounds include executor proposal",
          any(r.get("role") == "executor" and (r.get("proposal") or "").strip() for r in rounds), rounds)
    check("C1 rounds include acceptor agreement",
          any(r.get("role") == "acceptor" and r.get("agree") is True for r in rounds), rounds)
    et = event_types()
    check("C1 negotiation.started event", "negotiation.started" in et)
    check("C1 negotiation.round event", "negotiation.round" in et)
    check("C1 negotiation.agreed event", "negotiation.agreed" in et)
    evs = d.get("events") or []
    agreed_ids = [e["id"] for e in evs if e["type"] == "negotiation.agreed"]
    first_agreed = min(agreed_ids) if agreed_ids else (1 << 60)
    exec_before = [e["id"] for e in evs if e["type"] == "agent.session_started"
                   and (e.get("payload") or {}).get("phase") == "execute" and e["id"] < first_agreed]
    check("C1 no execution session before agreement", not exec_before, exec_before)
    ready_reasons = [(e.get("payload") or {}).get("reason") for e in evs
                     if e["type"] == "task.status_changed" and (e.get("payload") or {}).get("to") == "ready"]
    check("C1 planned->ready released by criteria_agreed_auto", "criteria_agreed_auto" in ready_reasons, ready_reasons)
    t = wait_until(lambda: (task_of(agree_id) or {}).get("status") == "done", 30)
    check("C1 task completed", t is not None, (task_of(agree_id) or {}).get("status"))

    print("[smoke] 4) C2 — multi-round negotiation (oppose then revise then agree)")
    s, t = post(f"/projects/{pid}/tasks", {
        "title": "多轮协商任务", "description": "需要两轮往复 [negotiate-round2]",
        "acceptance_criteria": "基线标准：功能可用",
        "executor_agent_id": ex["id"], "acceptor_agent_id": ac["id"]})
    multi_id = t.get("id")
    check("C2 multi-round task created", s == 200 and multi_id, f"{s} {t}")
    t = wait_until(lambda: agreed(multi_id), 30)
    check("C2 negotiation agreed after multiple rounds", t is not None, t)
    d = detail_of(multi_id)
    neg = d.get("negotiation") or {}
    rounds = neg.get("rounds") or []
    check("C2 completed round count >= 2", (neg.get("round") or 0) >= 2, neg.get("round"))
    agrees = [r.get("agree") for r in rounds if r.get("role") == "acceptor"]
    check("C2 acceptor opposed first, agreed later", agrees[:1] == [False] and agrees[-1:] == [True], agrees)
    check("C2 criteria carries the round-2 revision",
          "第 2 轮修订" in (t.get("acceptance_criteria") or ""), t and t.get("acceptance_criteria"))
    t = wait_until(lambda: (task_of(multi_id) or {}).get("status") == "done", 30)
    check("C2 multi-round task completed", t is not None, (task_of(multi_id) or {}).get("status"))

    print("[smoke] 5) C3 — scheduler must not dispatch an unagreed task")
    s, _ = post(f"/projects/{pid}/pause", {})
    check("C3 project paused", s == 200, s)
    s, t = post(f"/projects/{pid}/tasks", {
        "title": "协商阻挡探针", "description": "暂停期间创建，协商尚未达成",
        "acceptance_criteria": "待协商的草案",
        "executor_agent_id": ex["id"], "acceptor_agent_id": ac["id"]})
    gate_id = t.get("id")
    check("C3 pending task created while paused",
          s == 200 and t.get("negotiation_status") == "pending" and t.get("status") == "planned", f"{s} {t}")
    time.sleep(1.5)
    t = task_of(gate_id)
    check("C3 unagreed task stays planned (never dispatched)", t and t.get("status") == "planned", t and t.get("status"))
    check("C3 unagreed task still pending", t and t.get("negotiation_status") == "pending", t and t.get("negotiation_status"))
    s, _ = post(f"/projects/{pid}/resume", {})
    check("C3 project resumed", s == 200, s)
    t = wait_until(lambda: released(gate_id), 30)
    check("C3 task negotiates to agreement after resume", t is not None, t)
    t = wait_until(lambda: (task_of(gate_id) or {}).get("status") == "done", 30)
    check("C3 task completes only after agreement", t is not None, (task_of(gate_id) or {}).get("status"))

    print("[smoke] 6) C4 — adversarial: acceptor rejects everything -> failed -> adjudicated")
    s, t = post(f"/projects/{pid}/tasks", {
        "title": "对抗协商任务", "description": "验收者拒绝任何标准 [negotiate-reject]",
        "acceptance_criteria": "执行者草案标准",
        "executor_agent_id": ex["id"], "acceptor_agent_id": ac["id"]})
    rej_id = t.get("id")
    check("C4 adversarial task created", s == 200 and rej_id, f"{s} {t}")
    t = wait_until(lambda: failed(rej_id), 30)
    check("C4 negotiation reached failed", t is not None, t)
    check("C4 failed task stays planned (not dispatched)", t and t.get("status") == "planned", t and t.get("status"))
    d = detail_of(rej_id)
    neg = d.get("negotiation") or {}
    rounds = neg.get("rounds") or []
    check("C4 exhausted maximum rounds (round=3)", neg.get("round") == 3, neg.get("round"))
    check("C4 every round recorded (6 entries)", len(rounds) == 6, len(rounds))
    acc_rounds = [r for r in rounds if r.get("role") == "acceptor"]
    check("C4 all acceptor rounds rejected", len(acc_rounds) == 3 and all(r.get("agree") is False for r in acc_rounds), acc_rounds)
    check("C4 negotiation.failed event", "negotiation.failed" in event_types())
    dec = wait_until(lambda: next((x for x in decisions(pid, kind="negotiation_failed")
                                   if x.get("task_id") == rej_id and x.get("status") == "ruled"), None), 30)
    check("C4 decision maker adjudicated the failed negotiation", dec is not None, "no ruled negotiation_failed decision")
    if dec:
        check("C4 ruling produced by agent", (dec.get("ruling") or {}).get("by") == "agent", dec.get("ruling"))
    t = wait_until(lambda: (lambda x: x if x and x.get("negotiation_status") == "agreed"
                            and (x.get("acceptance_criteria") or "").strip() else None)(task_of(rej_id)), 30)
    check("C4 adjudication settled negotiation -> agreed", t is not None, t)
    check("C4 settled acceptance_criteria non-empty", t and bool((t.get("acceptance_criteria") or "").strip()),
          t and t.get("acceptance_criteria"))
    check("C4 negotiation.settled event", "negotiation.settled" in event_types())
    t = wait_until(lambda: (task_of(rej_id) or {}).get("status") == "done", 30)
    check("C4 task resumed its flow and completed", t is not None, (task_of(rej_id) or {}).get("status"))

    print("[smoke] 7) C5 — prepare durable negotiation states for restart")
    s, delres = delete(f"/agents/{dm['id']}")
    check("C5 decision maker removed (freezes adjudication)", s == 200, f"{s} {delres}")
    s, t = post(f"/projects/{pid}/tasks", {
        "title": "重启前协商失败任务", "description": "拒绝一切 [negotiate-reject]",
        "acceptance_criteria": "失败草案标准",
        "executor_agent_id": ex["id"], "acceptor_agent_id": ac["id"]})
    fail_id = t.get("id")
    check("C5 failed-negotiation task created", s == 200 and fail_id, f"{s} {t}")
    t = wait_until(lambda: failed(fail_id), 30)
    check("C5 negotiation failed while the decision maker is absent", t is not None, t)
    d = detail_of(fail_id)
    neg = d.get("negotiation") or {}
    fail_snapshot = {
        "negotiation_status": (t or {}).get("negotiation_status"),
        "negotiation_round": neg.get("round"),
        "rounds": len(neg.get("rounds") or []),
        "criteria": (t or {}).get("acceptance_criteria"),
    }
    check("C5 failed snapshot = failed/round 3/6 rounds",
          fail_snapshot["negotiation_status"] == "failed" and fail_snapshot["negotiation_round"] == 3
          and fail_snapshot["rounds"] == 6, fail_snapshot)
    wait_until(lambda: next((x for x in decisions(pid, kind="negotiation_failed")
                             if x.get("task_id") == fail_id and x.get("status") == "down"), None), 15)
    s, _ = post(f"/projects/{pid}/pause", {})
    check("C5 project paused before restart", s == 200, s)
    s, t = post(f"/projects/{pid}/tasks", {
        "title": "重启前待协商任务", "description": "普通任务",
        "acceptance_criteria": "待协商",
        "executor_agent_id": ex["id"], "acceptor_agent_id": ac["id"]})
    pend_id = t.get("id")
    check("C5 pending task created while paused",
          s == 200 and t.get("negotiation_status") == "pending" and t.get("status") == "planned", f"{s} {t}")
    pend_snapshot = {
        "negotiation_status": t.get("negotiation_status"),
        "negotiation_round": t.get("negotiation_round", 0),
        "status": t.get("status"),
    }
    json.dump({"pid": pid, "exec": ex["id"], "acc": ac["id"],
               "fail_id": fail_id, "fail": fail_snapshot,
               "pend_id": pend_id, "pend": pend_snapshot}, open(STATE, "w"))
    print(f"[smoke] phase1 saved state -> {STATE}")

else:  # phase2
    st = json.load(open(STATE))
    pid, fail_id, pend_id = st["pid"], st["fail_id"], st["pend_id"]

    print("[smoke] 8) C5 — after kill -9 restart, negotiation state is preserved")
    d = detail_of(fail_id)
    t = d.get("task") or {}
    neg = d.get("negotiation") or {}
    check("C5 failed negotiation_status preserved", t.get("negotiation_status") == "failed", t.get("negotiation_status"))
    check("C5 failed negotiation_round preserved (3)", (neg.get("round") or 0) == 3, neg.get("round"))
    check("C5 failed rounds history rebuilt from events (6)", len(neg.get("rounds") or []) == 6, len(neg.get("rounds") or []))
    check("C5 failed acceptance_criteria preserved",
          (t.get("acceptance_criteria") or "") == (st["fail"]["criteria"] or ""),
          (t.get("acceptance_criteria"), st["fail"]["criteria"]))
    d = detail_of(pend_id)
    t = d.get("task") or {}
    neg = d.get("negotiation") or {}
    check("C5 pending negotiation_status preserved", t.get("negotiation_status") == "pending", t.get("negotiation_status"))
    check("C5 pending task still planned under pause", t.get("status") == "planned", t.get("status"))
    check("C5 pending has no rounds yet", len(neg.get("rounds") or []) == 0, len(neg.get("rounds") or []))
    check("C5 pending negotiation_round preserved (0)", (neg.get("round") or 0) == st["pend"]["negotiation_round"],
          neg.get("round"))

    print("[smoke] 9) C5 — restore decision maker, resume flow to completion")
    s, dm2 = post("/agents", {"name": "neg-judge-2", "adapter": "mock", "roles": ["decision_maker"], "config": {"duration_ms": 50}})
    check("C5 decision maker re-registered", s == 200 and dm2.get("id"), f"{s} {dm2}")
    s, _ = post(f"/projects/{pid}/resume", {})
    check("C5 project resumed", s == 200, s)
    t = wait_until(lambda: agreed(fail_id), 40)
    check("C5 failed negotiation adjudicated to agreed after restart", t is not None, t)
    t = wait_until(lambda: (task_of(fail_id) or {}).get("status") == "done", 40)
    check("C5 previously-failed task completed", t is not None, (task_of(fail_id) or {}).get("status"))
    t = wait_until(lambda: agreed(pend_id), 40)
    check("C5 pending negotiation completed after restart", t is not None, t)
    t = wait_until(lambda: (task_of(pend_id) or {}).get("status") == "done", 40)
    check("C5 previously-pending task completed", t is not None, (task_of(pend_id) or {}).get("status"))

failed = [n for n, ok in CHECKS if not ok]
print()
print(f"[smoke] {PHASE}: {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
if failed:
    print(f"[smoke] {PHASE} FAILED:")
    for n in failed:
        print("  - " + n)
    sys.exit(1)
print(f"[smoke] {PHASE} ALL PASS")
PY

start_server || exit 1
python3 "$PY" phase1 "$PORT" "$WS" "$STATE"
RC1=$?

# kill -9 = simulated crash; then restart against the same SQLite db.
echo "[smoke] kill -9 server (pid $SERVER_PID) and restart…"
kill -9 "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
start_server || exit 1
python3 "$PY" phase2 "$PORT" "$WS" "$STATE"
RC2=$?

if [[ $RC1 -ne 0 || $RC2 -ne 0 ]]; then
  echo "[smoke] server log tail:"
  tail -40 "$LOG" 2>/dev/null || true
fi
exit $(( RC1 | RC2 ))
