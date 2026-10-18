#!/usr/bin/env bash
# M3 milestone + gate end-to-end acceptance test.
#
# Verifies, against a real running server (mock agents):
#   1. a milestone whose tasks all reach DONE flips to `awaiting_confirm`
#      and posts a `milestone_gate` inbox item;
#   2. only the milestone's downstream branch stops being dispatched while an
#      unrelated branch keeps executing;
#   3. `kill -9` + restart preserves milestone status and the open inbox item;
#   4. resolving the gate releases the downstream branch;
#   5. a project without milestones behaves exactly like M2 (runs to completed);
#   6. the SQLite migration is idempotent across repeated boots.
#
# Usage: server/tests/milestone_gate_e2e.sh
set -euo pipefail

SERVER_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$SERVER_DIR/target/debug/domo-server"
if [[ "${DOMO_E2E_NO_BUILD:-0}" != "1" ]]; then
  echo "building domo-server…"
  cargo build --quiet --manifest-path "$SERVER_DIR/Cargo.toml"
fi
WORK="${DOMO_E2E_DIR:-/tmp/opencode/domo-milestone-e2e}"
PORT="${DOMO_E2E_PORT:-8791}"
BASE="http://127.0.0.1:$PORT"
DB="$WORK/domo.db"
LOG="$WORK/domo.log"
WS="$WORK/ws"
PID=""

pass() { printf '  \033[32mPASS\033[0m %s\n' "$1"; }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; if [[ -n "$PID" ]]; then kill -9 "$PID" 2>/dev/null || true; fi; exit 1; }
info() { printf '\n\033[1m%s\033[0m\n' "$1"; }

cleanup() { if [[ -n "$PID" ]] && kill -0 "$PID" 2>/dev/null; then kill -9 "$PID" 2>/dev/null || true; fi; }
trap cleanup EXIT

start_server() {
  pushd "$WORK" >/dev/null
  DOMO_DB="$DB" DOMO_LOG="$LOG" DOMO_PORT="$PORT" DOMO_LOG_LEVEL=warn \
    "$BIN" >>"$WORK/stdout.log" 2>&1 &
  PID=$!
  popd >/dev/null
  for _ in $(seq 1 150); do
    if curl -sf "$BASE/api/projects" >/dev/null 2>&1; then return 0; fi
    sleep 0.1
  done
  echo "server did not start"; cat "$WORK/stdout.log" 2>/dev/null; exit 1
}

kill9() {
  if [[ -n "$PID" ]] && kill -0 "$PID" 2>/dev/null; then
    kill -9 "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  PID=""
}

get()  { curl -sf "$BASE$1"; }
post() { curl -sf -X POST "$BASE$1" -H 'Content-Type: application/json' -d "$2"; }
patch() { curl -sf -X PATCH "$BASE$1" -H 'Content-Type: application/json' -d "$2"; }

# pyget DOTTED.PATH  — read JSON from stdin and print the value at path
pyget() {
  python3 -c '
import sys, json, re
path = sys.argv[1]
d = json.load(sys.stdin)
if path:
    for tok in re.findall(r"[^.\[\]]+|\[\d+\]", path):
        d = d[int(tok[1:-1])] if tok.startswith("[") else d[tok]
print(d if not isinstance(d, (dict, list)) else json.dumps(d))
' "$1"
}

task_status() { get "/api/tasks/$1" | pyget task.status; }
ms_status()   { get "/api/milestones/$1" | pyget milestone.status; }

gate_item_id() {
  # $1 = milestone id; prints open milestone_gate item id (or empty)
  get /api/inbox | python3 -c '
import sys, json
mid = sys.argv[1]
items = json.load(sys.stdin)
for i in items:
    if i.get("type") == "milestone_gate" and i.get("status") == "open" \
       and i.get("payload", {}).get("milestone_id") == mid:
        print(i["id"]); break
' "$1"
}

wait_until() { # $1 = timeout decisecs, $2 = command string evaluated
  local n="$1"; shift
  for _ in $(seq 1 "$n"); do
    if eval "$1"; then return 0; fi
    sleep 0.1
  done
  return 1
}

# ---------------------------------------------------------------- setup
rm -rf "$WORK"; mkdir -p "$WORK" "$WS"
echo "work dir: $WORK"

start_server
pass "server started (boot #1, fresh DB)"

EXEC=$(post /api/agents '{"name":"exec","adapter":"mock","roles":["executor"],"capabilities":{"max_parallel":2,"supports_resume":false},"config":{"duration_ms":300}}' | pyget id)
ACC=$(post /api/agents '{"name":"acc","adapter":"mock","roles":["acceptor"],"config":{"duration_ms":300}}' | pyget id)
[[ -n "$EXEC" && -n "$ACC" ]] || fail "agent creation"
pass "registered mock executor + acceptor"

PRJ=$(post /api/projects '{"name":"M3 gate e2e","brief":"two milestones, cross dependency"}' | pyget id)
patch "/api/projects/$PRJ" "{\"workspace\":{\"kind\":\"local\",\"path\":\"$WS\"}}" >/dev/null
post "/api/projects/$PRJ/confirm" '{}' >/dev/null
[[ "$(get "/api/projects/$PRJ" | pyget project.status)" == "running" ]] || fail "project confirm"
pass "project running with local workspace"

M1=$(post "/api/projects/$PRJ/milestones" '{"name":"M1","goal":"phase one","effect":"foundation ready"}' | pyget id)
M2=$(post "/api/projects/$PRJ/milestones" '{"name":"M2","goal":"phase two"}' | pyget id)
[[ -n "$M1" && -n "$M2" ]] || fail "milestone creation"
pass "created milestones M1=$M1 M2=$M2"

T1=$(post "/api/projects/$PRJ/tasks" "{\"title\":\"T1\",\"description\":\"first\",\"milestone_id\":\"$M1\",\"executor_agent_id\":\"$EXEC\",\"acceptor_agent_id\":\"$ACC\"}" | pyget id)
T2=$(post "/api/projects/$PRJ/tasks" "{\"title\":\"T2\",\"description\":\"second\",\"milestone_id\":\"$M1\",\"executor_agent_id\":\"$EXEC\",\"acceptor_agent_id\":\"$ACC\"}" | pyget id)
T3=$(post "/api/projects/$PRJ/tasks" "{\"title\":\"T3\",\"description\":\"cross-milestone\",\"milestone_id\":\"$M2\",\"depends_on\":[\"$T1\"],\"executor_agent_id\":\"$EXEC\",\"acceptor_agent_id\":\"$ACC\"}" | pyget id)
T4=$(post "/api/projects/$PRJ/tasks" "{\"title\":\"T4\",\"description\":\"unrelated\",\"milestone_id\":\"$M2\",\"executor_agent_id\":\"$EXEC\",\"acceptor_agent_id\":\"$ACC\"}" | pyget id)
[[ -n "$T1" && -n "$T2" && -n "$T3" && -n "$T4" ]] || fail "task creation"

# milestone task_ids mirror should reflect tasks.milestone_id
M2_IDS=$(get "/api/milestones/$M2" | pyget milestone.task_ids)
[[ "$M2_IDS" == *"$T3"* && "$M2_IDS" == *"$T4"* ]] || fail "milestone task_ids mirror ($M2_IDS)"
pass "created 4 tasks; milestone task_ids mirror is consistent"

# ---------------------------------------------------------------- gate opens
info "1) first milestone completes -> awaiting_confirm + gate item"
wait_until 300 "[[ \"\$(ms_status $M1)\" == awaiting_confirm ]]" || fail "M1 never reached awaiting_confirm (got $(ms_status "$M1"))"
pass "M1 status = awaiting_confirm"

GATE=$(gate_item_id "$M1")
[[ -n "$GATE" ]] || fail "no open milestone_gate inbox item for M1"
GTYPE=$(get /api/inbox | python3 -c 'import sys,json;items=json.load(sys.stdin);print(next(i["type"] for i in items if i["id"]=="'"$GATE"'"))')
[[ "$GTYPE" == "milestone_gate" ]] || fail "inbox item type is $GTYPE"
pass "inbox gate item present: $GATE (type=milestone_gate)"

# ---------------------------------------------------------------- branch frozen / unrelated runs
info "2) downstream branch frozen while unrelated branch keeps running"
wait_until 200 "[[ \"\$(task_status $T4)\" == done ]]" || fail "unrelated task T4 did not run (got $(task_status "$T4"))"
pass "unrelated branch task T4 reached done while gate is open"

T3S=$(task_status "$T3")
[[ "$T3S" == "ready" || "$T3S" == "planned" ]] || fail "downstream T3 should be frozen, got $T3S"
pass "downstream task T3 is frozen (status=$T3S)"

# give the scheduler a few ticks; T3 must stay frozen
sleep 1.5
T3S=$(task_status "$T3")
[[ "$T3S" == "ready" || "$T3S" == "planned" ]] || fail "downstream T3 moved while gated: $T3S"
pass "T3 still frozen after extra scheduling ticks"

# ---------------------------------------------------------------- kill -9 / restart
info "3) kill -9 + restart preserves milestone + inbox state"
kill9
sleep 0.3
start_server
pass "server restarted (boot #2, migrate ran again)"

[[ "$(ms_status "$M1")" == "awaiting_confirm" ]] || fail "M1 status lost after restart: $(ms_status "$M1")"
pass "M1 still awaiting_confirm after kill -9"

GATE2=$(gate_item_id "$M1")
[[ "$GATE2" == "$GATE" ]] || fail "gate inbox item changed after restart ($GATE -> $GATE2)"
pass "same open gate inbox item survived restart: $GATE"

T3S=$(task_status "$T3")
[[ "$T3S" == "ready" || "$T3S" == "planned" ]] || fail "T3 dispatched after restart: $T3S"
sleep 1
T3S=$(task_status "$T3")
[[ "$T3S" == "ready" || "$T3S" == "planned" ]] || fail "T3 dispatched after restart (post-tick): $T3S"
pass "downstream T3 still frozen after restart"

# ---------------------------------------------------------------- resolve gate
info "4) resolve gate -> milestone passed, downstream released"
post "/api/inbox/$GATE/resolve" '{"resolution":"pass","note":"looks good"}' >/dev/null
[[ "$(ms_status "$M1")" == "passed" ]] || fail "M1 not passed after gate resolution"
pass "M1 status = passed"

wait_until 300 "[[ \"\$(task_status $T3)\" == done ]]" || fail "downstream T3 not released after gate pass (got $(task_status "$T3"))"
pass "downstream branch released: T3 reached done"

# audit trail
EV=$(get "/api/events?project_id=$PRJ")
for ev in milestone.created milestone.status_changed milestone.gate_opened milestone.passed milestone.gate_resolved; do
  echo "$EV" | python3 -c "import sys,json;evs=json.load(sys.stdin);sys.exit(0 if any(e['type']=='$ev' for e in evs) else 1)" \
    || fail "missing audit event $ev"
done
pass "milestone.* audit events present (created/status_changed/gate_opened/passed/gate_resolved)"

# direct gate endpoints (equivalent to resolving the inbox item)
info "4b) direct /confirm + /fail endpoints"
wait_until 200 "[[ \"\$(ms_status $M2)\" == awaiting_confirm ]]" || fail "M2 never reached awaiting_confirm (got $(ms_status "$M2"))"
post "/api/milestones/$M2/fail" '{"note":"needs rework"}' >/dev/null
[[ "$(ms_status "$M2")" == "failed" ]] || fail "M2 not failed via /fail"
pass "M2 -> failed via POST /api/milestones/:id/fail"
post "/api/milestones/$M2/confirm" '{"note":"re-reviewed"}' >/dev/null
[[ "$(ms_status "$M2")" == "passed" ]] || fail "M2 not passed via /confirm"
pass "M2 -> passed via POST /api/milestones/:id/confirm (re-confirm from failed)"
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/milestones/$M2/fail" -H 'Content-Type: application/json' -d '{}')
[[ "$CODE" == "409" ]] || fail "re-failing a passed milestone should 409, got $CODE"
pass "re-failing an already-passed milestone is rejected (409)"

# ---------------------------------------------------------------- no-milestone regression
info "5) project without milestones regresses cleanly to M2 behaviour"
PRJ2=$(post /api/projects '{"name":"no milestones","brief":"regression"}' | pyget id)
patch "/api/projects/$PRJ2" "{\"workspace\":{\"kind\":\"local\",\"path\":\"$WS\"}}" >/dev/null
post "/api/projects/$PRJ2/confirm" '{}' >/dev/null
RT=$(post "/api/projects/$PRJ2/tasks" "{\"title\":\"R1\",\"description\":\"plain\",\"executor_agent_id\":\"$EXEC\",\"acceptor_agent_id\":\"$ACC\"}" | pyget id)
wait_until 300 "[[ \"\$(task_status $RT)\" == done ]]" || fail "regression task did not finish"
[[ "$(get "/api/projects/$PRJ2" | pyget project.status)" == "completed" ]] || fail "no-milestone project did not complete"
[[ "$(get "/api/projects/$PRJ2" | pyget milestones)" == "[]" ]] || fail "regression project unexpectedly has milestones"
pass "no-milestone project ran its task to done and completed"

# ---------------------------------------------------------------- migration idempotency
info "6) migration is idempotent across repeated boots"
kill9
sleep 0.3
start_server
pass "server restarted again (boot #3) without migration error"
[[ "$(ms_status "$M1")" == "passed" ]] || fail "M1 status changed on third boot"
pass "state intact after repeated migrations"

echo
printf '\033[32mALL MILESTONE GATE E2E CHECKS PASSED\033[0m\n'
