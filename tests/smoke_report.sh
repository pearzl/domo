#!/usr/bin/env bash
# M3 项目总结报告 —— 端到端冒烟（mock agent）。
#
# 对着真实后端核对验收标准：
#   1. 完成一个含里程碑的项目后，调度器自动触发报告生成；
#      报告内容覆盖：每个里程碑 goal 与 gate 结果、任务状态统计、
#      退回 / 升级 / 挂起事件摘要、产物路径清单。
#   2. planner 故障时报告置 failed、错误写入 data/domo.log，且可手工重试成功
#      （POST /api/projects/:id/report）；成功后落盘 <data>/projects/<prj>/summary-report.md。
#   3. 无 planner 故障时，另一个项目完成后报告自动 ready（自动生成路径）。
#   4. 前端查看入口：构建产物含报告弹窗标记；SSE 推送 `report` 事件。
#
# 用法：bash scripts/smoke_report.sh   （退出码 0 = 全部通过）
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_REPORT_SMOKE_PORT:-8798}"
TMP_ROOT="${TMPDIR:-/tmp}"
RUN_DIR="$(mktemp -d "$TMP_ROOT/domo_report_smoke.XXXXXX")"
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

echo "[report-smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[report-smoke] server build failed"; exit 1; }

if [[ ! -f "$ROOT/web/dist/index.html" ]]; then
  echo "[report-smoke] building web (no dist)…"
  (cd "$ROOT/web" && npm install --silent && npm run build --silent) || { echo "[report-smoke] web build failed"; exit 1; }
fi

echo "[report-smoke] starting server on :$PORT (db $DB)"
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
  echo "[report-smoke] server did not come up; log:"; tail -30 "$RUN_DIR/stdout.log"
  exit 1
fi

# 全程抓取 SSE（前端实时刷新的数据源）
curl -sN "http://127.0.0.1:$PORT/api/stream" >"$SSE" 2>/dev/null &
SSE_PID=$!
sleep 0.3

python3 - "$PORT" "$RUN_DIR" "$SSE" "$WS" <<'PY'
import json, os, re, sys, time, urllib.request, urllib.error

PORT, RUN_DIR, SSE_FILE, WS = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
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
        with urllib.request.urlopen(r, timeout=20) as resp:
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

def wait_until(fn, timeout=45, interval=0.05):
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

def tasks_of(pid):
    return project_detail(pid).get("tasks", [])

def task_by_title(pid, title):
    return next((t for t in tasks_of(pid) if t["title"] == title), None)

def report_of(pid):
    s, b = get(f"/api/projects/{pid}/report")
    return b.get("report") if s == 200 else None

def inbox_items():
    s, b = get("/api/inbox")
    return b if s == 200 else []

def open_escalation_for(task_id):
    return next((i for i in inbox_items()
                 if i.get("type") == "escalation" and i.get("status") == "open"
                 and (i.get("payload") or {}).get("task_id") == task_id), None)

def open_gate_for(mid):
    return next((i for i in inbox_items()
                 if i.get("type") == "milestone_gate" and i.get("status") == "open"
                 and (i.get("payload") or {}).get("milestone_id") == mid), None)

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
print("[report-smoke] 1) agents（planner 初始故障 / executor / acceptor）")
s, P = post("/api/agents", {"name": "planner-P", "adapter": "mock", "roles": ["planner"],
                            "capabilities": {"max_parallel": 1, "supports_resume": False},
                            "config": {"duration_ms": 120, "report_fail": True}})
s2, E = post("/api/agents", {"name": "exec-E", "adapter": "mock", "roles": ["executor"],
                             "capabilities": {"max_parallel": 4, "supports_resume": False},
                             "config": {"duration_ms": 150}})
s3, C = post("/api/agents", {"name": "acc-C", "adapter": "mock", "roles": ["acceptor"],
                             "capabilities": {"max_parallel": 4, "supports_resume": False},
                             "config": {}})
check("3 个 agent 注册成功", s == 200 and s2 == 200 and s3 == 200, f"{s}/{s2}/{s3}")
P_id, E_id, C_id = P.get("id"), E.get("id"), C.get("id")

print("[report-smoke] 2) 项目 A：两个里程碑 + 4 个正常任务 + 1 挂起 + 1 升级")
s, proj = post("/api/projects", {"name": "报告冒烟A", "brief": "验证自动/重试报告生成"})
pid = proj.get("id")
check("项目创建", s == 200 and pid, f"{s} {proj}")
patch(f"/api/projects/{pid}", {"workspace": {"kind": "local", "path": WS}})
post(f"/api/projects/{pid}/confirm", {})
check("项目进入 running", project_detail(pid).get("project", {}).get("status") == "running")

s, M1 = post(f"/api/projects/{pid}/milestones", {"name": "阶段一", "goal": "打好地基", "effect": "可继续"})
s2, M2 = post(f"/api/projects/{pid}/milestones", {"name": "阶段二", "goal": "上层功能"})
check("两个里程碑创建", s == 200 and s2 == 200, f"{s} {M1} / {s2} {M2}")
M1_id, M2_id = M1.get("id"), M2.get("id")

def mk_task(title, **kw):
    body = {"title": title, "description": kw.pop("description", "d"),
            "executor_agent_id": E_id, "acceptor_agent_id": C_id,
            "acceptance_criteria": "OK", **kw}
    s, t = post(f"/api/projects/{pid}/tasks", body)
    return t if s == 200 else None

T1 = mk_task("T1 地基", milestone_id=M1_id)
T2 = mk_task("T2 并行", milestone_id=M1_id)
T3 = mk_task("T3 上层", milestone_id=M2_id)
T4 = mk_task("T4 上层并行", milestone_id=M2_id)
N1 = mk_task("N1 执行失败挂起", description="[fail] 模拟连续执行失败")
N2 = mk_task("N2 验收退回升级", description="[reject] 模拟连续验收退回")
check("6 个任务创建（含挂起/升级负面任务）", all([T1, T2, T3, T4, N1, N2]),
      [bool(x) for x in (T1, T2, T3, T4, N1, N2)])

print("[report-smoke] 3) 里程碑 gate 通过（阶段一 / 阶段二）")
for label, mid in (("阶段一", M1_id), ("阶段二", M2_id)):
    gate = wait_until(lambda mid=mid: open_gate_for(mid), 60)
    check(f"{label}：成员全部 DONE → 出现 open gate 条目", gate is not None)
    if gate:
        s, _ = post(f"/api/inbox/{gate['id']}/resolve", {"resolution": "pass", "note": "冒烟通过"})
        st = next((m["status"] for m in project_detail(pid).get("milestones", []) if m["id"] == mid), None)
        check(f"{label}：gate 确认 → passed", s == 200 and st == "passed", f"{s}/{st}")

print("[report-smoke] 4) 负面任务终止（挂起 abandon / 升级 abandon）")
# 失败重试含指数退避（5s→15s→45s），3 次失败需 >60s
n1 = wait_until(lambda: (task_by_title(pid, "N1 执行失败挂起") or {}).get("status") == "suspended", 200)
check("N1 连续失败 3 次 → suspended", n1 is not None, (task_by_title(pid, "N1 执行失败挂起") or {}).get("status"))
esc1 = wait_until(lambda: open_escalation_for(N1["id"]), 20)
check("N1 产生 open escalation 收件箱条目", esc1 is not None)
if esc1:
    s, _ = post(f"/api/inbox/{esc1['id']}/resolve", {"resolution": "abandon", "note": "冒烟放弃"})
    check("N1 abandon → cancelled", s == 200 and (task_by_title(pid, "N1 执行失败挂起") or {}).get("status") == "cancelled")

n2 = wait_until(lambda: (task_by_title(pid, "N2 验收退回升级") or {}).get("status") == "escalated", 60)
check("N2 验收退回 2 次 → escalated", n2 is not None, (task_by_title(pid, "N2 验收退回升级") or {}).get("status"))
esc2 = wait_until(lambda: open_escalation_for(N2["id"]), 20)
check("N2 产生 open escalation 收件箱条目", esc2 is not None)
if esc2:
    s, _ = post(f"/api/inbox/{esc2['id']}/resolve", {"resolution": "abandon", "note": "冒烟放弃"})
    check("N2 abandon → cancelled", s == 200 and (task_by_title(pid, "N2 验收退回升级") or {}).get("status") == "cancelled")

print("[report-smoke] 5) 项目完成 → 自动生成报告（planner 故障 → failed）")
done = wait_until(lambda: project_detail(pid).get("project", {}).get("status") == "completed", 200)
check("项目 A 自动进入 completed", done is not None, project_detail(pid).get("project", {}).get("status"))
failed = wait_until(lambda: (report_of(pid) or {}).get("status") == "failed", 90)
rep = report_of(pid)
check("planner 故障 → 报告自动生成失败（status=failed）", failed is not None, rep and rep.get("status"))
check("失败原因来自 planner（mock report_fail）", rep and "report_fail" in (rep.get("error") or ""), rep and rep.get("error"))
# 报告生成失败必须写 data/domo.log
try:
    with open(os.path.join(RUN_DIR, "domo.log"), "r", errors="replace") as f:
        logtxt = f.read()
except FileNotFoundError:
    logtxt = ""
check("domo.log 记录报告生成失败（ERROR）",
      "summary report generation failed" in logtxt,
      [ln for ln in logtxt.splitlines() if "report" in ln.lower()][-3:])

print("[report-smoke] 6) 修复 planner 后手工重试 → ready")
s, _ = patch(f"/api/agents/{P_id}", {"config": {"duration_ms": 120}})
check("修复 planner（清掉 report_fail）", s == 200, s)
s, trig = post(f"/api/projects/{pid}/report", {})
check("POST /report 触发重试", s == 200 and (trig.get("report") or {}).get("status") in ("pending", "generating"),
      f"{s} {trig}")
ready = wait_until(lambda: (report_of(pid) or {}).get("status") == "ready", 40)
rep = report_of(pid)
check("手工重试 → status=ready", ready is not None, rep and rep.get("status"))
check("累计尝试次数 >= 2（自动失败 + 手工重试）", rep and rep.get("attempts", 0) >= 2, rep and rep.get("attempts"))
content = (rep or {}).get("content") or ""

print("[report-smoke] 7) 报告内容覆盖验收标准")
for needle, label in [
    ("项目总结报告", "报告标题"),
    ("打好地基", "里程碑一 goal"),
    ("上层功能", "里程碑二 goal"),
    ("通过（gate 已确认", "里程碑 gate 结果（通过）"),
    ("任务状态统计", "任务状态统计章节"),
    ("退回 / 升级 / 挂起事件摘要", "退回/升级/挂起事件章节"),
    ("N2 验收退回升级", "验收退回事件（N2）"),
    ("N1 执行失败挂起", "挂起事件（N1）"),
    ("artifact://", "产物路径清单"),
]:
    check(f"报告含「{label}」：{needle}", needle in content)
check("报告含挂起原因 failure_limit", "failure_limit" in content, "failure_limit" not in content)
check("报告含升级原因 rejection_limit 或 escalation", ("rejection_limit" in content) or ("升级" in content))

print("[report-smoke] 8) 报告落盘到项目数据目录")
md_path = os.path.join(RUN_DIR, "projects", pid, "summary-report.md")
json_path = os.path.join(RUN_DIR, "projects", pid, "summary-report.json")
check("落盘 summary-report.md 存在且非空", os.path.isfile(md_path) and os.path.getsize(md_path) > 0, md_path)
check("落盘 summary-report.json 事实快照存在", os.path.isfile(json_path), json_path)
check("落盘内容与 API content 一致", os.path.isfile(md_path) and open(md_path, encoding="utf-8").read() == content)

print("[report-smoke] 9) 项目 B：planner 正常 → 报告自动 ready")
s, projB = post("/api/projects", {"name": "报告冒烟B", "brief": "验证自动生成"})
pidB = projB.get("id")
patch(f"/api/projects/{pidB}", {"workspace": {"kind": "local", "path": WS}})
post(f"/api/projects/{pidB}/confirm", {})
s, M3 = post(f"/api/projects/{pidB}/milestones", {"name": "唯一阶段", "goal": "自动报告目标"})
M3_id = M3.get("id")
# mk_task 绑定的是项目 A，这里为项目 B 单独创建
s, T5 = post(f"/api/projects/{pidB}/tasks", {"title": "B1 任务", "description": "d",
                                             "milestone_id": M3_id, "executor_agent_id": E_id,
                                             "acceptor_agent_id": C_id, "acceptance_criteria": "OK"})
check("项目 B 任务创建", s == 200 and T5, f"{s}")
gateB = wait_until(lambda: open_gate_for(M3_id), 60)
check("项目 B：里程碑 gate 出现", gateB is not None)
if gateB:
    post(f"/api/inbox/{gateB['id']}/resolve", {"resolution": "pass", "note": "冒烟通过"})
doneB = wait_until(lambda: project_detail(pidB).get("project", {}).get("status") == "completed", 200)
check("项目 B 自动 completed", doneB is not None)
readyB = wait_until(lambda: (report_of(pidB) or {}).get("status") == "ready", 40)
repB = report_of(pidB)
check("项目 B 报告自动 ready（无需手工重试）", readyB is not None, repB and repB.get("status"))
check("项目 B 报告 attempts == 1（一次自动成功）", repB and repB.get("attempts") == 1, repB and repB.get("attempts"))
check("项目 B 报告含自身里程碑 goal", repB and "自动报告目标" in (repB.get("content") or ""))
check("项目 B 报告落盘", os.path.isfile(os.path.join(RUN_DIR, "projects", pidB, "summary-report.md")))

print("[report-smoke] 10) SSE 与前端查看入口")
kinds = {ev for ev, _ in sse_frames()}
check("SSE 推送 `report` 事件（前端实时刷新入口）", "report" in kinds, sorted(kinds))

s, html = get("/", raw=True)
js_paths = re.findall(r'src="([^"]+\.js)"', html)
bundle = ""
for p in js_paths:
    st, txt = get(p if p.startswith("/") else "/" + p, raw=True)
    if st == 200:
        bundle += txt
for marker, label in [("项目总结报告", "报告弹窗标题"), ("重试生成", "失败重试按钮"),
                      ("报告生成失败", "失败态提示"), ("里程碑汇报", "报告章节渲染")]:
    check(f"bundle 含「{label}」标记：{marker}", marker in bundle)

failed = [n for n, ok in CHECKS if not ok]
print()
print(f"[report-smoke] {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
if failed:
    print("[report-smoke] FAILED:")
    for n in failed:
        print("  - " + n)
    sys.exit(1)
print("[report-smoke] ALL PASS")
PY
RC=$?

if [[ $RC -ne 0 ]]; then
  echo "[report-smoke] server log tail:"
  tail -40 "$LOG" 2>/dev/null || true
fi
exit $RC
