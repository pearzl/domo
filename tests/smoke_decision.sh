#!/usr/bin/env bash
# M3 决策者（decision_maker）自动裁决通道 —— 端到端验收冒烟。
#
# 覆盖验收标准：
#   1. acceptance_required=false 的任务自动生成豁免裁定请求，由决策者 agent 自动裁定，
#      全程可查（decisions 表 + decision.requested/decision.ruled 事件）；
#   2. 触及里程碑/项目目标的变更复核升级到人工收件箱，决策者不得自决；
#   3. 决策者实例缺失（被移除）或运行故障 → 收件箱出现 decision_maker_down，
#      相关分支挂起，无依赖的其他分支继续推进；恢复决策者后自动续跑；
#   4. 角色隔离（裁判不下场）：决策者实例不得被指派为 executor/acceptor，
#      登记/编辑角色/任务指派/项目池启用均被拒；DOMO_DM_ROLE_OVERLAP=1 显式打开例外。
#
# 用法：bash tests/smoke_decision.sh   （退出码 0 = 全部通过）
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# 端口默认随机取一个空闲端口：避免与其它冒烟（并行跑）撞端口 → 连到遗留实例产生假阳性
PORT="${DOMO_SMOKE_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()' 2>/dev/null || echo 8791)}"
TMP_ROOT="${TMPDIR:-/tmp}"
RUN_DIR="$(mktemp -d "$TMP_ROOT/domo_smoke.XXXXXX")"
DB="$RUN_DIR/domo.db"
WS="$RUN_DIR/workspace"
LOG="$RUN_DIR/server.log"
mkdir -p "$WS"

cleanup() {
  stop_server
}
trap cleanup EXIT

stop_server() {
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    for _ in $(seq 1 40); do
      kill -0 "$SERVER_PID" 2>/dev/null || break
      sleep 0.25
    done
    kill -9 "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=""
  fi
  # 端口必须真正释放，否则下一次启动会撞「Address already in use」
  for _ in $(seq 1 40); do
    curl -sf "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1 || return 0
    sleep 0.25
  done
  return 0
}

# start_server [额外环境变量...]：以给定环境启动服务端并等待就绪
start_server() {
  local extra=("$@")
  # 端口必须空闲：否则会连到遗留实例（历史事故：假阳性）
  if curl -sf "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1; then
    echo "[smoke] port $PORT already in use; refusing to start (would talk to a stale instance)"
    return 1
  fi
  echo "[smoke] starting server on :$PORT (db $DB${1:+ env: $1})"
  (
    cd "$ROOT/server"
    # 注意：`exec` 必须是 shell 内建（不能交给 env 去执行），
    # 且额外环境变量在为空时要整体消失（${extra[@]+...} 防空参数展开）。
    exec env DOMO_DB="$DB" DOMO_PORT="$PORT" DOMO_LOG="$LOG" DOMO_LOG_LEVEL=info \
      ${extra[@]+"${extra[@]}"} \
      "$ROOT/server/target/debug/domo-server"
  ) >"$RUN_DIR/stdout.log" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 60); do
    # 关键：进程提前退出（端口占用/启动失败）时必须报错，绝不能连到遗留实例
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "[smoke] server exited during startup; stdout tail:"
      tail -20 "$RUN_DIR/stdout.log" 2>/dev/null
      return 1
    fi
    if curl -sf "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1; then break; fi
    sleep 0.25
  done
  if ! curl -sf "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1; then
    echo "[smoke] server did not come up; log:"; tail -30 "$RUN_DIR/stdout.log"
    return 1
  fi
  return 0
}

echo "[smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[smoke] build failed"; exit 1; }

start_server || exit 1

python3 - "$PORT" "$WS" "$DB" <<'PY'
import json, os, sqlite3, sys, time, urllib.request, urllib.error

PORT, WS, DB = sys.argv[1], sys.argv[2], sys.argv[3]
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
        with urllib.request.urlopen(r, timeout=15) as resp:
            return resp.status, json.load(resp)
    except urllib.error.HTTPError as e:
        try:
            payload = json.load(e)
        except Exception:
            payload = {}
        return e.code, payload
    except Exception as e:
        return 0, {"error": str(e)}

def post(path, body): return req("POST", path, body)
def patch(path, body): return req("PATCH", path, body)
def get(path): return req("GET", path)

def wait_until(fn, timeout=25, interval=0.4):
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

def decisions(pid, **q):
    qs = "".join(f"&{k}={v}" for k, v in q.items())
    s, b = get(f"/projects/{pid}/decisions?x=1{qs}")
    return b if s == 200 else []

def event_types():
    s, b = get("/events")
    return [e["type"] for e in b] if s == 200 else []

print("[smoke] 1) agents + role isolation on registration")
s, exec_a = post("/agents", {"name": "exec-1", "adapter": "mock", "roles": ["executor"],
                                   "capabilities": {"max_parallel": 4}, "config": {"duration_ms": 50}})
check("executor agent registered", s == 200 and exec_a.get("id"), f"{s} {exec_a}")
s, acc_a = post("/agents", {"name": "acc-1", "adapter": "mock", "roles": ["acceptor"],
                                  "capabilities": {"max_parallel": 4}, "config": {"duration_ms": 50}})
check("acceptor agent registered", s == 200 and acc_a.get("id"), f"{s} {acc_a}")
s, dm_a = post("/agents", {"name": "judge-1", "adapter": "mock", "roles": ["decision_maker"], "config": {"duration_ms": 50}})
check("decision_maker agent registered", s == 200 and dm_a.get("id"), f"{s} {dm_a}")
dm_id = dm_a.get("id")
s, slow_a = post("/agents", {"name": "exec-slow", "adapter": "mock", "roles": ["executor"], "config": {"duration_ms": 3000}})
check("slow executor agent registered", s == 200 and slow_a.get("id"), f"{s} {slow_a}")

# 角色隔离（裁判不下场）：决策者 + 执行/验收 的组合登记必须被拒
s, multi_a = post("/agents", {"name": "judge-exec", "adapter": "mock", "roles": ["decision_maker", "executor"],
                              "config": {"duration_ms": 50}})
check("C4 decision_maker+executor registration rejected", s == 400 and not multi_a.get("id"),
      f"{s} {multi_a}")
s, multi_b = post("/agents", {"name": "judge-acc", "adapter": "mock", "roles": ["decision_maker", "acceptor"],
                              "config": {"duration_ms": 50}})
check("C4 decision_maker+acceptor registration rejected", s == 400 and not multi_b.get("id"),
      f"{s} {multi_b}")
# 决策者 + 规划者可以共存（规划者只提案，无决定权）
s, dm_plan = post("/agents", {"name": "judge-planner", "adapter": "mock", "roles": ["decision_maker", "planner"],
                              "config": {"duration_ms": 50}})
check("C4 decision_maker+planner registration allowed", s == 200 and dm_plan.get("id"), f"{s} {dm_plan}")

print("[smoke] 2) project bootstrap")
s, proj = post("/projects", {"name": "决策通道冒烟", "brief": "M3 decision maker smoke"})
check("project created", s == 200 and proj.get("id"), f"{s} {proj}")
pid = proj["id"]
s, _ = patch(f"/projects/{pid}", {"workspace": {"kind": "local", "path": WS}})
check("workspace set", s == 200, s)
s, _ = post(f"/projects/{pid}/confirm", {})
check("project confirmed (running)", s == 200, s)

print("[smoke] 3) C1 — exemption request auto-ruled by the decision maker")
s, t_ex = post(f"/projects/{pid}/tasks", {
    "title": "豁免验收任务", "description": "极简单的任务", "acceptance_required": False,
    "executor_agent_id": exec_a["id"],
})
check("exemption task created", s == 200 and t_ex.get("id"), f"{s} {t_ex}")
ex_id = t_ex.get("id")
d_ex = wait_until(lambda: next((d for d in decisions(pid, kind="exemption") if d["status"] == "ruled"), None), 25)
check("C1 exemption decision auto-ruled", d_ex is not None, "no ruled exemption decision within 25s")
if d_ex:
    ruling = d_ex.get("ruling") or {}
    check("C1 ruling approve by agent", ruling.get("decision") == "approve" and ruling.get("by") == "agent", ruling)
    check("C1 rationale present", bool(ruling.get("rationale")), ruling)
check("C1 decision.requested event", "decision.requested" in event_types())
check("C1 decision.ruled event", "decision.ruled" in event_types())
# 新语义：任务在高优先级 agent 处排队（含计划/审查阶段），等待预算放宽
t = wait_until(lambda: (task_of(ex_id) or {}).get("status") == "done" and task_of(ex_id), 150)
check("C1 exempted task ran to done without acceptance", t is not None,
      (task_of(ex_id) or {}).get("status"))
if t:
    check("C1 acceptance_required stayed false", t.get("acceptance_required") is False, t.get("acceptance_required"))

print("[smoke] 4) C2 — acceptance-criteria negotiation failure adjudication")
s, t_neg = post(f"/projects/{pid}/tasks", {
    "title": "协商失败任务", "description": "验收标准谈不拢", "acceptance_required": True,
    "negotiation_status": "failed", "acceptance_criteria": "执行者提议的标准",
    "executor_agent_id": exec_a["id"], "acceptor_agent_id": acc_a["id"],
})
check("negotiation-failed task created", s == 200 and t_neg.get("id"), f"{s} {t_neg}")
neg_id = t_neg.get("id")
d_neg = wait_until(lambda: next((d for d in decisions(pid, kind="negotiation_failed") if d["status"] == "ruled"), None), 25)
check("C2 negotiation decision auto-ruled", d_neg is not None, "no ruled negotiation decision within 25s")
if d_neg:
    check("C2 ruling recorded", (d_neg.get("ruling") or {}).get("decision") in ("approve", "deny"), d_neg.get("ruling"))
t = wait_until(lambda: (task_of(neg_id) or {}).get("negotiation_status") == "agreed" and task_of(neg_id), 25)
check("C2 negotiation settled → agreed", t is not None, (task_of(neg_id) or {}).get("negotiation_status"))
if t:
    check("C2 criteria written back", bool(t.get("acceptance_criteria")), t)
t = wait_until(lambda: (task_of(neg_id) or {}).get("status") == "done", 30)
check("C2 task completed after adjudication", t is not None, (task_of(neg_id) or {}).get("status"))

print("[smoke] 5) C3 — task-level change review: tactical self-ruling vs strategic escalation")
s, t_plain = post(f"/projects/{pid}/tasks", {
    "title": "战术变更目标", "description": "普通任务", "acceptance_required": True,
    "acceptance_criteria": "改动标题即可",
    "executor_agent_id": slow_a["id"], "acceptor_agent_id": acc_a["id"],
    "depends_on": [neg_id],
})
check("tactical target task created", s == 200 and t_plain.get("id"), f"{s} {t_plain}")
plain_id = t_plain.get("id")
s, body = post(f"/tasks/{plain_id}/decisions", {
    "kind": "change_review", "proposal": {"title": "战术变更后标题", "description": "改描述"},
})
check("tactical change review accepted", s == 200 and body.get("decision", {}).get("id"), f"{s} {body}")
check("C3 tactical classified", body.get("decision", {}).get("scope") == "tactical", body)
d_tac = wait_until(lambda: next((d for d in decisions(pid, kind="change_review") if d["id"] == body.get("decision", {}).get("id") and d["status"] == "ruled"), None), 25)
check("C3 tactical change self-ruled by agent", d_tac is not None, "tactical change was not ruled")
t = wait_until(lambda: (task_of(plain_id) or {}).get("title") == "战术变更后标题", 15)
check("C3 tactical change applied to the task", t is not None, (task_of(plain_id) or {}).get("title"))

s, body = post(f"/tasks/{plain_id}/decisions", {
    "kind": "change_review", "proposal": {"milestone_id": "ms_not_mine", "title": "还想改标题"},
})
s2, body2 = post(f"/projects/{pid}/decisions", {
    "kind": "change_review", "task_id": plain_id,
    "proposal": {"project": {"brief": "改掉项目目标"}, "title": "目标级改动"},
})
check("strategic change review accepted", s == 200 and s2 == 200, f"{s} {body} / {s2} {body2}")
str_ids = [b.get("decision", {}).get("id") for b in (body, body2) if b.get("decision")]
check("C3 strategic classified", all(b.get("decision", {}).get("scope") == "strategic" for b in (body, body2)),
      f"{body} / {body2}")
time.sleep(3)
d_str = [d for d in decisions(pid, kind="change_review") if d["id"] in str_ids]
check("C3 strategic NOT self-ruled", d_str and all(d["status"] == "escalated" for d in d_str),
      [d["status"] for d in d_str])
s, inbox = get("/inbox")
open_dec = [i for i in inbox if i.get("status") == "open" and i.get("payload", {}).get("decision_id") in str_ids]
check("C3 strategic landed in human inbox", len(open_dec) == len(str_ids), f"open={len(open_dec)} want={len(str_ids)}")
check("C3 decision.escalated event", "decision.escalated" in event_types())
check("C3 target task unchanged", (task_of(plain_id) or {}).get("title") == "战术变更后标题",
      (task_of(plain_id) or {}).get("title"))

if open_dec:
    item = open_dec[0]
    s, res = post(f"/inbox/{item['id']}/resolve", {"resolution": "approve", "note": "人工批准战略变更"})
    check("human resolve of strategic decision", s == 200, f"{s} {res}")
    s, dl = get(f"/projects/{pid}/decisions")
    dd = next((d for d in dl if d["id"] == item["payload"]["decision_id"]), None)
    check("C3 human ruling recorded (by=human)", dd and dd["status"] == "ruled"
          and (dd.get("ruling") or {}).get("by") == "human", dd)
    check("C3 strategic change NOT auto-applied",
          dd and ((dd.get("ruling") or {}).get("effect") or {}).get("effect") == "strategic_change_approved",
          dd and dd.get("ruling"))

print("[smoke] 6) C4 — decision maker may not be assigned as executor/acceptor")
# 6a) 角色编辑：把已登记的决策者实例加上 executor 角色 → 必须被拒
s, body = patch(f"/agents/{dm_id}", {"roles": ["decision_maker", "executor"]})
check("C4 adding executor role to a decision maker rejected", s == 400, f"{s} {body}")
s, body = patch(f"/agents/{dm_id}", {"roles": ["decision_maker", "acceptor"]})
check("C4 adding acceptor role to a decision maker rejected", s == 400, f"{s} {body}")
s, body = get(f"/agents")
check("C4 decision maker roles unchanged after rejected edits",
      any(a["id"] == dm_id and a["roles"] == ["decision_maker"] for a in body), body)

# 6b) 任务指派：直接指派决策者实例 → 必须被拒
s, body = post(f"/projects/{pid}/tasks", {
    "title": "决策者下场探针", "description": "探针", "acceptance_required": False,
    "executor_agent_id": dm_id,
})
check("C4 assigning a decision maker as executor rejected",
      s == 400 and "decision_maker" in str(body.get("error", "")), f"{s} {body}")
s, body = post(f"/projects/{pid}/tasks", {
    "title": "决策者下场探针2", "description": "探针", "acceptance_required": True,
    "executor_agent_id": exec_a["id"], "acceptor_agent_id": dm_id,
})
check("C4 assigning a decision maker as acceptor rejected",
      s == 400 and "decision_maker" in str(body.get("error", "")), f"{s} {body}")

# 6c) 历史数据兜底：把决策者实例的角色直接改成「决策者+执行者」（绕过 API），
#     指派仍必须被拒，动态选人也必须跳过它。
legacy_ok = False
try:
    con = sqlite3.connect(DB, timeout=10)
    con.execute("UPDATE agents SET roles=? WHERE id=?",
                (json.dumps(["decision_maker", "executor"]), dm_id))
    con.commit()
    con.close()
    legacy_ok = True
except Exception as e:  # pragma: no cover
    print(f"  WARN  could not prepare legacy roles: {e}", flush=True)
check("legacy dual-role instance prepared", legacy_ok)
s, body = post(f"/projects/{pid}/tasks", {
    "title": "历史双角色探针", "description": "探针", "acceptance_required": False,
    "executor_agent_id": dm_id,
})
check("C4 legacy dual-role instance rejected as executor",
      s == 400 and "decision_maker" in str(body.get("error", "")), f"{s} {body}")
# 动态选人：未显式指派执行者的任务不得被派给决策者实例
s, body = post(f"/projects/{pid}/tasks", {
    "title": "动态选人探针", "description": "探针", "acceptance_required": True,
    "acceptance_criteria": "直接完成",
})
check("dynamic-assignment probe task created", s == 200 and body.get("id"), f"{s} {body}")
probe_id = body.get("id")
t = wait_until(lambda: (task_of(probe_id) or {}).get("executor_agent_id") and task_of(probe_id), 20)
check("C4 dynamic assignment skips decision maker instances",
      t is not None and t.get("executor_agent_id") != dm_id, t and t.get("executor_agent_id"))
# 项目 agent 池：把决策者实例放进 executor 优先级列表 → 必须被拒
s, body = req("PUT", f"/projects/{pid}/pool", {
    "executor": [dm_id, exec_a["id"]], "acceptor": [acc_a["id"]],
    "planner": [], "decision_maker": [dm_id],
})
check("C4 enabling a decision maker in the executor pool rejected", s == 400, f"{s} {body}")

print("[smoke] 7) C3 — decision maker down: inbox item + branch isolation")
# 决策者故障降级：移除唯一 DM 实例。
# 先把 C4 段登记的「决策者+规划者」实例摘掉 decision_maker 角色（DM+planner 本身合法，
# 但只要它还持 decision_maker 角色就仍能裁定，本用例的前提是「一个决策者都不剩」）。
if dm_plan.get("id"):
    s, body = patch(f"/agents/{dm_plan.get('id')}", {"roles": ["planner"]})
    check("C4 decision_maker+planner instance demoted before the DM-down case", s == 200, f"{s} {body}")
s, delres = req("DELETE", f"/agents/{dm_id}", None)
check("decision maker removed", s == 200, f"{s} {delres}")
_remaining_dm = [a for a in get("/agents")[1] if "decision_maker" in (a.get("roles") or [])]
check("no decision maker instance left", not _remaining_dm, _remaining_dm)
s, t_down = post(f"/projects/{pid}/tasks", {
    "title": "决策者故障期任务", "description": "需要豁免裁定", "acceptance_required": False,
    "executor_agent_id": exec_a["id"],
})
check("exemption task created while DM is gone", s == 200 and t_down.get("id"), f"{s} {t_down}")
down_id = t_down.get("id")
s, t_other = post(f"/projects/{pid}/tasks", {
    "title": "无关分支任务", "description": "不依赖任何决策", "acceptance_required": True,
    "acceptance_criteria": "直接完成", "executor_agent_id": exec_a["id"], "acceptor_agent_id": acc_a["id"],
})
check("unrelated branch task created", s == 200 and t_other.get("id"), f"{s} {t_other}")
other_id = t_other.get("id")

item = wait_until(lambda: next((i for i in get("/inbox")[1]
                                if i.get("status") == "open" and i.get("type") == "decision_maker_down"), None), 25)
check("C3 decision_maker_down inbox item", item is not None, "no decision_maker_down item within 25s")
check("C3 decision.maker_down event", "decision.maker_down" in event_types())
t = wait_until(lambda: (task_of(down_id) or {}).get("status") == "suspended", 15)
check("C3 related branch suspended", t is not None, (task_of(down_id) or {}).get("status"))
t = wait_until(lambda: (task_of(other_id) or {}).get("status") == "done", 30)
check("C3 unrelated branch keeps running", t is not None, (task_of(other_id) or {}).get("status"))
s, dl = get(f"/projects/{pid}/decisions")
down_dec = next((d for d in dl if d["kind"] == "exemption" and d["status"] == "down"
                 and d.get("task_id") == down_id), None)
check("C3 decision status == down", down_dec is not None,
      [(d["kind"], d["status"], d.get("task_id")) for d in dl])
down_dec_id = down_dec["id"] if down_dec else None

print("[smoke] 8) recovery — register the decision maker again")
s, dm2 = post("/agents", {"name": "judge-2", "adapter": "mock", "roles": ["decision_maker"], "config": {"duration_ms": 50}})
check("decision maker re-registered", s == 200 and dm2.get("id"), f"{s} {dm2}")
d = wait_until(lambda: next((d for d in decisions(pid, kind="exemption")
                              if d["id"] == down_dec_id and d["status"] == "ruled"), None), 25) if down_dec_id else None
check("stuck decision re-queued and ruled", d is not None, "down decision did not resume after DM re-registration")
t = wait_until(lambda: (task_of(down_id) or {}).get("status") == "done", 150)
check("suspended branch resumed to done", t is not None, (task_of(down_id) or {}).get("status"))
if item:
    s, inbox = get("/inbox")
    closed = next((i for i in inbox if i["id"] == item["id"]), None)
    check("decision_maker_down item auto-closed after ruling", closed and closed["status"] == "resolved", closed)

print("[smoke] 9) C3 — decision maker run failure → decision_maker_down")
# 决策者实例在，但每次裁定都失败（模拟 LLM 调用异常）→ 达上限后转 decision_maker_down
s, dm3 = post("/agents", {"name": "judge-broken", "adapter": "mock", "roles": ["decision_maker"],
                          "config": {"duration_ms": 50, "adjudication_fail": True}})
check("broken decision maker registered", s == 200 and dm3.get("id"), f"{s} {dm3}")
# 摘掉健康的决策者，只留故障实例
s, _ = req("DELETE", f"/agents/{dm2.get('id')}", None)
check("healthy decision maker removed", s == 200, s)
s, t_fail = post(f"/projects/{pid}/tasks", {
    "title": "决策者运行故障任务", "description": "需要豁免裁定", "acceptance_required": False,
    "executor_agent_id": exec_a["id"],
})
check("exemption task created while DM is broken", s == 200 and t_fail.get("id"), f"{s} {t_fail}")
fail_id = t_fail.get("id")
s, t_other2 = post(f"/projects/{pid}/tasks", {
    "title": "故障期无关分支", "description": "不依赖任何决策", "acceptance_required": True,
    "acceptance_criteria": "直接完成", "executor_agent_id": exec_a["id"], "acceptor_agent_id": acc_a["id"],
})
other2_id = t_other2.get("id")
item2 = wait_until(lambda: next((i for i in get("/inbox")[1]
                                 if i.get("status") == "open" and i.get("type") == "decision_maker_down"
                                 and i.get("payload", {}).get("task_id") == fail_id), None), 40)
check("C3 broken decision maker → decision_maker_down inbox item", item2 is not None,
      "no decision_maker_down item for the failing decision maker")
t = wait_until(lambda: (task_of(fail_id) or {}).get("status") == "suspended", 20)
check("C3 broken decision maker suspends the related branch", t is not None, (task_of(fail_id) or {}).get("status"))
t = wait_until(lambda: (task_of(other2_id) or {}).get("status") == "done", 40)
check("C3 unrelated branch keeps running during decision maker failure", t is not None,
      (task_of(other2_id) or {}).get("status"))
s, dl = get(f"/projects/{pid}/decisions")
fail_dec = next((d for d in dl if d["kind"] == "exemption" and d["status"] == "down"
                 and d.get("task_id") == fail_id), None)
check("C3 failed decision status == down", fail_dec is not None,
      [(d["kind"], d["status"], d.get("task_id")) for d in dl])

print("[smoke] 10) audit trail completeness")
s, dl = get(f"/projects/{pid}/decisions")
kinds = sorted({d["kind"] for d in dl})
check("decision kinds queryable", {"change_review", "exemption"} <= set(kinds), kinds)
check("every decision has a scope", all(d.get("scope") in ("tactical", "strategic") for d in dl), dl)
check("decision.requested/ruled/escalated/maker_down events all emitted",
      all(e in event_types() for e in
          ["decision.requested", "decision.ruled", "decision.escalated", "decision.maker_down"]))

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
  exit $RC
fi

# ---------- 例外开关：DOMO_DM_ROLE_OVERLAP=1 显式打开「决策者兼任」 ----------
echo "[smoke] 11) exception mode (DOMO_DM_ROLE_OVERLAP=1) — BYO 池过小时的显式例外"
stop_server
start_server DOMO_DM_ROLE_OVERLAP=1 || exit 1

python3 - "$PORT" <<'PY'
import json, sys, time, urllib.request, urllib.error

PORT = sys.argv[1]
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
        with urllib.request.urlopen(r, timeout=15) as resp:
            return resp.status, json.load(resp)
    except urllib.error.HTTPError as e:
        try:
            payload = json.load(e)
        except Exception:
            payload = {}
        return e.code, payload
    except Exception as e:
        return 0, {"error": str(e)}

def post(path, body): return req("POST", path, body)
def get(path): return req("GET", path)

s, dual = post("/agents", {"name": "judge-exec", "adapter": "mock",
                           "roles": ["decision_maker", "executor"], "config": {"duration_ms": 50}})
check("exception mode allows decision_maker+executor registration", s == 200 and dual.get("id"), f"{s} {dual}")
# 验收者探针要用「决策者+验收者」实例（执行者实例没有 acceptor 角色，指派会被角色校验拒绝）
s, dual_acc = post("/agents", {"name": "judge-acc", "adapter": "mock",
                               "roles": ["decision_maker", "acceptor"], "config": {"duration_ms": 50}})
check("exception mode allows decision_maker+acceptor registration", s == 200 and dual_acc.get("id"), f"{s} {dual_acc}")
s, proj = post("/projects", {"name": "例外模式", "brief": "decision maker overlap allowed"})
pid = proj.get("id")
s, _ = req("PATCH", f"/projects/{pid}", {"workspace": {"kind": "local", "path": "/tmp"}})
s, _ = post(f"/projects/{pid}/confirm", {})
check("project confirmed", s == 200, s)
s, t = post(f"/projects/{pid}/tasks", {
    "title": "兼任执行者任务", "description": "例外模式下允许", "acceptance_required": False,
    "executor_agent_id": dual.get("id"),
})
check("exception mode allows assigning a decision maker as executor",
      s == 200 and t.get("executor_agent_id") == dual.get("id"), f"{s} {t}")
s, t2 = post(f"/projects/{pid}/tasks", {
    "title": "兼任验收者任务", "description": "例外模式下允许", "acceptance_required": True,
    "acceptance_criteria": "直接完成", "acceptor_agent_id": dual_acc.get("id"),
})
check("exception mode allows assigning a decision maker as acceptor",
      s == 200 and t2.get("acceptor_agent_id") == dual_acc.get("id"), f"{s} {t2}")

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
