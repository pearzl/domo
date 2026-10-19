#!/usr/bin/env bash
# M4 三档恢复降级冒烟（真实 opencode resume + mock）——中断恢复全链路。
#
# 覆盖验收口径（详见 docs/SMOKE_RECOVERY_TIERS.md）：
#   用例 4  真实 opencode 中断 → kill -9 重启 → tier=native_resume 原生恢复（attempt 不自增、
#           逻辑会话名不变、`--session <崩溃前同一真实id>`）→ 跑完；含改造前/改造后对照断言（口径 1）。
#   用例 5  supports_resume=false 的真实 agent → 第一档 skipped → 第二档交接摘要重建
#           （交接书五节 + 落盘 + 新 session id ≠ 原 id）→ 跑完。
#   用例 6  mock 适配器（supports_resume=false）→ 跳过①直接②，reason 注明 supports_resume=false。
#   用例 7  持久化的真实 session id 置为无效值 → 第一档 failed（含失败原因）→ 自动降级②跑完。
#   用例 9  两档均失败（工作区失效）→ 任务 suspended + 收件箱（payload 同含两档失败原因）；
#           无关分支任务不被阻塞继续跑完。
#   用例 10 tier=manual 挂起任务 kill -9 重启后再进一轮三档 → 成功脱离 suspended、
#           原收件箱条目关闭（不重复堆叠）。
#   用例 11 项目 paused 期间中断任务不自动恢复（无新派发/无新 task.recovered），resume 后按三档续跑。
#   用例 12 任何恢复路径下同一 task 只有一个在途执行 worker/句柄（DB 占用表 + live_handles 断言）。
#   事件契约 task.recovered（tier/outcome/reason/task_id/attempt/session_id，append-only，时间线与审计可见）。
#
# 用法：DOMO_RT_MODEL=<provider>/<model> bash tests/smoke_recovery_tiers.sh   （退出码 0 = 全部通过）
#       DOMO_RT_PORT=8797 DOMO_RT_MODEL=<provider>/<model> bash tests/smoke_recovery_tiers.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_RT_PORT:-8797}"
MODEL="${DOMO_RT_MODEL:?请通过 DOMO_RT_MODEL 指定真实模型（如 provider/model）}"
TMP_ROOT="${TMPDIR:-/tmp}"
RUN_DIR="$(mktemp -d "$TMP_ROOT/domo_recovery_tiers.XXXXXX")"
DB="$RUN_DIR/domo.db"
WS="$RUN_DIR/workspace"
LOG="$RUN_DIR/domo.log"
RESULTS="$RUN_DIR/results.md"
PY="$RUN_DIR/smoke.py"
mkdir -p "$WS"

echo "[rt-smoke] model = $MODEL"
echo "[rt-smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[rt-smoke] build failed"; exit 1; }

echo "[rt-smoke] smoke driver -> $PY"
cat >"$PY" <<'PY'
import json, os, re, signal, sqlite3, subprocess, sys, time, urllib.request, urllib.error

PORT, RUN_DIR, WS, DB, LOG, MODEL, ROOT = sys.argv[1:8]
PORT = int(PORT)
BASE = f"http://127.0.0.1:{PORT}/api"
RESULTS = os.path.join(RUN_DIR, "results.md")
# DOMO_SERVER_BIN 可覆盖服务端二进制（隔离运行时避开按进程名的批量清理）
SERVER_BIN = os.environ.get("DOMO_SERVER_BIN") or os.path.join(ROOT, "server/target/debug/domo-server")
SERVER = None
CHECKS, NOTES, SAMPLERS = [], [], {}

def check(name, cond, detail=""):
    cond = bool(cond)
    CHECKS.append((name, cond, "" if cond else str(detail)))
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  -- " + str(detail)) if (detail and not cond) else ""), flush=True)

def note(msg):
    NOTES.append(msg)
    print("  NOTE  " + msg, flush=True)

def section(title):
    print(f"\n[rt-smoke] {title}", flush=True)
    with open(RESULTS, "a") as f:
        f.write(f"\n### {title}\n\n")

# ---------------- process control ----------------

def start_server():
    global SERVER
    env = dict(os.environ, DOMO_DB=DB, DOMO_PORT=str(PORT), DOMO_LOG=LOG, DOMO_LOG_LEVEL="info")
    SERVER = subprocess.Popen(
        [SERVER_BIN], cwd=os.path.join(ROOT, "server"), env=env,
        stdout=open(os.path.join(RUN_DIR, "stdout.log"), "ab"), stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    for _ in range(240):
        try:
            if req("GET", "/agents")[0] == 200:
                return True
        except Exception:
            pass
        time.sleep(0.25)
    return False

def crash():
    """kill -9 服务进程 + 其全部子进程（真实 opencode CLI 子进程一并杀死=整机级崩溃语义）。"""
    global SERVER
    if SERVER is not None:
        try:
            os.killpg(os.getpgid(SERVER.pid), signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            SERVER.wait(timeout=15)
        except Exception:
            pass
    SERVER = None
    time.sleep(1.0)

# ---------------- api helpers ----------------

def req(method, path, body=None, timeout=90):
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

def get(p, timeout=90): return req("GET", p, timeout=timeout)
def post(p, b=None, timeout=90): return req("POST", p, b if b is not None else {}, timeout=timeout)
def patch(p, b, timeout=90): return req("PATCH", p, b, timeout=timeout)

def task_of(tid):
    s, b = get(f"/tasks/{tid}")
    return b.get("task") if s == 200 else None

def detail_of(tid):
    s, b = get(f"/tasks/{tid}")
    return b if s == 200 else {}

def project_status(pid):
    s, b = get(f"/projects/{pid}")
    return ((b.get("project") or {}).get("status") if s == 200 else None)

def events(pid):
    s, b = get(f"/events?project_id={pid}")
    return b if s == 200 else []

def recovered_events(pid=None, tid=None):
    out = []
    for p in ([pid] if pid else [P1, P2]):
        for e in events(p):
            if e["type"] == "task.recovered" and (tid is None or e.get("task_id") == tid):
                out.append(e)
    return out

def inbox():
    s, b = get("/inbox")
    return b if s == 200 else []

def amap_caps(agent_id):
    s, b = get("/agents")
    for a in (b if s == 200 else []):
        if a.get("id") == agent_id:
            return a.get("capabilities") or {}
    return {}

def open_recovery_items(tid):
    return [i for i in inbox()
            if i.get("status") == "open" and i.get("type") == "escalation"
            and (i.get("payload") or {}).get("task_id") == tid
            and (i.get("payload") or {}).get("reason") == "recovery_exhausted"]

# ---------------- db helpers (sqlite, direct) ----------------

def db_query(sql, args=()):
    conn = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    try:
        return conn.execute(sql, args).fetchall()
    finally:
        conn.close()

def db_exec(sql, args=()):
    conn = sqlite3.connect(DB)
    try:
        conn.execute(sql, args)
        conn.commit()
    finally:
        conn.close()

def session_map_real(logical):
    rows = db_query("SELECT real FROM session_map WHERE logical=?", (logical,))
    return rows[0][0] if rows else None

def occupancy_count(tid):
    n = 0
    for (s,) in db_query("SELECT current_task_ids FROM agents"):
        try:
            ids = json.loads(s or "[]")
        except Exception:
            ids = []
        n += sum(1 for x in ids if x == tid)
    return n

def log_lines(tid):
    return [r[0] or "" for r in db_query("SELECT line FROM logs WHERE task_id=? ORDER BY id", (tid,))]

def log_has(tid, needle):
    return any(needle in l for l in log_lines(tid))

# ---------------- sampling (口径 12：无重复并发 worker） ----------------

class Sampler:
    def __init__(self, tag):
        self.tag = tag
        self.max_live = 0
        self.max_occ = 0
        self.n = 0
        self.saw_live = False
        SAMPLERS[tag] = self

    def sample(self, tid):
        d = detail_of(tid)
        live = int(d.get("live_handles") or 0)
        occ = occupancy_count(tid)
        self.max_live = max(self.max_live, live)
        self.max_occ = max(self.max_occ, occ)
        self.n += 1
        if live == 1:
            self.saw_live = True

def wait_status(tid, want, timeout=420, sampler=None, interval=0.5):
    """轮询任务状态直至命中；每轮可附带采样（句柄数/占用表）。"""
    if isinstance(want, str):
        want = {want}
    end = time.time() + timeout
    while time.time() < end:
        if sampler:
            sampler.sample(tid)
        t = task_of(tid)
        st = (t or {}).get("status")
        if st in want:
            return st
        time.sleep(interval)
    return None

def wait_until(fn, timeout=120, interval=0.5):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(interval)
    return None

# ---------------- task/agent/project setup ----------------

def mk_agent(name, roles, adapter="mock", supports_resume=False, extra=None):
    body = {"name": name, "adapter": adapter, "roles": roles,
            "capabilities": {"max_parallel": 1, "supports_resume": supports_resume}}
    cfg = {}
    if adapter == "opencode":
        cfg = {"model": MODEL, "timeout_ms": 300000}
    if extra:
        cfg.update(extra)
    if cfg:
        body["config"] = cfg
    s, b = post("/agents", body)
    return b.get("id") if s == 200 else None

def mk_project(name, brief):
    s, b = post("/projects", {"name": name, "brief": brief,
                              "workspace": {"kind": "local", "path": WS}})
    pid = b.get("id")
    patch(f"/projects/{pid}", {"workspace": {"kind": "local", "path": WS}})
    post(f"/projects/{pid}/confirm", {})
    return pid

def mk_task(pid, title, desc, ex, acc):
    s, b = post(f"/projects/{pid}/tasks", {
        "title": title, "description": desc,
        "executor_agent_id": ex, "acceptor_agent_id": acc,
        "negotiation_status": "agreed"})
    return b.get("id") if s == 200 else None

REAL_DESC = ("在当前工作目录创建文件 {f}，内容为 12 行：第 1 行到第 12 行，"
             "每行一句简短英文。完成后回复 TASK_DONE 并简述产出。")

def wait_session_then_crash(tid, timeout=600):
    """等执行真正开始（可能先经历计划制定/审查阶段，且可能有打回重拟）→ 等该 attempt 的
    真实会话 id **已落 SQLite session_map**（=满足第一档 resume 前置条件）→ 立刻 kill -9 整个进程组。
    返回 (真实会话 id, 崩溃时刻状态, 中断时的 attempt)。"""
    st = wait_status(tid, "executing", timeout)
    attempt = int((task_of(tid) or {}).get("attempt") or 0)
    if st != "executing":
        return None, st, attempt
    attempt = int((task_of(tid) or {}).get("attempt") or 1)
    logical = f"{tid}_a{attempt}"
    sid = None
    end = time.time() + timeout
    while time.time() < end:
        t = task_of(tid) or {}
        if t.get("status") != "executing":
            return None, t.get("status"), attempt  # 执行在持久化前就结束了（罕见）
        sid = session_map_real(logical)
        if sid:
            break
        time.sleep(0.3)
    crash()
    return sid, (task_of(tid) or {}).get("status"), attempt

def wait_mock_then_crash(tid, timeout=180):
    """mock 执行 worker 已在途（executing + live_handles>=1）→ kill -9 整个进程组。"""
    end = time.time() + timeout
    live = 0
    while time.time() < end:
        d = detail_of(tid)
        if (d.get("task") or {}).get("status") == "executing":
            live = int(d.get("live_handles") or 0)
            if live >= 1:
                break
        time.sleep(0.3)
    status_now = (task_of(tid) or {}).get("status")
    attempt_used = int((task_of(tid) or {}).get("attempt") or 0)
    crash()
    return status_now, attempt_used

def contract_ok(e):
    p = e.get("payload") or {}
    ok = all(k in p for k in ("tier", "outcome", "reason", "task_id", "attempt", "session_id"))
    ok = ok and p.get("tier") in ("native_resume", "handoff", "manual")
    ok = ok and p.get("outcome") in ("succeeded", "failed", "skipped", "suspended")
    return ok, p

def sampler_checks(tag, tid, expect_ran=True):
    s = SAMPLERS.get(tag)
    if not s:
        check(f"[12] {tag} 采样存在", False, "no samples")
        return
    check(f"[12] {tag} 同一 task 在途 worker 句柄恒 ≤1（live_handles 采样 {s.n} 次）",
          s.max_live <= 1, f"max={s.max_live}")
    check(f"[12] {tag} DB 占用表无重复占用（occupancy 采样 {s.n} 次）",
          s.max_occ <= 1, f"max={s.max_occ}")
    if expect_ran:
        check(f"[12] {tag} 恢复期间确有在途 worker（曾采样到 live_handles==1）", s.saw_live)

# ======================================================================
P1 = P2 = None
IDS = {}

def main():
    global P1, P2, IDS
    section("0) 团队池与项目（真实 opencode + mock 混编）")
    IDS["exec-native"] = mk_agent("oc-exec", ["executor"], "opencode", True)   # 第一档可用
    IDS["exec-nor"] = mk_agent("oc-exec-nr", ["executor"], "opencode", False)  # 第一档跳过（真实会话 id）
    IDS["exec-mock"] = mk_agent("mock-exec", ["executor"], "mock", False, {"duration_ms": 30000})
    IDS["exec-mock-slow"] = mk_agent("mock-slow", ["executor"], "mock", False, {"duration_ms": 90000})  # 第一档跳过（mock）
    IDS["acc"] = mk_agent("mock-acc", ["acceptor"], "mock", False)
    check("5 个 agent 注册成功（opencode×2 + mock×3，含长时 mock）", all(IDS.values()), IDS)
    s, b = get("/agents")
    amap = {a["id"]: a for a in b} if s == 200 else {}
    check("oc-exec 声明 supports_resume=true", amap.get(IDS["exec-native"], {}).get("capabilities", {}).get("supports_resume") is True)
    check("oc-exec-nr 声明 supports_resume=false", amap.get(IDS["exec-nor"], {}).get("capabilities", {}).get("supports_resume") is False)
    P1 = mk_project("M4恢复-真实opencode", "三档恢复真实链路（原生 resume / 交接重建 / 人工重建）")
    P2 = mk_project("M4恢复-mock", "三档恢复 mock 链路（跳过原生档直接交接重建）")
    check("两个项目创建并 running", bool(P1) and bool(P2) and project_status(P1) == "running" and project_status(P2) == "running",
          (P1, P2, project_status(P1), project_status(P2)))

    if not case4():
        return
    if not case5():
        return
    case6()
    case7()
    case9_and_10()
    case11()
    contract_and_audit()

# ---------------- 用例 4：真实 opencode 原生恢复 ----------------
def case4():
    section("用例 4 / 口径 1+3+4：真实 opencode 中断 → kill -9 → tier=native_resume 原生恢复")
    tid = mk_task(P1, "C4-原生恢复", REAL_DESC.format(f="c4_native.txt"), IDS["exec-native"], IDS["acc"])
    check("[4.1] C4 任务创建并进入执行", bool(tid) and wait_status(tid, "executing", 600) == "executing", tid)
    sid, st, attempt_pre = wait_session_then_crash(tid)   # attempt_pre = 中断时的 attempt
    check("[4.2] C4 中断前已进入执行轮（attempt>=1，恢复不新建 attempt）", attempt_pre >= 1, (attempt_pre, st))
    check("[4.3] C4 崩溃前已解析到真实 opencode 会话 id 并落 SQLite", bool(sid), (sid, st))
    logical = f"{tid}_a{attempt_pre}"
    check("[4.4] 崩溃前 session_map 持久化 logical→real 映射", session_map_real(logical) == sid, (logical, session_map_real(logical)))
    pre_rows = db_query("SELECT real, task_id, phase, attempt FROM session_map WHERE logical=?", (logical,))
    if not start_server():
        check("[4] 服务重启成功", False, "server did not come up")
        return False
    smp = Sampler("C4-native")
    st = wait_status(tid, {"done", "awaiting_acceptance", "suspended"}, 600, smp)
    check("[4.5] 重启后任务自动恢复并跑完（awaiting_acceptance/done）", st in ("awaiting_acceptance", "done"), st)
    evs = recovered_events(tid=tid)
    nat = [e for e in evs if (e["payload"] or {}).get("tier") == "native_resume"]
    check("[4.6] 落 tier=native_resume 事件（outcome=succeeded）", len(nat) == 1 and nat[0]["payload"].get("outcome") == "succeeded",
          [(e["payload"] or {}).get("outcome") for e in nat])
    okc, p = contract_ok(nat[0]) if nat else (False, {})
    check("[4.7] 事件契约字段齐全（tier/outcome/reason/task_id/attempt/session_id）", okc, p)
    check("[4.8] 恢复事件 session_id == 崩溃前真实 id", p.get("session_id") == sid, (p.get("session_id"), sid))
    t1 = task_of(tid) or {}
    check("[4.9] attempt 未自增（恢复后 attempt == 中断前 attempt=第一档落地证据）",
          t1.get("attempt") == attempt_pre, (t1.get("attempt"), attempt_pre))
    check("[4.10] 恢复派发的逻辑会话名不变（exec_session(task_id,attempt)）",
          any((e.get("payload") or {}).get("session_id") == f"{tid}_a{attempt_pre}"
              and (e.get("payload") or {}).get("trigger") == "recovery"
              for e in events(P1) if e["type"] == "agent.session_started"),
          [ (e.get("payload") or {}).get("session_id") for e in events(P1) if e["type"] == "agent.session_started" ])
    check("[4.11] 恢复派发使用 --session <崩溃前同一真实id>", log_has(tid, f"--session {sid}"))
    post_rows = db_query("SELECT real, task_id, phase, attempt FROM session_map WHERE logical=?", (logical,))
    check("[4.12] 重启后 DB 中真实 id 与崩溃前一致（事件/DB 双证）", pre_rows == post_rows, (pre_rows, post_rows))
    check("[4.13] 仅第一档：无 handoff / manual 事件（逐档判定）",
          not any((e["payload"] or {}).get("tier") in ("handoff", "manual") for e in evs),
          [(e["payload"] or {}).get("tier") for e in evs])
    # 基线差异对照（口径 1）：改造前对无活句柄的 executing 任务走 dispatch_exec(…"recovery")
    # → attempt+1 → 全新会话 {task}_a2 重派。改造后这两类痕迹都必须不存在。
    check("[4.14] 对照断言：改造前行为痕迹不存在（无 {task}_a{pre+1} 全新会话重派、attempt 未自增）",
          not any((e.get("payload") or {}).get("session_id") == f"{tid}_a{attempt_pre + 1}"
                  for e in events(P1) if e["type"] == "agent.session_started")
          and (task_of(tid) or {}).get("attempt") == attempt_pre,
          ((task_of(tid) or {}).get("attempt"), attempt_pre))
    sampler_checks("C4-native", tid)
    return True

# ---------------- 用例 5：真实 opencode 交接摘要重建 ----------------
def case5():
    section("用例 5 / 口径 2+5：supports_resume=false → 第一档 skipped → 第二档交接重建")
    tid = mk_task(P1, "C5-交接重建", REAL_DESC.format(f="c5_handoff.txt"), IDS["exec-nor"], IDS["acc"])
    check("[5.1] C5 任务创建并进入执行", bool(tid) and wait_status(tid, "executing", 600) == "executing", tid)
    t0 = task_of(tid) or {}
    check("[5.1b] C5 执行者固定为 oc-exec-nr（supports_resume=false，动态选人不覆盖固定）",
          t0.get("executor_agent_id") == IDS["exec-nor"] and t0.get("agent_pinned") is True
          and (amap_caps(IDS["exec-nor"]).get("supports_resume") is False),
          (t0.get("executor_agent_id"), t0.get("agent_pinned")))
    sid, st, attempt_pre = wait_session_then_crash(tid)
    check("[5.2] 崩溃前解析到真实会话 id", bool(sid), (sid, st))
    if not start_server():
        check("[5] 服务重启成功", False)
        return False
    smp = Sampler("C5-handoff")
    st = wait_status(tid, {"done", "awaiting_acceptance", "suspended"}, 600, smp)
    check("[5.3] 交接重建后跑完（awaiting_acceptance/done）", st in ("awaiting_acceptance", "done"), st)
    evs = recovered_events(tid=tid)
    nat = [e for e in evs if (e["payload"] or {}).get("tier") == "native_resume"]
    hof = [e for e in evs if (e["payload"] or {}).get("tier") == "handoff"]
    check("[5.4] 第一档 skipped 且 reason 注明 supports_resume=false",
          len(nat) == 1 and nat[0]["payload"].get("outcome") == "skipped"
          and "supports_resume=false" in (nat[0]["payload"].get("reason") or ""),
          [((e["payload"] or {}).get("outcome"), (e["payload"] or {}).get("reason")) for e in nat])
    check("[5.5] 第二档 handoff 事件且 outcome=succeeded", len(hof) == 1 and hof[0]["payload"].get("outcome") == "succeeded",
          [(e["payload"] or {}).get("outcome") for e in hof])
    hp = (hof[0]["payload"] if hof else {}) or {}
    doc = hp.get("handoff") or {}
    missing = [k for k in ("目标", "已完成", "剩余", "约束", "产物路径") if not str(doc.get(k) or "").strip()]
    check("[5.6] 交接书五节齐全且非空（目标/已完成/剩余/约束/产物路径）", not missing, missing)
    check("[5.7] 交接书内嵌 Markdown 含五节标题",
          all(f"## {k}" in (doc.get("markdown") or "") for k in ("目标", "已完成", "剩余", "约束", "产物路径")))
    hpath = doc.get("path") or ""
    check("[5.8] 交接书落盘且事件记录路径（文件存在可复查）",
          bool(hpath) and os.path.isfile(hpath) and os.path.getsize(hpath) > 0, hpath)
    new_sid = hp.get("session_id")
    check("[5.9] 第二档使用新 session id ≠ 原 session id（事件可证）",
          bool(new_sid) and new_sid != sid, (new_sid, sid))
    check("[5.10] 第二档新会话在日志可证（执行日志出现新会话 id）", bool(new_sid) and log_has(tid, f"opencode session: {new_sid}"))
    check("[5.11] attempt 未自增（交接沿用同一中断 attempt）", (task_of(tid) or {}).get("attempt") == attempt_pre,
          ((task_of(tid) or {}).get("attempt"), attempt_pre))
    check("[5.12] 交接会话逻辑名 = {task}_a{pre}_h1（新会话，非原 exec_session 名）",
          any((e.get("payload") or {}).get("session_id") == f"{tid}_a{attempt_pre}_h1"
              and (e.get("payload") or {}).get("trigger") == "recovery"
              for e in events(P1) if e["type"] == "agent.session_started"))
    sampler_checks("C5-handoff", tid)
    return True

# ---------------- 用例 6：mock（supports_resume=false） ----------------
def case6():
    section("用例 6 / 口径 6：mock 适配器中断 → 跳过①直接②（reason 注明 supports_resume=false）")
    tid = mk_task(P2, "C6-mock交接", "模拟任务，稍后完成", IDS["exec-mock"], IDS["acc"])
    check("[6.1] C6 任务创建并进入执行", bool(tid) and wait_status(tid, "executing", 600) == "executing", tid)
    t0 = task_of(tid) or {}
    check("[6.1b] C6 执行者固定为 mock-exec（supports_resume=false）",
          t0.get("executor_agent_id") == IDS["exec-mock"] and t0.get("agent_pinned") is True,
          (t0.get("executor_agent_id"), t0.get("agent_pinned")))
    _, attempt_pre = wait_mock_then_crash(tid)
    if not start_server():
        check("[6] 服务重启成功", False)
        return
    smp = Sampler("C6-mock")
    st = wait_status(tid, "done", 300, smp)
    check("[6.2] 交接续作后跑完 done", st == "done", st)
    evs = recovered_events(tid=tid)
    nat = [e for e in evs if (e["payload"] or {}).get("tier") == "native_resume"]
    hof = [e for e in evs if (e["payload"] or {}).get("tier") == "handoff"]
    check("[6.3] 跳过第一档：tier=native_resume outcome=skipped",
          len(nat) == 1 and nat[0]["payload"].get("outcome") == "skipped",
          [((e["payload"] or {}).get("tier"), (e["payload"] or {}).get("outcome")) for e in evs])
    check("[6.4] reason 注明 supports_resume=false（跳过①直接②）",
          "supports_resume=false" in ((nat[0]["payload"].get("reason") if nat else "") or ""))
    check("[6.5] tier=handoff 事件 outcome=succeeded，且 reason 汇报第一档 skip 原因",
          len(hof) == 1 and hof[0]["payload"].get("outcome") == "succeeded"
          and "supports_resume=false" in (hof[0]["payload"].get("reason") or ""),
          [(e["payload"] or {}).get("outcome") for e in hof])
    check("[6.6] attempt 未自增（恢复沿用中断 attempt）", (task_of(tid) or {}).get("attempt") == attempt_pre,
          ((task_of(tid) or {}).get("attempt"), attempt_pre))
    okc, _ = contract_ok(hof[0]) if hof else (False, {})
    check("[6.7] handoff 事件契约字段齐全", okc)
    sampler_checks("C6-mock", tid)

# ---------------- 用例 7：第一档强制失败降级 ----------------
def case7():
    section("用例 7 / 口径 7：持久化真实 session id 置无效 → 第一档 failed → 自动降级②")
    tid = mk_task(P1, "C7-强制失败降级", REAL_DESC.format(f="c7_forced.txt"), IDS["exec-native"], IDS["acc"])
    check("[7.1] C7 任务创建并进入执行", bool(tid) and wait_status(tid, "executing", 600) == "executing", tid)
    sid, st, attempt_pre = wait_session_then_crash(tid)
    check("[7.2] 崩溃前解析到真实会话 id", bool(sid), (sid, st))
    logical = f"{tid}_a{attempt_pre}"
    db_exec("UPDATE session_map SET real=? WHERE logical=?", ("corrupted-not-a-session-id", logical))
    check("[7.3] 已将持久化 session id 置为无效值（模拟 resume 失败）",
          session_map_real(logical) == "corrupted-not-a-session-id", (logical, session_map_real(logical)))
    if not start_server():
        check("[7] 服务重启成功", False)
        return
    smp = Sampler("C7-forced")
    st = wait_status(tid, {"done", "awaiting_acceptance", "suspended"}, 600, smp)
    check("[7.4] 自动降级交接重建后跑完", st in ("awaiting_acceptance", "done"), st)
    evs = recovered_events(tid=tid)
    nat = [e for e in evs if (e["payload"] or {}).get("tier") == "native_resume"]
    hof = [e for e in evs if (e["payload"] or {}).get("tier") == "handoff"]
    check("[7.5] 落 tier=native_resume outcome=failed 且含失败原因",
          len(nat) == 1 and nat[0]["payload"].get("outcome") == "failed"
          and bool((nat[0]["payload"].get("reason") or "").strip()),
          [((e["payload"] or {}).get("outcome"), (e["payload"] or {}).get("reason")) for e in nat])
    check("[7.6] 失败原因指向 CLI 报错（拒绝无效 --session id）",
          any(k in (nat[0]["payload"].get("reason") or "") for k in ("Expected a string starting with", "opencode run failed")),
          (nat[0]["payload"].get("reason") if nat else None))
    check("[7.7] 自动降级②：handoff succeeded 且新 session id ≠ 崩溃前 id",
          len(hof) == 1 and hof[0]["payload"].get("outcome") == "succeeded"
          and (hof[0]["payload"].get("session_id") or "") not in ("", sid),
          [(e["payload"] or {}).get("outcome") for e in hof])
    check("[7.8] attempt 未自增（降级轮次沿用同一 attempt）", (task_of(tid) or {}).get("attempt") == attempt_pre,
          ((task_of(tid) or {}).get("attempt"), attempt_pre))
    check("[7.9] 恢复档失败不计入「失败 3 次挂起」额度（failure_count 不变）",
          (task_of(tid) or {}).get("failure_count") == 0, (task_of(tid) or {}).get("failure_count"))
    sampler_checks("C7-forced", tid)
    return True

# ---------------- 用例 9 + 10：两档失败挂起 → 重启再恢复 ----------------
def case9_and_10():
    section("用例 9 / 口径 9+12：两档均失败 → suspended + 收件箱（附两档原因）；无关分支不受阻")
    tid = mk_task(P1, "C9-两档失败", REAL_DESC.format(f="c9_manual.txt"), IDS["exec-native"], IDS["acc"])
    tid2 = mk_task(P1, "C9-无关分支", "无关分支任务（mock，不需要工作区；长时执行确保被崩溃打断）", IDS["exec-mock-slow"], IDS["acc"])
    check("[9.1] C9 主任务与无关分支任务创建并进入执行", bool(tid) and bool(tid2)
          and wait_status(tid, "executing", 600) == "executing", (tid, tid2))
    sid, st, attempt_pre = wait_session_then_crash(tid)
    check("[9.2] 崩溃前解析到真实会话 id", bool(sid), (sid, st))
    broken = WS + "-BROKEN"
    rows = db_query("SELECT workspace FROM projects WHERE id=?", (P1,))
    ws = json.loads(rows[0][0]) if rows and rows[0][0] else {"kind": "local", "path": WS}
    ws["path"] = broken
    db_exec("UPDATE projects SET workspace=? WHERE id=?", (json.dumps(ws), P1))
    if not start_server():
        check("[9] 服务重启成功", False)
        return
    smp = Sampler("C9-manual")
    st = wait_status(tid, "suspended", 300, smp)
    check("[9.3] 两档均失败 → 任务 suspended（人工重建）", st == "suspended", st)
    evs = recovered_events(tid=tid)
    nat = [e for e in evs if (e["payload"] or {}).get("tier") == "native_resume"]
    hof = [e for e in evs if (e["payload"] or {}).get("tier") == "handoff"]
    man = [e for e in evs if (e["payload"] or {}).get("tier") == "manual"]
    check("[9.4] 第一档 outcome=failed（resume 启动失败：工作区失效）",
          len(nat) == 1 and nat[0]["payload"].get("outcome") == "failed",
          [(e["payload"] or {}).get("outcome") for e in nat])
    check("[9.5] 第二档 outcome=failed（交接会话派发失败）",
          len(hof) == 1 and hof[0]["payload"].get("outcome") == "failed",
          [(e["payload"] or {}).get("outcome") for e in hof])
    check("[9.6] 落 tier=manual outcome=suspended 事件", len(man) == 1 and man[0]["payload"].get("outcome") == "suspended",
          [(e["payload"] or {}).get("outcome") for e in man])
    mp = (man[0]["payload"] if man else {}) or {}
    check("[9.7] manual 事件 reason 逐档列明第一档与第二档失败原因",
          "第一档" in (mp.get("reason") or "") and "第二档" in (mp.get("reason") or ""),
          (mp.get("reason") or "")[:200])
    items = open_recovery_items(tid)
    check("[9.8] 收件箱新增 escalation 条目（reason=recovery_exhausted）", len(items) == 1, len(items))
    pl = (items[0].get("payload") if items else {}) or {}
    check("[9.9] 收件箱 payload 同时含第一档与第二档失败原因",
          bool(pl.get("tier1_reason")) and bool(pl.get("tier2_reason")), pl)
    check("[9.10] attempt 未自增、failure_count 未被恢复失败抬高",
          (task_of(tid) or {}).get("attempt") == attempt_pre and (task_of(tid) or {}).get("failure_count") == 0,
          ((task_of(tid) or {}).get("attempt"), attempt_pre, (task_of(tid) or {}).get("failure_count")))
    # C9 主任务两档都是「秒失败」（工作区失效），worker 存活极短：只断言句柄恒 ≤1；
    # 无关分支（mock 交接续作 20s）作为在途 worker 采样对象。
    sampler_checks("C9-manual", tid, expect_ran=False)
    smp2 = Sampler("C9-unrelated")
    check("[9.11] 无关分支任务不被阻塞（挂起期间继续跑完）", wait_status(tid2, "done", 300, smp2) == "done",
          (task_of(tid2) or {}).get("status"))
    sampler_checks("C9-unrelated", tid2)

    section("用例 10 / 口径 10：SUSPENDED 任务重启后再进一轮三档 → 脱离挂起 + 收件箱闭环")
    crash()
    db_exec("UPDATE projects SET workspace=? WHERE id=?", (json.dumps({"kind": "local", "path": WS}), P1))
    check("[10.1] 已恢复工作区并再次 kill -9（重启再恢复一轮）", True)
    if not start_server():
        check("[10] 服务重启成功", False)
        return
    smp = Sampler("C10-rering")
    st = wait_status(tid, {"done", "awaiting_acceptance"}, 600, smp)
    check("[10.2] 重启后重新进三档并成功脱离 suspended（done/awaiting_acceptance）",
          st in ("done", "awaiting_acceptance"), st)
    evs = recovered_events(tid=tid)
    nat2 = [e for e in evs if (e["payload"] or {}).get("tier") == "native_resume"]
    check("[10.3] 本轮 tier=native_resume outcome=succeeded（工作区修复后原生续作）",
          len(nat2) == 2 and nat2[-1]["payload"].get("outcome") == "succeeded",
          [(e["payload"] or {}).get("outcome") for e in nat2])
    check("[10.4] 原收件箱条目已关闭（resolved：auto_recovered/auto_retry 均为自动恢复闭环）",
          any(i.get("status") == "resolved"
              and (i.get("resolution") or {}).get("resolution") in ("auto_recovered", "auto_retry")
              for i in inbox() if (i.get("payload") or {}).get("task_id") == tid
              and (i.get("payload") or {}).get("reason") == "recovery_exhausted"))
    check("[10.5] 同一任务无重复堆叠收件箱条目（open 条目为 0）", len(open_recovery_items(tid)) == 0,
          len(open_recovery_items(tid)))
    check("[10.6] attempt 全程未自增", (task_of(tid) or {}).get("attempt") == attempt_pre,
          ((task_of(tid) or {}).get("attempt"), attempt_pre))
    check("[10.7] 原生续作仍用崩溃前同一真实 id",
          bool(nat2) and nat2[-1]["payload"].get("session_id") == sid, (nat2[-1]["payload"].get("session_id") if nat2 else None, sid))
    sampler_checks("C10-rering", tid)

# ---------------- 用例 11：暂停语义不冲突 ----------------
def case11():
    section("用例 11 / 口径 11：paused 期间中断任务不自动恢复，resume 后按三档续跑")
    tid = mk_task(P2, "C11-暂停不恢复", "暂停语义探针任务", IDS["exec-mock"], IDS["acc"])
    check("[11.1] C11 任务创建并进入执行", bool(tid) and wait_status(tid, "executing", 600) == "executing", tid)
    s, _ = post(f"/projects/{P2}/pause", {})
    check("[11.2] 项目暂停受理（在途会话跑完当前尝试即停：pausing→paused）",
          s == 200 and project_status(P2) in ("pausing", "paused"), (s, project_status(P2)))
    # 服务仍在线时固化崩溃前基线（事件水位 + 日志行数）
    before_ev = max((e["id"] for e in events(P2)), default=0)
    before_logs = len(log_lines(tid))
    wait_mock_then_crash(tid)
    if not start_server():
        check("[11] 服务重启成功", False)
        return
    time.sleep(4)
    check("[11.3] 暂停期间无新派发：任务保持中断态（executing/recovering，无在途 worker）",
          (task_of(tid) or {}).get("status") in ("executing", "recovering")
          and int(detail_of(tid).get("live_handles") or 0) == 0,
          ((task_of(tid) or {}).get("status"), detail_of(tid).get("live_handles")))
    after_ev = max((e["id"] for e in events(P2)), default=0)
    check("[11.4] 暂停期间无新 task.recovered / 无新事件（自动恢复被冻结）",
          not any(e["type"] == "task.recovered" and e["id"] > before_ev for e in events(P2)),
          [e["type"] for e in events(P2) if e["id"] > before_ev])
    check("[11.5] 暂停期间任务日志无新增（无任何执行/恢复派发）", len(log_lines(tid)) == before_logs,
          (len(log_lines(tid)), before_logs))
    check("[11.6] 恢复标记保留（recovery.pending=true，等待项目解冻）",
          (detail_of(tid).get("recovery") or {}).get("pending") is True, detail_of(tid).get("recovery"))
    check("[11.7] 项目状态不受影响（仍 paused）", project_status(P2) == "paused", project_status(P2))
    s, _ = post(f"/projects/{P2}/resume", {})
    check("[11.8] 项目 resume 后按三档流程恢复并跑完", s == 200 and wait_status(tid, "done", 300) == "done",
          (s, (task_of(tid) or {}).get("status")))
    evs = recovered_events(tid=tid)
    check("[11.9] resume 后恢复链路：native skipped → handoff succeeded",
          any((e["payload"] or {}).get("tier") == "native_resume" and (e["payload"] or {}).get("outcome") == "skipped" for e in evs)
          and any((e["payload"] or {}).get("tier") == "handoff" and (e["payload"] or {}).get("outcome") == "succeeded" for e in evs),
          [((e["payload"] or {}).get("tier"), (e["payload"] or {}).get("outcome")) for e in evs])
    check("[11.10] 暂停/恢复语义回归：恢复后项目 running、任务正常流转",
          project_status(P2) in ("running", "completed"), project_status(P2))

# ---------------- 事件契约与审计可见性 ----------------
def contract_and_audit():
    section("事件契约与审计（口径 8 / 13）")
    all_evs = events(P1) + events(P2)
    rec = [e for e in all_evs if e["type"] == "task.recovered"]
    bad = [e for e in rec if not contract_ok(e)[0]]
    check("[8.1] 全部 task.recovered 事件契约字段齐全且取值在枚举内", not bad, bad[:2])
    check("[8.2] 每档各落一条（成功/失败/跳过均含）：三档事件均出现过",
          {((e["payload"] or {}).get("tier")) for e in rec} >= {"native_resume", "handoff", "manual"},
          sorted({((e["payload"] or {}).get("tier") or "?") for e in rec}))
    outcomes = {((e["payload"] or {}).get("outcome")) for e in rec}
    check("[8.3] outcome 覆盖 succeeded/failed/skipped/suspended 四态",
          {"succeeded", "failed", "skipped", "suspended"} <= outcomes, sorted(outcomes))
    ids = [e["id"] for e in rec]
    per_proj_ok = True
    for p in (P1, P2):
        pids = [e["id"] for e in events(p) if e["type"] == "task.recovered"]
        if len(pids) != len(set(pids)) or pids != sorted(pids):
            per_proj_ok = False
    check("[8.4] append-only：事件 id 无重复且各项目时间线严格递增", len(ids) == len(set(ids)) and per_proj_ok, ids)
    check("[8.5] 任务详情时间线包含 task.recovered（前端可见）",
          any("task.recovered" in [e["type"] for e in (detail_of(t).get("events") or [])]
              for t in [e.get("task_id") for e in rec if e.get("task_id")][:5] if t))
    s, b = get(f"/events?project_id={P1}&type=task.recovered")
    check("[8.6] 审计视图按类型检索可见（type=task.recovered）",
          s == 200 and len(b) > 0 and all(e["type"] == "task.recovered" for e in b), s)
    log = ""
    try:
        with open(LOG, "r", errors="replace") as f:
            log = f.read()
    except FileNotFoundError:
        pass
    check("[13] 服务端运行日志含 opencode spawn/finished 记录", "opencode run spawn" in log and "opencode run finished" in log)
    check("[13] 服务端运行日志无 panic", "panicked at" not in log)
    global_smp_ok = all(s.max_live <= 1 and s.max_occ <= 1 for s in SAMPLERS.values())
    check("[12] 全部恢复路径采样汇总：无双在途 worker/句柄", global_smp_ok,
          {k: (s.max_live, s.max_occ) for k, s in SAMPLERS.items()})
    occ_dup = [r for (r,) in db_query("SELECT current_task_ids FROM agents") if len(json.loads(r or "[]")) != len(set(json.loads(r or "[]")))]
    check("[12] DB 占用表无重复条目（终检）", not occ_dup, occ_dup)

def dump_results():
    with open(RESULTS, "a") as f:
        f.write(f"\n## 汇总 — {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
        for n, ok, d in CHECKS:
            f.write(f"- {'PASS' if ok else 'FAIL'} — {n}" + (f"  _({d})_" if d else "") + "\n")
        if NOTES:
            f.write("\nNotes:\n")
            for x in NOTES:
                f.write(f"- {x}\n")
    failed = [n for n, ok, _ in CHECKS if not ok]
    print()
    print(f"[rt-smoke] {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
    if failed:
        print("[rt-smoke] FAILED:")
        for n in failed:
            print("  - " + n)
    return 1 if failed else 0

if __name__ == "__main__":
    try:
        if not start_server():
            print("[rt-smoke] server did not come up; stdout tail:")
            print(open(os.path.join(RUN_DIR, "stdout.log"), errors="replace").read()[-3000:])
            sys.exit(dump_results())
        main()
    except Exception:
        import traceback
        traceback.print_exc()
        check("[fatal] smoke driver crashed", False, "see traceback above")
    finally:
        crash()
    sys.exit(dump_results())
PY

python3 "$PY" "$PORT" "$RUN_DIR" "$WS" "$DB" "$LOG" "$MODEL" "$ROOT"
RC=$?

# ---------------- 归档：结果 + 关键日志落盘到仓库 data/ 下可复查 ----------------
ARCH_DIR="$ROOT/data/smoke_recovery_tiers"
mkdir -p "$ARCH_DIR"
RUN_NO=1
while [[ -e "$ARCH_DIR/run${RUN_NO}-results.md" ]]; do RUN_NO=$((RUN_NO + 1)); done
RUN_TAG="run${RUN_NO}"
cp -f "$RESULTS" "$ARCH_DIR/$RUN_TAG-results.md" 2>/dev/null || true
cp -f "$LOG" "$ARCH_DIR/$RUN_TAG-domo.log" 2>/dev/null || true
{
  echo "# M4 三档恢复冒烟结果（$RUN_TAG）";
  echo;
  echo "- 时间：$(date -Is)";
  echo "- 模型：$MODEL";
  echo "- 端口：$PORT";
  echo "- 运行目录：$RUN_DIR";
  echo "- 退出码：$RC";
  echo "- 结果明细：$ARCH_DIR/$RUN_TAG-results.md";
  echo "- 服务端日志：$ARCH_DIR/$RUN_TAG-domo.log";
  echo;
  cat "$RESULTS" 2>/dev/null || true;
} > "$ARCH_DIR/$RUN_TAG-summary.md"
cp -f "$RESULTS" "$ARCH_DIR/results.md" 2>/dev/null || true
cp -f "$LOG" "$ARCH_DIR/domo.log" 2>/dev/null || true
cp -f "$ARCH_DIR/$RUN_TAG-summary.md" "$ARCH_DIR/summary.md" 2>/dev/null || true
echo "[rt-smoke] 归档：$ARCH_DIR/{$RUN_TAG-summary.md,$RUN_TAG-results.md,$RUN_TAG-domo.log} + latest 别名"

if [[ $RC -ne 0 ]]; then
  echo "[rt-smoke] FAILED (rc=$RC); server log tail:"
  tail -60 "$LOG" 2>/dev/null || true
fi
exit $RC
