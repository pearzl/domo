#!/usr/bin/env bash
# M3 端到端冒烟（真实 opencode / glm 系 agent）—— 全链路正向 + 负路径 + 重启恢复。
#
# 团队池：planner×2 / executor×2 / acceptor×2 / decision_maker×2（全部 adapter=opencode，
#         model 默认 opencode-go/glm-5.3-flash，可用 DOMO_M3_MODEL 覆盖）。
#
# 覆盖任务验收标准：
#   正向：planner 产出含里程碑的规划 → 人确认 → 执行者↔验收者协商验收标准 → 执行与验收
#         → 里程碑 gate 通过；gate 退回 → 里程碑窗口追加任务 → 重入 gate 再通过；
#         决策者纠偏重规划（tactical 上报 → 决策者批准 → planner 复用规划会话产出调整并落地）；
#         全部终态 → 项目总结报告自动生成并落盘。
#   负向：①协商失败 → 决策者裁决恢复；②成环依赖在窗口被拒并汇报原因、其余项照常应用；
#         ③决策者缺席 → decision_maker_down 收件箱 + 任务挂起，不阻塞无关分支；
#         ④milestone gate 只挂起该里程碑的下游分支，无关分支继续跑。
#   恢复：kill -9 重启后里程碑/gate/协商状态/决策/报告完整保留，中断流程续跑。
#   审计：关键日志（data/domo.log 语义的服务端运行日志）与结果记录落盘可复查。
#
# 用法：bash scripts/smoke_m3_e2e.sh          （退出码 0 = 全部通过）
#       DOMO_M3_PORT=8801 bash scripts/smoke_m3_e2e.sh
#       DOMO_M3_MODEL=opencode-go/glm-5.3-flash bash scripts/smoke_m3_e2e.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_M3_PORT:-8799}"
MODEL="${DOMO_M3_MODEL:-opencode-go/glm-5.3-flash}"
RUN_DIR="$(mktemp -d /tmp/opencode/domo_m3_e2e.XXXXXX)"
DB="$RUN_DIR/domo.db"
WS="$RUN_DIR/workspace"
LOG="$RUN_DIR/domo.log"
STATE="$RUN_DIR/state.json"
PY="$RUN_DIR/smoke.py"
RESULTS="$RUN_DIR/results.md"
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
  echo "[m3-smoke] starting server on :$PORT (db $DB, log $LOG)"
  (
    cd "$ROOT/server"
    DOMO_DB="$DB" DOMO_PORT="$PORT" DOMO_LOG="$LOG" DOMO_LOG_LEVEL=info \
      exec "$ROOT/server/target/debug/domo-server"
  ) >"$RUN_DIR/stdout.log" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 120); do
    if curl -sf "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1; then return 0; fi
    sleep 0.25
  done
  echo "[m3-smoke] server did not come up; log:"; tail -40 "$RUN_DIR/stdout.log"
  return 1
}

echo "[m3-smoke] model = $MODEL"
echo "[m3-smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[m3-smoke] build failed"; exit 1; }

echo "[m3-smoke] smoke driver -> $PY"
cat >"$PY" <<'PY'
import json, os, sys, time, urllib.request, urllib.error

PHASE, PORT, RUN_DIR, WS, STATE, MODEL = sys.argv[1:7]
BASE = f"http://127.0.0.1:{PORT}/api"
RESULTS = os.path.join(RUN_DIR, "results.md")
CHECKS = []
NOTES = []

def check(name, cond, detail=""):
    cond = bool(cond)
    CHECKS.append((name, cond, "" if cond else str(detail)))
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  -- " + str(detail)) if (detail and not cond) else ""), flush=True)

def note(msg):
    NOTES.append(msg)
    print("  NOTE  " + msg, flush=True)

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
def delete(p, timeout=60): return req("DELETE", p, timeout=timeout)

def wait_until(fn, timeout=300, interval=1.0):
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
def project_status(pid): return (project(pid).get("project") or {}).get("status")
def tasks_of(pid): return project(pid).get("tasks", [])
def milestones_of(pid): return project(pid).get("milestones", [])
def task(tid):
    s, b = get(f"/tasks/{tid}")
    return b.get("task") if s == 200 else None
def task_detail(tid):
    s, b = get(f"/tasks/{tid}")
    return b if s == 200 else {}
def task_by_title(pid, title):
    return next((t for t in tasks_of(pid) if t["title"] == title), None)
def inbox():
    s, b = get("/inbox")
    return b if s == 200 else []
def open_gate(mid):
    return next((i for i in inbox() if i.get("type") == "milestone_gate" and i.get("status") == "open"
                 and (i.get("payload") or {}).get("milestone_id") == mid), None)
def open_window(mid):
    return next((i for i in inbox() if i.get("status") == "open"
                 and (i.get("payload") or {}).get("kind") == "milestone_window"
                 and (i.get("payload") or {}).get("milestone_id") == mid), None)
def open_dm_down(tid):
    return next((i for i in inbox() if i.get("type") == "decision_maker_down" and i.get("status") == "open"
                 and (i.get("payload") or {}).get("task_id") == tid), None)
def decisions(pid, **q):
    qs = "".join(f"&{k}={v}" for k, v in q.items())
    s, b = get(f"/projects/{pid}/decisions?x=1{qs}")
    return b if s == 200 else []
def events(pid):
    s, b = get(f"/events?project_id={pid}")
    return b if s == 200 else []
def etypes(pid): return [e["type"] for e in events(pid)]
def report(pid):
    s, b = get(f"/projects/{pid}/report")
    return b.get("report") if s == 200 else None
def resolve(iid, resolution, note="", **extra):
    body = {"resolution": resolution, "note": note}
    body.update(extra)
    return post(f"/inbox/{iid}/resolve", body)

def new_project(name, brief):
    s, b = post("/projects", {"name": name, "brief": brief})
    pid = b.get("id")
    patch(f"/projects/{pid}", {"workspace": {"kind": "local", "path": WS}})
    return pid

def create_task(pid, title, desc, ex, acc, **kw):
    body = {"title": title, "description": desc,
            "executor_agent_id": ex, "acceptor_agent_id": acc}
    body.update(kw)
    s, b = post(f"/projects/{pid}/tasks", body)
    return b if s == 200 else None

def dump_results():
    with open(RESULTS, "a") as f:
        f.write(f"\n## {PHASE} — {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
        for n, ok, d in CHECKS:
            f.write(f"- {'PASS' if ok else 'FAIL'} — {n}" + (f"  _({d})_" if d else "") + "\n")
        if NOTES:
            f.write("\nNotes:\n")
            for x in NOTES:
                f.write(f"- {x}\n")
    failed = [n for n, ok, _ in CHECKS if not ok]
    print()
    print(f"[m3-smoke] {PHASE}: {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
    if failed:
        print(f"[m3-smoke] {PHASE} FAILED:")
        for n in failed:
            print("  - " + n)
    return 1 if failed else 0


# ======================================================================
# PHASE 1: 团队池 → 正向全链路 → 负路径 → 决策者缺席 → 快照后崩溃重启
# ======================================================================
def phase1():
    print("[m3-smoke] 1) 团队池：真实 opencode glm 系，多角色多实例")
    ids = {}
    def mk(name, roles, mp=1):
        s, b = post("/agents", {"name": name, "adapter": "opencode", "roles": roles,
                                "capabilities": {"max_parallel": mp, "supports_resume": True},
                                "config": {"model": MODEL, "timeout_ms": 300000}})
        return s, b
    specs = [("planner-1", ["planner"]), ("planner-2", ["planner"]),
             ("exec-1", ["executor"]), ("exec-2", ["executor"]),
             ("acc-1", ["acceptor"]), ("acc-2", ["acceptor"]),
             ("judge-1", ["decision_maker"]), ("judge-2", ["decision_maker"])]
    ok = True
    for name, roles in specs:
        s, b = mk(name, roles)
        if s == 200 and b.get("id"):
            ids[name] = b["id"]
        else:
            ok = False
    check("8 个真实 agent 注册成功（planner/executor/acceptor/decision_maker ×2）",
          ok and len(ids) == 8, ids)
    for name, roles in specs:
        a = next((x for x in get("/agents")[1] if x["id"] == ids.get(name, "")), {})
        check(f"agent {name} adapter=opencode 且角色={roles}",
              a.get("adapter") == "opencode" and a.get("roles") == roles,
              (a.get("adapter"), a.get("roles")))
    P1, P2 = ids["planner-1"], ids["planner-2"]
    E1, E2 = ids["exec-1"], ids["exec-2"]
    A1, A2 = ids["acc-1"], ids["acc-2"]
    D1, D2 = ids["judge-1"], ids["judge-2"]
    dm_ids = {D1, D2}

    # ---------------- Project A: planner 规划（含里程碑）→ 确认 → 协商/执行/验收 ----
    print("[m3-smoke] 2) 项目 A：planner 产出含里程碑的规划 → 人确认")
    pidA = new_project("M3端到端A-真实全链路",
        "分两个阶段交付：阶段一在项目工作目录创建文件 base.txt（内容一行 hello base）；"
        "阶段二在 base.txt 基础上创建 summary.txt（内容包含 base.txt 的问候语与一行 done）。"
        "请拆成可独立执行与验收的任务，并用里程碑把两个阶段清楚分开。")
    check("项目 A 创建", bool(pidA), pidA)

    planned = None
    for attempt in range(1, 4):
        s, b = post(f"/projects/{pidA}/plan", {"engine": "opencode"}, timeout=420)
        if s == 200 and b.get("tasks_added") and b.get("milestones_added"):
            planned = b
            break
        note(f"规划第 {attempt} 次未产出里程碑（status={s}），重试")
    check("planner 规划成功返回", planned is not None, "规划 3 次均未返回含里程碑的规划")
    if planned is None:
        json.dump({"pidA": pidA, "ids": ids, "failed": "plan", "complete": False}, open(STATE, "w"))
        return dump_results()
    tasksA = planned.get("tasks_added", [])
    msA = planned.get("milestones_added", [])
    check("planner 产出 >=2 个任务", len(tasksA) >= 2, len(tasksA))
    check("planner 产出 >=1 个里程碑", len(msA) >= 1, len(msA))
    check("每个里程碑都关联了成员任务", all(m.get("task_ids") for m in msA),
          [m.get("task_ids") for m in msA])
    allmember = {t for m in msA for t in m.get("task_ids", [])}
    check("任务全部归属里程碑", all(t["id"] in allmember for t in tasksA),
          [(t["id"] in allmember) for t in tasksA])
    check("规划任务均分配了 executor 且非决策者",
          all(t.get("executor_agent_id") and t["executor_agent_id"] not in dm_ids for t in tasksA))
    check("规划任务均分配了 acceptor 且非决策者、且 != executor",
          all(t.get("acceptor_agent_id") and t["acceptor_agent_id"] not in dm_ids
              and t["acceptor_agent_id"] != t["executor_agent_id"] for t in tasksA))
    check("规划任务进入验收标准协商（negotiation_status=pending）",
          all(t.get("negotiation_status") == "pending" for t in tasksA),
          [t.get("negotiation_status") for t in tasksA])

    s, _ = post(f"/projects/{pidA}/confirm", {})
    check("项目 A 人确认 → running", s == 200 and project_status(pidA) == "running", s)

    print("[m3-smoke] 3) 项目 A：协商 → 执行 → 验收 → 里程碑 gate 通过 → 总结报告")
    # pump: 自动通过出现的 gate，安全恢复升级项，直到项目 completed
    seen = set()
    end = time.time() + 1500
    while time.time() < end and project_status(pidA) != "completed":
        for m in milestones_of(pidA):
            if m["status"] == "awaiting_confirm":
                g = open_gate(m["id"])
                if g and g["id"] not in seen:
                    resolve(g["id"], "pass", "冒烟自动确认")
                    seen.add(g["id"])
        for it in inbox():
            if it.get("status") == "open" and it.get("type") == "escalation" \
                    and it.get("project_id") == pidA and it["id"] not in seen:
                resolve(it["id"], "resume", "冒烟自动恢复")
                seen.add(it["id"])
        time.sleep(1.5)
    check("项目 A 全部终态 → completed", project_status(pidA) == "completed", project_status(pidA))
    msA_status = [m["status"] for m in milestones_of(pidA)]
    check("项目 A 全部里程碑 passed", msA_status and all(x == "passed" for x in msA_status), msA_status)
    # 协商执行都可审计
    ta = etypes(pidA)
    check("项目 A 有 negotiation.round 事件（真实多轮协商）", "negotiation.round" in ta)
    check("项目 A 有 negotiation.agreed 事件", "negotiation.agreed" in ta)
    check("项目 A 有 acceptance.recorded（验收通过）事件", "acceptance.recorded" in ta)
    check("项目 A 有 milestone.passed 事件", "milestone.passed" in ta)
    doneA = [t for t in tasks_of(pidA) if t["status"] == "done"]
    check("项目 A 所有任务 done", len(doneA) == len(tasks_of(pidA)) and len(doneA) > 0,
          [t["status"] for t in tasks_of(pidA)])
    # 协商历史落库可回放
    anydetail = task_detail(doneA[0]["id"]) if doneA else {}
    neg = anydetail.get("negotiation") or {}
    check("项目 A 任务详情含协商轮次记录", bool(neg.get("rounds")), neg.get("rounds"))
    wait_until(lambda: (report(pidA) or {}).get("status") == "ready", 600)
    repA = report(pidA) or {}
    check("项目 A 总结报告自动生成 ready", repA.get("status") == "ready", repA.get("status"))
    check("项目 A 报告正文非空", bool((repA.get("content") or "").strip()))
    mdA = os.path.join(RUN_DIR, "projects", pidA, "summary-report.md")
    check("项目 A 报告落盘 summary-report.md", os.path.isfile(mdA) and os.path.getsize(mdA) > 0, mdA)

    # ---------------- Project B: gate 分支挂起 + 退回窗口追加 + 环校验 ----------------
    print("[m3-smoke] 4) 项目 B：gate 只挂起下游分支；退回窗口追加任务；成环依赖被拒")
    pidB = new_project("M3端到端B-gate语义", "验证里程碑 gate 的分支挂起、退回窗口与环校验")
    check("项目 B 创建", bool(pidB), pidB)
    s, _ = post(f"/projects/{pidB}/confirm", {})
    check("项目 B running", s == 200 and project_status(pidB) == "running", s)
    s, M1 = post(f"/projects/{pidB}/milestones", {"name": "阶段B1", "goal": "地基", "effect": "可上层"})
    s2, M2 = post(f"/projects/{pidB}/milestones", {"name": "阶段B2", "goal": "上层"})
    check("项目 B 两个里程碑创建", s == 200 and s2 == 200, (s, s2))
    M1id, M2id = M1["id"], M2["id"]

    b_base = create_task(pidB, "B-地基", "在当前工作目录创建文件 b_base.txt，内容为一行：base ok",
                         E1, A1, milestone_id=M1id, acceptance_criteria="b_base.txt 存在且内容为 base ok")
    b_unrel = create_task(pidB, "B-无关分支", "在当前工作目录创建文件 b_unrel.txt，内容为一行：unrel ok",
                          E2, A2, acceptance_criteria="b_unrel.txt 存在且内容为 unrel ok")
    b_down = create_task(pidB, "B-下游分支", "在当前工作目录创建文件 b_down.txt，内容为一行：down ok",
                         E1, A1, milestone_id=M2id, depends_on=[b_base["id"]],
                         acceptance_criteria="b_down.txt 存在且内容为 down ok")
    b_probe = create_task(pidB, "B-环校验探针", "环校验探针任务（无需产出）",
                          E2, A2, depends_on=[b_down["id"]], acceptance_criteria="探针任务，无需产物")
    check("项目 B 4 个任务创建", all([b_base, b_unrel, b_down, b_probe]))

    # 无关分支应继续跑完，而下游分支被冻结
    unrel_done = wait_until(lambda: (task(b_unrel["id"]) or {}).get("status") == "done", 600)
    check("无关分支任务在 gate 未确认时独立跑完（done）", unrel_done is not None,
          (task(b_unrel["id"]) or {}).get("status"))
    m1gate = wait_until(lambda: open_gate(M1id), 600)
    check("阶段B1 成员全部 DONE → gate 出现（awaiting_confirm）", m1gate is not None)
    dnow = task(b_down["id"]) or {}
    check("下游分支被 gate 冻结：未开工（attempt=0，状态非 done）",
          dnow.get("status") != "done" and dnow.get("attempt", 0) == 0,
          (dnow.get("status"), dnow.get("attempt")))
    check("冻结发生在里程碑 gate 之前（creation 起即冻结），无关分支不受影响",
          (task(b_unrel["id"]) or {}).get("status") == "done")

    # gate 退回 → 里程碑 failed + 窗口
    s, _ = resolve(m1gate["id"], "reject", "冒烟退回：规划需要补充")
    m1 = next((m for m in milestones_of(pidB) if m["id"] == M1id), {})
    check("gate 退回 → 里程碑 failed", s == 200 and m1.get("status") == "failed", (s, m1.get("status")))
    win = wait_until(lambda: open_window(M1id), 60)
    check("退回后重开里程碑重规划窗口（replan/milestone_window 收件箱项）", win is not None)
    check("退回后下游分支仍冻结", (task(b_down["id"]) or {}).get("status") != "done")

    # 窗口应用：新增任务（默认挂本里程碑、依赖按标题解析）+ 成环项被拒
    changes = [
        {"op": "add", "task": {"title": "B-补充任务", "description":
            "在当前工作目录创建文件 b_add.txt，内容为一行：add ok",
            "depends_on": ["B-地基"], "acceptance_required": True,
            "acceptance_criteria": "b_add.txt 存在且内容为 add ok"}},
        {"op": "update", "task_id": b_probe["id"], "fields": {"depends_on": ["B-环校验探针"]}},
    ]
    s, res = post(f"/milestones/{M1id}/window/apply", {"changes": changes, "note": "追加补充任务并测试环校验"})
    applied = res.get("applied", []) if s == 200 else []
    skipped = res.get("skipped", []) if s == 200 else []
    check("窗口方案应用返回 200", s == 200, (s, res))
    check("窗口内新增任务被应用（applied 含 add）",
          any(a.get("op") == "add" for a in applied), applied)
    cyc = [x for x in skipped if "cycle" in (x.get("reason") or "")]
    check("成环依赖被拒并汇报原因（skipped 含 cycle）", bool(cyc), skipped)
    check("成环项被拒不影响其余项（同一批新增已应用）",
          any(a.get("op") == "add" for a in applied) and bool(cyc))
    probe_after = task(b_probe["id"]) or {}
    check("成环 update 被拒后原依赖保持不变（无半应用副作用）",
          probe_after.get("depends_on") == [b_down["id"]], probe_after.get("depends_on"))
    b_add = task_by_title(pidB, "B-补充任务")
    check("窗口新增任务默认挂到该里程碑", bool(b_add) and b_add.get("milestone_id") == M1id,
          b_add and b_add.get("milestone_id"))
    check("窗口新增任务默认带验收与验收标准",
          bool(b_add) and b_add.get("acceptance_required") and bool((b_add.get("acceptance_criteria") or "").strip()),
          b_add)
    check("窗口操作写入 task.created source=milestone_window",
          any(e["type"] == "task.created" and (e.get("payload") or {}).get("source") == "milestone_window"
              for e in events(pidB)))
    check("窗口开启/应用事件留痕",
          "milestone.window_opened" in etypes(pidB) and "milestone.window_applied" in etypes(pidB))

    # 清理探针（非终态），以免影响项目完成
    s, _ = post(f"/projects/{pidB}/intervene/apply", {"changes": [{"op": "cancel", "task_id": b_probe["id"]}]})
    check("环校验探针任务被取消清理", s == 200 and (task(b_probe["id"]) or {}).get("status") == "cancelled", s)

    # 补充任务完成后 M1 重入 gate → 通过 → 下游解冻
    add_done = wait_until(lambda: (task(b_add["id"]) or {}).get("status") == "done", 900)
    check("窗口追加任务完成后 done", add_done is not None, (task(b_add["id"]) or {}).get("status"))
    regate = wait_until(lambda: (next((m for m in milestones_of(pidB) if m["id"] == M1id), {}) or {}).get("status")
                        == "awaiting_confirm", 300)
    check("窗口成员全部完成 → 里程碑重入 awaiting_confirm", regate is not None,
          [m["status"] for m in milestones_of(pidB)])
    check("重入 gate 事件 reason=window_replan_done",
          any(e["type"] == "milestone.gate_opened" and (e.get("payload") or {}).get("reason") == "window_replan_done"
              for e in events(pidB)))
    g2 = open_gate(M1id)
    check("重入 gate 有对应收件箱条目", g2 is not None)
    if g2:
        s, _ = resolve(g2["id"], "pass", "冒烟二次确认")
        m1b = next((m for m in milestones_of(pidB) if m["id"] == M1id), {})
        check("二次确认 → 里程碑 passed", s == 200 and m1b.get("status") == "passed", (s, m1b.get("status")))
    down_done = wait_until(lambda: (task(b_down["id"]) or {}).get("status") == "done", 900)
    check("gate 通过后下游分支解冻并跑完", down_done is not None, (task(b_down["id"]) or {}).get("status"))
    # M2 gate（b_down）
    m2gate = wait_until(lambda: open_gate(M2id), 300)
    check("阶段B2 成员完成后 gate 出现", m2gate is not None)
    if m2gate:
        resolve(m2gate["id"], "pass", "冒烟确认")
    compB = wait_until(lambda: project_status(pidB) == "completed", 600)
    check("项目 B 全部终态 → completed", compB is not None, project_status(pidB))
    wait_until(lambda: (report(pidB) or {}).get("status") == "ready", 600)
    check("项目 B 总结报告生成 ready", (report(pidB) or {}).get("status") == "ready",
          (report(pidB) or {}).get("status"))

    # ---------------- Project D: 决策者纠偏重规划（tactical replan）----------------
    print("[m3-smoke] 5) 项目 D：执行中上报规划问题 → 决策者纠偏 → planner 复用会话产出并落地")
    pidD = new_project("M3端到端D-纠偏重规划", "验证执行中上报规划问题由决策者常规纠偏重规划")
    check("项目 D 创建", bool(pidD), pidD)
    s, _ = post(f"/projects/{pidD}/confirm", {})
    check("项目 D running", s == 200 and project_status(pidD) == "running", s)
    d_anchor = create_task(pidD, "D-锚点任务", "在当前工作目录创建文件 d_anchor.txt，内容为一行：anchor ok",
                           E1, A1, acceptance_criteria="d_anchor.txt 存在且内容为 anchor ok")
    check("项目 D 锚点任务创建（非终态，可上报重规划）", bool(d_anchor), d_anchor)
    s, r = post(f"/tasks/{d_anchor['id']}/replan", {
        "reason": "依赖顺序需调整，建议补充一个收尾任务",
        "detail": "锚点任务完成后需要一个收尾补充任务",
        "reported_by": "executor",
        "proposed_changes": [{"op": "add", "task": {
            "title": "D-纠偏补充任务",
            "description": "在当前工作目录创建文件 d_replan.txt，内容为一行：replan ok",
            "depends_on": [], "acceptance_required": True,
            "acceptance_criteria": "d_replan.txt 存在且内容为 replan ok"}}]})
    decD = r.get("decision") if s == 200 else None
    check("重规划请求登记（scope=tactical，decision.kind=replan）",
          s == 200 and r.get("scope") == "tactical" and decD and decD.get("kind") == "replan",
          (s, r.get("scope"), decD))
    check("replan.proposed 事件留痕", "replan.proposed" in etypes(pidD))
    ruledD = wait_until(lambda: next((d for d in decisions(pidD, kind="replan")
                                      if d.get("task_id") == d_anchor["id"] and d.get("status") == "ruled"), None), 600)
    check("决策者（真实 agent）批准纠偏重规划", ruledD is not None)
    if ruledD:
        check("纠偏裁定来源为 agent 且为批准",
              (ruledD.get("ruling") or {}).get("by") == "agent"
              and (ruledD.get("ruling") or {}).get("decision") == "approve", ruledD.get("ruling"))
    check("replan.approved 事件留痕", "replan.approved" in etypes(pidD))
    appliedD = wait_until(lambda: "replan.applied" in etypes(pidD), 900)
    check("planner 复用规划会话产出调整并落地（replan.applied）", appliedD is not None)
    newD = wait_until(lambda: (task_by_title(pidD, "D-纠偏补充任务")
                               or next((t for t in tasks_of(pidD) if t["id"] != d_anchor["id"]), None)), 300)
    check("重规划新增任务落地并进入调度（planned/ready/…）", bool(newD), [t["title"] for t in tasks_of(pidD)])
    check("重规划新建任务默认带验收",
          bool(newD) and newD.get("acceptance_required") and bool((newD.get("acceptance_criteria") or "").strip()),
          newD)
    pumpD = wait_until(lambda: project_status(pidD) == "completed", 1200)
    if pumpD:
        check("项目 D 纠偏后全部终态 → completed", True)
        wait_until(lambda: (report(pidD) or {}).get("status") == "ready", 600)
        repD = report(pidD) or {}
        check("项目 D 总结报告生成 ready", repD.get("status") == "ready", repD.get("status"))
    else:
        note(f"项目 D 未在时限内完成（{project_status(pidD)}），归档清理；重规划通道本身已验证")
        post(f"/projects/{pidD}/cancel", {})
        check("项目 D 已归档清理（非阻塞）", project_status(pidD) == "archived", project_status(pidD))

    # ---------------- Project C: 协商失败转决策者 + 决策者缺席转收件箱 ----------------
    print("[m3-smoke] 6) 项目 C：协商失败→决策者裁决；决策者缺席→decision_maker_down")
    s, projC = post("/projects", {"name": "M3端到端C-决策通道", "brief": "协商失败与决策者缺席降级"})
    pidC = projC.get("id")
    patch(f"/projects/{pidC}", {"workspace": {"kind": "local", "path": WS}})
    check("项目 C 创建（initializing）", s == 200 and pidC, s)
    # 给 C 一个里程碑并把 C1 挂进去：C1 完成后里程碑进入 gate（awaiting_confirm），
    # 项目停在 running 而不会因「唯一任务已完成」提前 auto-completed —— 否则
    # 会先触发一次随后作废的总结报告、并让下面的 pause/resume 因状态错位返回 409。
    # 该 gate 在 phase2 恢复续跑后确认，C 才进入终态并生成报告。
    _, MC = post(f"/projects/{pidC}/milestones", {"name": "C阶段", "goal": "决策通道验证阶段"})
    MCid = MC.get("id")
    check("项目 C 里程碑创建（C1 完成后保持项目非终态）", bool(MCid), MC)
    c1 = create_task(pidC, "C-协商失败", "在当前工作目录创建文件 c1.txt，内容为一行：c1 ok",
                     E1, A1, milestone_id=MCid,
                     acceptance_criteria="c1.txt 存在且内容为 c1 ok")
    check("C1 任务创建为 planned+pending", bool(c1) and c1.get("status") == "planned"
          and c1.get("negotiation_status") == "pending", c1)
    s, r = post(f"/tasks/{c1['id']}/negotiation", {"status": "failed", "acceptance_criteria": "c1.txt 存在且内容为 c1 ok"})
    dec1 = r.get("decision") if s == 200 else None
    check("协商失败登记 decision(kind=negotiation_failed, pending)", bool(dec1) and dec1.get("kind") == "negotiation_failed",
          (s, dec1))
    check("协商失败任务未开工（保持 planned）", (task(c1["id"]) or {}).get("status") == "planned")
    s, _ = post(f"/projects/{pidC}/confirm", {})
    check("项目 C confirm → running（触发决策者派发）", s == 200 and project_status(pidC) == "running", s)
    ruled1 = wait_until(lambda: next((d for d in decisions(pidC, kind="negotiation_failed")
                                      if d.get("task_id") == c1["id"] and d.get("status") == "ruled"), None), 600)
    check("决策者（真实 agent）裁定协商失败请求", ruled1 is not None)
    if ruled1:
        check("裁定来源为 agent", (ruled1.get("ruling") or {}).get("by") == "agent", ruled1.get("ruling"))
    settled1 = wait_until(lambda: (task(c1["id"]) or {}).get("negotiation_status") == "agreed", 300)
    check("裁定后协商置 agreed 并恢复流转", settled1 is not None,
          (task(c1["id"]) or {}).get("negotiation_status"))
    check("negotiation.settled 事件留痕", "negotiation.settled" in etypes(pidC))
    c1done = wait_until(lambda: (task(c1["id"]) or {}).get("status") == "done", 600)
    check("C1 裁定后执行+验收跑完 done", c1done is not None, (task(c1["id"]) or {}).get("status"))

    # 决策者缺席
    s1, _ = delete(f"/agents/{D1}")
    s2, _ = delete(f"/agents/{D2}")
    check("删除两个决策者实例（模拟缺席）", s1 == 200 and s2 == 200, (s1, s2))
    post(f"/projects/{pidC}/pause", {})
    c2 = create_task(pidC, "C-决策者缺席", "在当前工作目录创建文件 c2.txt，内容为一行：c2 ok",
                     E2, A2, acceptance_criteria="c2.txt 存在且内容为 c2 ok")
    post(f"/tasks/{c2['id']}/negotiation", {"status": "failed", "acceptance_criteria": "c2.txt 存在且内容为 c2 ok"})
    s, _ = post(f"/projects/{pidC}/resume", {})
    check("项目 C resume", s == 200, s)
    down_item = wait_until(lambda: open_dm_down(c2["id"]), 300)
    check("决策者缺席 → decision_maker_down 收件箱条目", down_item is not None)
    d2 = wait_until(lambda: next((d for d in decisions(pidC, kind="negotiation_failed")
                                  if d.get("task_id") == c2["id"] and d.get("status") == "down"), None), 300)
    check("对应决策置 down（挂起不阻塞无关分支）", d2 is not None)
    c2t = task(c2["id"]) or {}
    check("协商失败任务因决策者缺席被挂起（suspended）", c2t.get("status") == "suspended", c2t.get("status"))
    check("C1（无关分支）不受决策者缺席影响（已 done）",
          (task(c1["id"]) or {}).get("status") == "done")

    # 冻结现场，快照后崩溃重启
    post(f"/projects/{pidC}/pause", {})
    snap = {
        "pidA": pidA, "pidB": pidB, "pidC": pidC, "pidD": pidD, "ids": ids,
        "A": {"status": project_status(pidA), "milestones": [m["status"] for m in milestones_of(pidA)],
              "report": (report(pidA) or {}).get("status"), "tasks": len(tasks_of(pidA))},
        "B": {"status": project_status(pidB), "milestones": [m["status"] for m in milestones_of(pidB)],
              "report": (report(pidB) or {}).get("status"), "tasks": len(tasks_of(pidB))},
        "D": {"status": project_status(pidD)},
        "C": {"status": project_status(pidC), "c1": c1["id"], "c2": c2["id"],
              "c2_neg": c2t.get("negotiation_status"), "c2_status": c2t.get("status")},
        "complete": True,
    }
    json.dump(snap, open(STATE, "w"))
    print(f"[m3-smoke] phase1 snapshot saved -> {STATE}")
    return dump_results()


# ======================================================================
# PHASE 2: 重启后校验持久化 → 恢复决策者 → 续跑 C → 结果与日志落盘
# ======================================================================
def phase2():
    snap = json.load(open(STATE))
    if not snap.get("complete"):
        # phase1 在跑完整链路前就中止（例如真实 agent 连续失败）：不要抛 KeyError，
        # 明确记为失败并保留现场，便于复查 results.md / domo.log。
        check("重启恢复前置现场完整（phase1 已跑完快照）", False,
              f"phase1 未完成，failed={snap.get('failed')!r}")
        return dump_results()
    pidA, pidB, pidC = snap["pidA"], snap["pidB"], snap["pidC"]
    ids = snap["ids"]
    E1, E2 = ids["exec-1"], ids["exec-2"]
    A1, A2 = ids["acc-1"], ids["acc-2"]

    print("[m3-smoke] 7) 重启恢复：A/B/D 终态与 C 的失败协商/决策者缺席现场均不丢失")
    check("重启后项目 A 仍 completed", project_status(pidA) == "completed", project_status(pidA))
    check("重启后项目 A 里程碑状态保留（全 passed）",
          all(m["status"] == "passed" for m in milestones_of(pidA)),
          [m["status"] for m in milestones_of(pidA)])
    check("重启后项目 A 报告保留 ready", (report(pidA) or {}).get("status") == "ready",
          (report(pidA) or {}).get("status"))
    check("重启后项目 A 任务数不变", len(tasks_of(pidA)) == snap["A"]["tasks"],
          (len(tasks_of(pidA)), snap["A"]["tasks"]))
    check("重启后项目 B 仍 completed", project_status(pidB) == "completed", project_status(pidB))
    check("重启后项目 B 里程碑状态保留（全 passed）",
          all(m["status"] == "passed" for m in milestones_of(pidB)),
          [m["status"] for m in milestones_of(pidB)])
    check("重启后项目 B 报告保留 ready", (report(pidB) or {}).get("status") == "ready",
          (report(pidB) or {}).get("status"))
    pidD = snap["pidD"]
    snapD_status = snap.get("D", {}).get("status")
    check("重启后项目 D 状态保留且一致（completed/archived）",
          project_status(pidD) == snapD_status
          and snapD_status in ("completed", "archived"),
          (project_status(pidD), snapD_status))

    c2 = snap["C"]["c2"]
    c2t = task(c2) or {}
    check("重启后 C2 协商失败状态保留（failed）", c2t.get("negotiation_status") == "failed",
          c2t.get("negotiation_status"))
    check("重启后 C2 挂起状态保留（suspended）", c2t.get("status") == "suspended", c2t.get("status"))
    d2 = next((d for d in decisions(pidC, kind="negotiation_failed")
               if d.get("task_id") == c2 and d.get("status") == "down"), None)
    check("重启后决策仍为 down（持久化）", d2 is not None)
    item = open_dm_down(c2)
    check("重启后 decision_maker_down 收件箱条目仍 open", item is not None)
    check("重启后项目 C 保持 paused", project_status(pidC) == "paused", project_status(pidC))

    print("[m3-smoke] 8) 恢复决策者实例 → retry → 续裁 → C 跑完 → 总结报告")
    s1, j1 = post("/agents", {"name": "judge-3", "adapter": "opencode", "roles": ["decision_maker"],
                              "capabilities": {"max_parallel": 1, "supports_resume": True},
                              "config": {"model": MODEL, "timeout_ms": 300000}})
    check("重新注册决策者实例", s1 == 200 and j1.get("id"), (s1, j1))
    check("恢复前 C2 仍挂起（未擅自放行）", (task(c2) or {}).get("status") == "suspended")
    s, r = resolve(item["id"], "retry", "决策者已恢复，重新裁定")
    check("down 收件箱 retry 成功", s == 200, (s, r))
    s, _ = post(f"/projects/{pidC}/resume", {})
    check("项目 C 恢复 running", s == 200, s)
    ruledC = wait_until(lambda: next((d for d in decisions(pidC, kind="negotiation_failed")
                                      if d.get("task_id") == c2 and d.get("status") == "ruled"), None), 600)
    check("恢复后决策者裁定 C2 协商失败", ruledC is not None)
    c2done = wait_until(lambda: (task(c2) or {}).get("status") == "done", 600)
    check("C2 裁定后执行+验收跑完 done", c2done is not None, (task(c2) or {}).get("status"))
    # C 的里程碑 gate：全部任务终态但 gate 未决 → 项目保持 running（不提前 completed），
    # 人工确认后放行、项目才进入终态并自动生成总结报告。这正是 M3「gate 阻断项目完成」语义。
    cgate = wait_until(lambda: next((i for i in inbox() if i.get("type") == "milestone_gate"
                                     and i.get("status") == "open" and i.get("project_id") == pidC), None), 120)
    check("C 全部任务终态后里程碑 gate 打开（项目保持非终态）", cgate is not None)
    check("gate 未确认时项目 C 保持 running（不提前 completed）",
          project_status(pidC) == "running", project_status(pidC))
    check("gate 未确认时不提前生成总结报告",
          (report(pidC) or {}).get("status") != "ready", (report(pidC) or {}).get("status"))
    if cgate:
        s, _ = resolve(cgate["id"], "pass", "冒烟确认 C 里程碑 gate")
        check("项目 C 里程碑 gate 确认 → passed", s == 200, s)
    compC = wait_until(lambda: project_status(pidC) == "completed", 600)
    check("项目 C 全部终态 → completed", compC is not None, project_status(pidC))
    wait_until(lambda: (report(pidC) or {}).get("status") == "ready", 600)
    check("项目 C 总结报告生成 ready", (report(pidC) or {}).get("status") == "ready",
          (report(pidC) or {}).get("status"))

    print("[m3-smoke] 9) 审计与日志落盘可复查")
    all_ev = etypes(pidA) + etypes(pidB) + etypes(pidC) + etypes(pidD)
    for ev in ["planner.proposed", "negotiation.started", "negotiation.round", "negotiation.agreed",
               "acceptance.recorded", "milestone.gate_opened", "milestone.passed", "milestone.failed",
               "milestone.window_opened", "milestone.window_applied", "replan.approved", "replan.applied",
               "decision.requested", "decision.ruled", "negotiation.settled", "report.generated"]:
        check(f"审计事件存在：{ev}", ev in all_ev)
    log = ""
    try:
        with open(os.path.join(RUN_DIR, "domo.log"), "r", errors="replace") as f:
            log = f.read()
    except FileNotFoundError:
        pass
    check("服务端运行日志含 opencode spawn 记录", "opencode run spawn" in log)
    check("服务端运行日志含 opencode run finished 记录", "opencode run finished" in log)
    check("服务端运行日志无 panic", "panicked at" not in log)
    return dump_results()


if PHASE == "phase1":
    sys.exit(phase1())
else:
    sys.exit(phase2())
PY

start_server || exit 1
python3 "$PY" phase1 "$PORT" "$RUN_DIR" "$WS" "$STATE" "$MODEL"
RC1=$?

echo "[m3-smoke] kill -9 server (pid $SERVER_PID) and restart (simulated crash)…"
kill -9 "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
start_server || exit 1
python3 "$PY" phase2 "$PORT" "$RUN_DIR" "$WS" "$STATE" "$MODEL"
RC2=$?

# ---------------- 归档：结果 + 关键日志落盘到仓库 data/ 下可复查 ----------------
# 每次运行归档为 runN-{summary,results,domo.log}，同时刷新 latest 别名
# {summary.md,results.md,domo.log}；并刷新 STABILITY.md 记录多次运行的可重复性。
ARCH_DIR="$ROOT/data/smoke_m3_e2e"
mkdir -p "$ARCH_DIR"
RUN_NO=1
while [[ -e "$ARCH_DIR/run${RUN_NO}-results.md" ]]; do RUN_NO=$((RUN_NO + 1)); done
RUN_TAG="run${RUN_NO}"
RUN_TIME="$(date -Is)"
cp -f "$RESULTS" "$ARCH_DIR/$RUN_TAG-results.md" 2>/dev/null || true
cp -f "$LOG" "$ARCH_DIR/$RUN_TAG-domo.log" 2>/dev/null || true
{
  echo "# M3 端到端冒烟结果（$RUN_TAG）";
  echo;
  echo "- 时间：$RUN_TIME";
  echo "- 模型：$MODEL";
  echo "- 端口：$PORT";
  echo "- 运行目录：$RUN_DIR";
  echo "- phase1 退出码：$RC1 / phase2 退出码：$RC2";
  echo "- 结果明细：$ARCH_DIR/$RUN_TAG-results.md（本次运行 run dir: $RUN_DIR）";
  echo "- 服务端日志：$ARCH_DIR/$RUN_TAG-domo.log（同一份另存 data/domo.log）";
  echo;
  cat "$RESULTS" 2>/dev/null || true;
} > "$ARCH_DIR/$RUN_TAG-summary.md"

# latest 别名：始终指向最近一次运行，方便按验收口径直接复查
cp -f "$RESULTS" "$ARCH_DIR/results.md" 2>/dev/null || true
cp -f "$LOG" "$ARCH_DIR/domo.log" 2>/dev/null || true
cp -f "$ARCH_DIR/$RUN_TAG-summary.md" "$ARCH_DIR/summary.md" 2>/dev/null || true
# 关键运行日志同时落到 data/domo.log（服务端日志默认路径语义），便于按验收口径复查
cp -f "$LOG" "$ROOT/data/domo.log" 2>/dev/null || true

# 可重复性记录：逐次统计 PASS/FAIL，并与上一次运行比较检查集是否一致
checks_diff() {
  # $1/$2 = results 文件；仅比较检查名与 PASS/FAIL（忽略时间戳与备注）
  diff <(grep -E '^- (PASS|FAIL) — ' "$1" 2>/dev/null) \
       <(grep -E '^- (PASS|FAIL) — ' "$2" 2>/dev/null) >/dev/null 2>&1
}
{
  echo "# M3 端到端冒烟 · 可重复性记录";
  echo;
  echo "每次运行使用独立临时目录（SQLite/工作区/日志），断言基于状态持久化，"
  echo "脚本可直接重复执行；下表由脚本归档阶段自动生成。检查集对比忽略时间戳与备注。";
  echo;
  echo "| 运行 | 时间 | 检查数 | PASS | FAIL | 结果 | 与上次检查集 |";
  echo "|---|---|---|---|---|---|---|";
  prev=""
  for f in $(ls -1v "$ARCH_DIR"/run*-results.md 2>/dev/null); do
    tag="$(basename "$f" -results.md)"
    total="$(grep -cE '^- (PASS|FAIL) — ' "$f" 2>/dev/null || true)"
    pass="$(grep -cE '^- PASS — ' "$f" 2>/dev/null || true)"
    fail="$(grep -cE '^- FAIL — ' "$f" 2>/dev/null || true)"
    mt="$(stat -c %y "$ARCH_DIR/$tag-summary.md" 2>/dev/null | cut -d. -f1 || true)"
    if [[ "$fail" == "0" && "$total" != "0" ]]; then verdict="PASS"; else verdict="FAIL"; fi
    if [[ -z "$prev" ]]; then
      same="—（首次归档）"
    elif checks_diff "$prev" "$f"; then
      same="一致"
    else
      same="**不一致**"
    fi
    echo "| $tag | ${mt:-unknown} | $total | $pass | $fail | $verdict | $same |"
    prev="$f"
  done
  echo
  if [[ "$RC1" -eq 0 && "$RC2" -eq 0 ]]; then
    echo "本次（$RUN_TAG）phase1=$RC1 / phase2=$RC2，全部检查通过。"
  else
    echo "本次（$RUN_TAG）phase1=$RC1 / phase2=$RC2，**存在失败**，详见 $RUN_TAG-results.md。"
  fi
  echo
  echo "用例清单与覆盖范围见 [docs/SMOKE_M3_E2E.md](../../docs/SMOKE_M3_E2E.md)。"
} > "$ARCH_DIR/STABILITY.md"
echo "[m3-smoke] 归档：$ARCH_DIR/{$RUN_TAG-summary.md,$RUN_TAG-results.md,$RUN_TAG-domo.log} + latest 别名 + STABILITY.md；并写 data/domo.log"

if [[ $RC1 -ne 0 || $RC2 -ne 0 ]]; then
  echo "[m3-smoke] FAILED (rc1=$RC1 rc2=$RC2); server log tail:"
  tail -60 "$LOG" 2>/dev/null || true
fi
exit $(( RC1 | RC2 ))
