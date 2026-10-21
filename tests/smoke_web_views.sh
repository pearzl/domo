#!/usr/bin/env bash
# M3 前端三视图（DAG 图 / 项目总览里程碑 / 组织视图负载）—— 端到端数据契约冒烟。
#
# 三处前端增强都直接消费「REST 快照 + SSE 推送」，本脚本对着真实后端（mock agent）
# 校验它们所依赖的数据，等价于在浏览器里逐条核对验收标准：
#   1. 任务视图 DAG 图：GET /projects/:id 的 tasks[].depends_on 必须与图上节点/边一一对应
#      —— 节点 = 项目任务全集、边 = depends_on 全配对、无悬空依赖、里程碑分组可解析；
#      状态变化经 SSE `task` 事件推送（前端据此实时刷新节点颜色）。
#   2. 项目总览：里程碑 awaiting_confirm 时必须存在 payload.milestone_id 匹配的 open
#      milestone_gate 收件箱条目 → 前端「待确认 · 去收件箱 →」的跳转目标真实存在。
#   3. 组织视图负载：并行派发 3 个任务时 agents[].current_task_ids / status 反映占用，
#      队列长度（指派给该 agent 且 planned/ready 的任务数）与 max_parallel 占用比可计算；
#      任务完成后占用释放回空闲。同时校验 gate 冻结下游时的「排队」形态。
#   4. 静态托管：后端托管 web/dist，bundle 内含三处增强的标记字符串（构建产物确为最新）。
#
# 说明：「切页再切回视图状态不丢」是纯客户端状态（tasksViewMode / selectedTaskId /
# focusedInboxId 提升到全局 store），无法在无浏览器环境断言，故不在本脚本覆盖范围。
#
# 用法：bash scripts/smoke_web_views.sh   （退出码 0 = 全部通过）
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_WEB_SMOKE_PORT:-8797}"
TMP_ROOT="${TMPDIR:-/tmp}"
RUN_DIR="$(mktemp -d "$TMP_ROOT/domo_web_smoke.XXXXXX")"
DB="$RUN_DIR/domo.db"
WS="$RUN_DIR/workspace"
LOG="$RUN_DIR/domo.log"
SSE="$RUN_DIR/sse.txt"
mkdir -p "$WS"

cleanup() {
  [[ -n "${SSE_PID:-}" ]] && kill "$SSE_PID" 2>/dev/null || true
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "[web-smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[web-smoke] server build failed"; exit 1; }

if [[ ! -f "$ROOT/web/dist/index.html" ]]; then
  echo "[web-smoke] building web (no dist)…"
  (cd "$ROOT/web" && npm install --silent && npm run build --silent) || { echo "[web-smoke] web build failed"; exit 1; }
fi

echo "[web-smoke] starting server on :$PORT (db $DB)"
(
  cd "$ROOT"
  DOMO_DB="$DB" DOMO_PORT="$PORT" DOMO_LOG="$LOG" DOMO_LOG_LEVEL=warn \
    exec "$ROOT/server/target/debug/domo-server"
) >"$RUN_DIR/stdout.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 80); do
  curl -sf "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1 && break
  sleep 0.25
done
if ! curl -sf "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1; then
  echo "[web-smoke] server did not come up; log:"; tail -30 "$RUN_DIR/stdout.log"
  exit 1
fi

# 全程抓取 SSE（前端实时刷新的数据源）
curl -sN "http://127.0.0.1:$PORT/api/stream" >"$SSE" 2>/dev/null &
SSE_PID=$!
sleep 0.3

python3 - "$PORT" "$WS" "$SSE" <<'PY'
import json, os, re, subprocess, sys, time, urllib.request, urllib.error

PORT, WS, SSE_FILE = sys.argv[1], sys.argv[2], sys.argv[3]
BASE = f"http://127.0.0.1:{PORT}"
CHECKS = []

def check(name, cond, detail=""):
    cond = bool(cond)
    CHECKS.append((name, cond))
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  -- " + str(detail)) if (detail and not cond) else ""), flush=True)

def req(method, path, body=None, raw=False):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(BASE + path, data=data, method=method,
                               headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=15) as resp:
            payload = resp.read()
            if raw:
                return resp.status, payload.decode("utf-8", "replace")
            return resp.status, json.loads(payload.decode())
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.load(e)
        except Exception:
            return e.code, {}
    except Exception as e:
        return 0, {"error": str(e)}

def get(path, raw=False): return req("GET", path, raw=raw)
def post(path, body): return req("POST", path, body)
def patch(path, body): return req("PATCH", path, body)

def wait_until(fn, timeout=30, interval=0.05):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(interval)
    return None

def project_detail(pid):
    s, b = get(f"/api/projects/{pid}")
    return b if s == 200 else {}

def agents():
    s, b = get("/api/agents")
    return b if s == 200 else []

def agent_by(prefix):
    return next((a for a in agents() if a["id"] == prefix), None)

def wait_agent(prefix, pred, timeout=90, interval=0.05):
    end = time.time() + timeout
    while time.time() < end:
        a = agent_by(prefix)
        if a and pred(a):
            return a
        time.sleep(interval)
    return None

def sse_frames():
    try:
        with open(SSE_FILE, "r", errors="replace") as f:
            text = f.read()
    except FileNotFoundError:
        return []
    out = []
    for block in text.replace("\r\n", "\n").split("\n\n"):
        ev, datas = None, []
        for line in block.split("\n"):
            if line.startswith("event:"):
                ev = line[6:].strip()
            elif line.startswith("data:"):
                datas.append(line[5:].strip())
        if ev and datas:
            out.append((ev, "\n".join(datas)))
    return out

# ---------------------------------------------------------------- setup
print("[web-smoke] 1) agents（执行者 A max_parallel=3 / 执行者 B max_parallel=2 / 验收者 C）")
s, A = post("/api/agents", {"name": "exec-A", "adapter": "mock", "roles": ["executor"],
                            "capabilities": {"max_parallel": 3, "supports_resume": False},
                            "config": {"duration_ms": 2500}})
s2, B = post("/api/agents", {"name": "exec-B", "adapter": "mock", "roles": ["executor"],
                             "capabilities": {"max_parallel": 2, "supports_resume": False},
                             "config": {"duration_ms": 1500}})
s3, C = post("/api/agents", {"name": "acc-C", "adapter": "mock", "roles": ["acceptor"],
                             "config": {"duration_ms": 200}})
check("3 个 agent 注册成功（A/B 含并行上限）", s == 200 and s2 == 200 and s3 == 200, f"{s}/{s2}/{s3}")
A_id, B_id, C_id = A.get("id"), B.get("id"), C.get("id")

print("[web-smoke] 2) 项目 + 两个里程碑 + 5 个带依赖任务")
# 方案 B：共享目录串行；并行负载视图必须用 Git 仓库工程（worktree）验证。
os.makedirs(WS, exist_ok=True)
subprocess.run(["git", "-C", WS, "init", "-q"], check=True)
with open(os.path.join(WS, "README.md"), "w") as f:
    f.write("web views smoke\n")
subprocess.run(["git", "-C", WS, "add", "-A"], check=True)
subprocess.run(["git", "-C", WS, "-c", "user.name=domo", "-c", "user.email=domo@example.invalid",
                "commit", "-q", "-m", "init"], check=True)
s, eng = post("/api/engineerings", {"name": "前端三视图工程", "workspace_path": WS})
check("Git 仓库工程自动识别", s == 200 and eng.get("workspace_kind") == "single", eng)
s, proj = post("/api/projects", {"name": "前端三视图冒烟", "brief": "web views smoke",
                                 "engineering_id": eng.get("id")})
pid = proj.get("id")
check("项目创建", s == 200 and pid, f"{s} {proj}")
post(f"/api/projects/{pid}/confirm", {})
check("项目进入 running", get(f"/api/projects/{pid}")[1].get("project", {}).get("status") == "running")

s, M1 = post(f"/api/projects/{pid}/milestones", {"name": "阶段一", "goal": "打好地基", "effect": "可继续"})
s2, M2 = post(f"/api/projects/{pid}/milestones", {"name": "阶段二", "goal": "上层功能"})
check("里程碑创建", s == 200 and s2 == 200, f"{s} {M1} / {s2} {M2}")
M1_id, M2_id = M1.get("id"), M2.get("id")

def mk_task(body):
    s, t = post(f"/api/projects/{pid}/tasks", body)
    return t if s == 200 else None

T1 = mk_task({"title": "T1 地基", "description": "d", "milestone_id": M1_id,
              "executor_agent_id": A_id, "acceptor_agent_id": C_id,
              "acceptance_criteria": "OK"})
T2 = mk_task({"title": "T2 并行-2", "description": "d", "milestone_id": M1_id,
              "executor_agent_id": A_id, "acceptor_agent_id": C_id,
              "acceptance_criteria": "OK"})
T3 = mk_task({"title": "T3 并行-3", "description": "d", "milestone_id": M1_id,
              "executor_agent_id": A_id, "acceptor_agent_id": C_id,
              "acceptance_criteria": "OK"})
T4 = mk_task({"title": "T4 依赖 T1", "description": "d", "milestone_id": M2_id,
              "depends_on": [T1["id"]], "executor_agent_id": B_id, "acceptor_agent_id": C_id,
              "acceptance_criteria": "OK"})
T5 = mk_task({"title": "T5 依赖 T1+T2", "description": "d", "milestone_id": M2_id,
              "depends_on": [T1["id"], T2["id"]], "executor_agent_id": B_id, "acceptor_agent_id": C_id,
              "acceptance_criteria": "OK"})
check("5 个任务创建（含跨里程碑依赖）", all([T1, T2, T3, T4, T5]))
D = project_detail(pid)
tasks = D.get("tasks", [])

# ---------------------------------------------------------------- DAG graph contract
print("[web-smoke] 3) DAG 图契约：节点/边与数据库依赖完全一致")
node_ids = {t["id"] for t in tasks}
expect_edges = {(d, t["id"]) for t in tasks for d in t.get("depends_on", [])}
check("节点 = 项目任务全集（前端按 tasks 渲染节点）", len(node_ids) == 5, sorted(t["title"] for t in tasks))
dangling = [d for (_f, _t) in expect_edges for d in [_f] if d not in node_ids]
check("无悬空依赖 → 图上不会出现指向不存在节点的边", not dangling, dangling)
check("边 = depends_on 全配对（5 节点 3 条依赖边）", len(expect_edges) == 3, sorted(expect_edges))
check("T5 的两条依赖都被构图取到",
      {(T1["id"], T5["id"]), (T2["id"], T5["id"])} <= expect_edges)

milestone_ids = {m["id"] for m in D.get("milestones", [])}
bad_group = [t["id"] for t in tasks if t.get("milestone_id") and t["milestone_id"] not in milestone_ids]
check("里程碑分组可解析：每个任务的 milestone_id 都命中已知里程碑", not bad_group, bad_group)
m1_members = sorted(t["id"] for t in tasks if t.get("milestone_id") == M1_id)
check("泳道成员与 milestone.task_ids 一致",
      m1_members == sorted(next(m for m in D["milestones"] if m["id"] == M1_id)["task_ids"]), m1_members)

# ---------------------------------------------------------------- org load: 3-way parallel
print("[web-smoke] 4) 组织视图负载：并行派发 3 个任务时的占用/空闲")
def load(a):
    """与 web/src/views/Org.tsx loadOf() 同一口径：占用=current_task_ids，队列=指派的 planned/ready"""
    cap = max(1, a.get("capabilities", {}).get("max_parallel", 1))
    occupied = len(a.get("current_task_ids") or [])
    queue = sum(1 for t in tasks_now()
                if t.get("executor_agent_id") == a["id"] and t.get("status") in ("planned", "ready"))
    return {"cap": cap, "occupied": occupied, "free": max(0, cap - occupied), "queue": queue,
            "pct": min(100, round(occupied / cap * 100))}

def tasks_now():
    return project_detail(pid).get("tasks", [])

full = wait_agent(A_id, lambda a: len(a.get("current_task_ids") or []) == 3)
la = load(full) if full else load(agent_by(A_id) or {})
check("执行者 A 并行执行 3 个任务：占用 3/3（100%）", la["occupied"] == 3 and la["pct"] == 100, la)
check("执行者 A 无空闲余量：free = 0 且 status = busy",
      la["free"] == 0 and (agent_by(A_id) or {}).get("status") == "busy", la)
check("执行者 A 的占用任务就是 T1/T2/T3（图/负载/任务视图一致）",
      set((agent_by(A_id) or {}).get("current_task_ids") or []) == {T1["id"], T2["id"], T3["id"]},
      (agent_by(A_id) or {}).get("current_task_ids"))
check("执行者 B 未被派发：占用 0/2、空闲余量 2（status=idle）",
      (lambda a: a and len(a.get("current_task_ids") or []) == 0 and a.get("status") == "idle"
       and load(a)["free"] == 2)(agent_by(B_id)), load(agent_by(B_id) or {}))
check("执行者 B 队列 = 2（T4/T5 已指派、等依赖完成 → 负载视图显示「排队 2」）",
      load(agent_by(B_id) or {})["queue"] == 2, load(agent_by(B_id) or {}))

# ---------------------------------------------------------------- milestone gate + overview
print("[web-smoke] 5) 项目总览：里程碑 awaiting_confirm + gate 跳转目标")
g = wait_until(lambda: next((m for m in project_detail(pid).get("milestones", [])
                             if m["id"] == M1_id and m["status"] == "awaiting_confirm"), None), 30)
check("阶段一：全部任务 DONE → awaiting_confirm（总览显示待确认）", g is not None,
      [m["status"] for m in project_detail(pid).get("milestones", [])])
s, inbox = get("/api/inbox")
gate = next((i for i in inbox
             if i.get("type") == "milestone_gate" and i.get("status") == "open"
             and (i.get("payload") or {}).get("milestone_id") == M1_id), None)
check("存在 open 的 milestone_gate 条目且 payload.milestone_id 指向该里程碑（跳转目标）",
      gate is not None, [i.get("type") for i in inbox])
if gate:
    pl = gate.get("payload") or {}
    check("gate payload 带 name/goal（收件箱条目可读）", bool(pl.get("name")) and pl.get("name") == "阶段一", pl)
# 总览进度条口径：已完成任务数 / 里程碑任务数
done_m1 = sum(1 for t in tasks_now() if t.get("milestone_id") == M1_id and t.get("status") == "done")
check("阶段一进度条口径 = 3/3", done_m1 == 3, done_m1)

# ---------------------------------------------------------------- gate freezes downstream
print("[web-smoke] 6) gate 冻结下游：B 显示「占用 0/2 · 排队 2」")
frozen = wait_until(lambda: all(t.get("status") in ("planned", "ready") for t in
                                [next(x for x in tasks_now() if x["id"] == T4["id"]),
                                 next(x for x in tasks_now() if x["id"] == T5["id"])]), 10)
check("下游 T4/T5 冻结在 planned/ready（DAG 上呈灰/蓝色）", frozen is not None,
      [(x["title"], x["status"]) for x in tasks_now() if x["id"] in (T4["id"], T5["id"])])
lb = load(agent_by(B_id) or {})
check("执行者 B：占用 0/2 + 排队 2（负载视图区分「占用」与「队列」）",
      lb["occupied"] == 0 and lb["queue"] == 2 and lb["free"] == 2, lb)

# ---------------------------------------------------------------- resolve gate -> parallel 2-way
print("[web-smoke] 7) 处理 gate 后下游放行：B 并行 2 个任务占满")
s, _ = post(f"/api/inbox/{gate['id']}/resolve", {"resolution": "pass", "note": "冒烟通过"})
check("收件箱 gate 处理成功（前端「处理」按钮路径）", s == 200, s)
check("阶段一 → passed",
      next((m["status"] for m in project_detail(pid).get("milestones", []) if m["id"] == M1_id), None) == "passed")
bfull = wait_agent(B_id, lambda a: len(a.get("current_task_ids") or []) == 2, timeout=120)
lb2 = load(bfull) if bfull else load(agent_by(B_id) or {})
check("执行者 B 并行执行 2 个任务：占用 2/2（100%）、空闲 0",
      lb2["occupied"] == 2 and lb2["pct"] == 100 and lb2["free"] == 0, lb2)
check("执行者 B 队列排空：2 个任务都已派发（占用 2 + 队列 0 = 并行上限）",
      lb2["queue"] == 0 or lb2["occupied"] + lb2["queue"] == 2, lb2)

# ---------------------------------------------------------------- release back to idle
print("[web-smoke] 8) 任务完成后占用释放回空闲")
alldone = wait_until(lambda: all(t.get("status") == "done" for t in tasks_now()), 40)
check("全部任务 done", alldone is not None, [(t["title"], t["status"]) for t in tasks_now()])
idle_a, idle_b = wait_until(lambda: (agent_by(A_id) or {}).get("status") == "idle", 10), \
                 wait_until(lambda: (agent_by(B_id) or {}).get("status") == "idle", 10)
check("执行者 A 释放：0/3（0%）· idle", idle_a is not None and load(agent_by(A_id))["occupied"] == 0)
check("执行者 B 释放：0/2（0%）· idle", idle_b is not None and load(agent_by(B_id))["occupied"] == 0)
role_counts = {r: sum(1 for a in agents() if r in (a.get("roles") or []))
               for r in ("planner", "executor", "acceptor")}
check("角色分布口径（executor 2 / acceptor 1）",
      role_counts["executor"] == 2 and role_counts["acceptor"] == 1, role_counts)

# ---------------------------------------------------------------- SSE realtime
print("[web-smoke] 9) SSE 实时推送（前端图/负载实时刷新的数据源）")
frames = sse_frames()
kinds = {ev for ev, _ in frames}
for k in ("task", "agent", "milestone", "inbox"):
    check(f"SSE 推送 `{k}` 事件", k in kinds, sorted(kinds))
max_occ = 0
for ev, data in frames:
    if ev != "agent":
        continue
    try:
        a = json.loads(data)
    except Exception:
        continue
    if a.get("id") == A_id:
        max_occ = max(max_occ, len(a.get("current_task_ids") or []))
check("SSE agent 帧里出现过 A 的占用 3（负载视图实时刷新）", max_occ >= 3, max_occ)
status_frames = [json.loads(d).get("status") for ev, d in frames if ev == "task"]
check("SSE task 帧带状态（DAG 节点颜色实时刷新）", "executing" in status_frames, sorted(set(status_frames)))

# ---------------------------------------------------------------- static hosting
print("[web-smoke] 10) 静态托管：构建产物含三处增强的标记")
s, html = get("/", raw=True)
ok_html = s == 200 and "<div id=\"root\">" in html
check("GET / 返回构建后的 SPA(index.html)", ok_html, s)
js_paths = re.findall(r'src="([^"]+\.js)"', html)
bundle = ""
for p in js_paths:
    st, txt = get(p if p.startswith("/") else "/" + p, raw=True)
    if st == 200:
        bundle += txt
for marker, label in [("DAG 图", "任务视图 DAG 切换"), ("待确认 · 去收件箱", "总览 gate 跳转按钮"),
                      ("占用", "负载占用比"), ("队列空", "队列长度"), ("角色分布", "角色分布"),
                      ("规划台", "规划台"), ("方案摘要", "方案摘要"), ("验收一屏", "验收一屏"),
                      ("工程 / 项目", "三级导航")]:
    check(f"bundle 含「{label}」标记：{marker}", marker in bundle)

failed = [n for n, ok in CHECKS if not ok]
print()
print(f"[web-smoke] {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
if failed:
    print("[web-smoke] FAILED:")
    for n in failed:
        print("  - " + n)
    sys.exit(1)
print("[web-smoke] ALL PASS")
PY
RC=$?

if [[ $RC -ne 0 ]]; then
  echo "[web-smoke] server log tail:"
  tail -40 "$LOG" 2>/dev/null || true
fi
exit $RC
