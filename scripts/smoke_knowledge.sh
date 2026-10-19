#!/usr/bin/env bash
# M5 项目知识库与经验沉淀 —— 端到端冒烟（mock agent）。
#
# 对着真实后端核对验收标准：
#   1. 条目模型：知识条目含正文/适用范围/来源事件引用/时间，随 SQLite 持久化，
#      在 API 层增删改查（CRUD 均写 `knowledge.*` 审计事件）。
#   2. 提示词注入：按项目把条目渲染成「高优先级规范段」注入 planner/executor/acceptor
#      三类角色提示词开头（`prompt.assembled` 事件可断言）；未注入项目不含该段。
#   3. 来源可追溯：每条条目回链到产生它的人工指导事件（source_event_id →
#      task.guidance_added，payload.text / guidance_id 逐项对得上）。
#   4. 跨项目复用：项目条目提升为全局后，其他项目可见并注入其提示词。
#   5. 重启恢复：`kill -9` 服务后同库重启，条目字段逐项一致、来源事件仍可解析。
#
# 用法：bash scripts/smoke_knowledge.sh   （退出码 0 = 全部通过）
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_KNOWLEDGE_SMOKE_PORT:-8799}"
TMP_ROOT="${TMPDIR:-/tmp}"
RUN_DIR="$(mktemp -d "$TMP_ROOT/domo_knowledge_smoke.XXXXXX")"
DB="$RUN_DIR/domo.db"
WS="$RUN_DIR/workspace"
LOG="$RUN_DIR/server.log"
mkdir -p "$WS"

echo "[knowledge-smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[knowledge-smoke] server build failed"; exit 1; }

if [[ ! -f "$ROOT/web/dist/index.html" ]]; then
  echo "[knowledge-smoke] building web (no dist)…"
  (cd "$ROOT/web" && npm install --silent && npm run build --silent) || { echo "[knowledge-smoke] web build failed"; exit 1; }
fi

echo "[knowledge-smoke] running driver (port $PORT, db $DB)"
python3 - "$PORT" "$DB" "$WS" "$LOG" "$ROOT" <<'PY'
import atexit, json, os, signal, subprocess, sys, time, urllib.request, urllib.error

PORT, DB, WS, LOG, ROOT = sys.argv[1:6]
BASE = f"http://127.0.0.1:{PORT}/api"
SERVER_BIN = os.path.join(ROOT, "server", "target", "debug", "domo-server")
DIST = os.path.join(ROOT, "web", "dist")
SECTION_MARK = "【项目规范（高优先级，必须遵守）】"

CHECKS = []          # (name, ok, detail)
CHECK_NAMES = []     # 保持执行顺序，用于与 EXPECTED_CHECKS 逐一比对

def check(name, cond, detail=""):
    cond = bool(cond)
    CHECKS.append((name, cond, "" if cond else str(detail)))
    CHECK_NAMES.append(name)
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
def delete(p, timeout=60): return req("DELETE", p, timeout=timeout)

def wait_until(fn, timeout=60, interval=0.4):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(interval)
    return None

# ---------- server lifecycle ----------
def start_server():
    env = dict(os.environ, DOMO_DB=DB, DOMO_PORT=PORT, DOMO_LOG=LOG, DOMO_LOG_LEVEL="warn")
    with open(os.path.join(os.path.dirname(DB), "stdout.log"), "ab") as out:
        return subprocess.Popen([SERVER_BIN], env=env, stdout=out, stderr=subprocess.STDOUT,
                                cwd=os.path.dirname(ROOT))

def wait_up(timeout=30):
    return wait_until(lambda: get("/agents")[0] == 200, timeout)

def kill9(proc):
    proc.send_signal(signal.SIGKILL)
    try: proc.wait(timeout=10)
    except Exception: pass

SERVER = start_server()
atexit.register(lambda: kill9(SERVER))  # 退出（含失败/异常）时确保不留孤儿服务进程
if not wait_up():
    print("[knowledge-smoke] server did not come up")
    sys.exit(1)

# ---------- helpers ----------
def events(pid):
    s, b = get(f"/events?project_id={pid}")
    return b if s == 200 else []

def etypes(pid):
    return [e["type"] for e in events(pid)]

def prompts(pid, role=None, phase=None):
    out = []
    for e in events(pid):
        if e["type"] != "prompt.assembled":
            continue
        p = e.get("payload") or {}
        if role and p.get("role") != role:
            continue
        if phase and p.get("phase") != phase:
            continue
        out.append(e)
    return out

def prompt_texts(pid, role, phase):
    return [(e.get("payload") or {}).get("prompt", "") for e in prompts(pid, role, phase)]

def any_prompt_contains(pid, role, phase, needle):
    return any(needle in t for t in prompt_texts(pid, role, phase))

def all_prompts_start_with_mark(pid):
    ts = [t for e in prompts(pid) for t in [(e.get("payload") or {}).get("prompt", "")]]
    return bool(ts) and all(t.startswith(SECTION_MARK) for t in ts)

def tasks_of(pid):
    s, b = get(f"/projects/{pid}")
    return (b or {}).get("tasks", []) if s == 200 else []

def wait_tasks_done(pid, n, timeout=90):
    return wait_until(lambda: len([t for t in tasks_of(pid) if t.get("status") == "done"]) >= n, timeout)

def new_project(name):
    s, b = post("/projects", {"name": name, "brief": f"M5 knowledge smoke ({name})",
                              "workspace": {"kind": "local", "path": WS}})
    return b.get("id") if s == 200 else None

def create_task(pid, title, desc, ex, acc):
    s, b = post(f"/projects/{pid}/tasks", {
        "title": title, "description": desc, "acceptance_required": True,
        "acceptance_criteria": "产物满足任务描述",
        "executor_agent_id": ex, "acceptor_agent_id": acc,
    })
    return b.get("id") if s == 200 else None

# ======================================================================
# 0) agent 池：mock planner / executor / acceptor
# ======================================================================
print("[knowledge-smoke] 0) agents")
s, planner = post("/agents", {"name": "kn-planner", "adapter": "mock", "roles": ["planner"],
                              "capabilities": {"max_parallel": 4, "supports_resume": False},
                              "config": {"duration_ms": 50}})
check("planner agent registered", s == 200 and planner.get("id"), f"{s} {planner}")
s, exec_a = post("/agents", {"name": "kn-exec", "adapter": "mock", "roles": ["executor"],
                             "capabilities": {"max_parallel": 4, "supports_resume": False},
                             "config": {"duration_ms": 50}})
check("executor agent registered", s == 200 and exec_a.get("id"), f"{s} {exec_a}")
s, acc_a = post("/agents", {"name": "kn-acc", "adapter": "mock", "roles": ["acceptor"],
                            "capabilities": {"max_parallel": 4, "supports_resume": False},
                            "config": {"duration_ms": 50}})
check("acceptor agent registered", s == 200 and acc_a.get("id"), f"{s} {acc_a}")

# ======================================================================
# 1) 项目 A：种子任务 → 人工指导 → 来源事件
# ======================================================================
print("[knowledge-smoke] 1) project A + guidance source event")
pid_a = new_project("知识库A")
check("project A created", bool(pid_a), pid_a)
seed_tid = create_task(pid_a, "种子任务（供写入人工指导）", "承载一条人工指导", exec_a["id"], acc_a["id"])
check("seed task created in A", bool(seed_tid), seed_tid)

GUID = "冒烟人工指导：所有改动必须附带可复现的验证命令。"
s, g = post(f"/tasks/{seed_tid}/guidance", {"text": GUID})
check("guidance posted returns guidance id", s == 200 and g.get("id"), f"{s} {g}")
guid_id = (g or {}).get("id")

gev = wait_until(lambda: next((e for e in events(pid_a)
                               if e["type"] == "task.guidance_added"
                               and (e.get("payload") or {}).get("guidance_id") == guid_id), None), 20)
check("guidance_added event carries guidance_id", gev is not None, "no matching guidance event")
src_event_id = gev["id"] if gev else -1
check("guidance event text matches what was sent",
      gev is not None and (gev.get("payload") or {}).get("text") == GUID,
      gev and gev.get("payload"))

# ======================================================================
# 2) 知识条目 CRUD + 校验 + 审计
# ======================================================================
print("[knowledge-smoke] 2) knowledge CRUD + validation + audit")
KBODY = "禁止在未经评审时直接向 main 分支 push。"
s, b = post(f"/projects/{pid_a}/knowledge", {"body": "", "source_event_id": src_event_id})
check("create rejected: empty body → 400", s == 400, f"{s} {b}")
s, b = post(f"/projects/{pid_a}/knowledge", {"body": KBODY, "source_event_id": 99999999})
check("create rejected: unknown source_event_id → 400", s == 400, f"{s} {b}")

s, k1 = post(f"/projects/{pid_a}/knowledge",
             {"body": KBODY, "applies_to": "全部任务", "source_event_id": src_event_id})
check("K1 created with full fields",
      s == 200 and k1.get("id") and k1.get("scope") == "project" and k1.get("body") == KBODY
      and k1.get("applies_to") == "全部任务" and k1.get("source_event_id") == src_event_id
      and k1.get("project_id") == pid_a and k1.get("created_at") and k1.get("updated_at"),
      f"{s} {k1}")
k1_id = (k1 or {}).get("id")

s, lst = get(f"/projects/{pid_a}/knowledge")
k1_in_list = next((x for x in lst if x.get("id") == k1_id), None) if s == 200 else None
check("K1 in project A list, field-by-field matches",
      k1_in_list == k1, f"{s} {k1_in_list} vs {k1}")

s, det = get(f"/knowledge/{k1_id}")
check("K1 detail GET field-by-field matches", s == 200 and det == k1, f"{s} {det}")
check("knowledge.viewed audit event emitted", "knowledge.viewed" in etypes(pid_a))

# 第二条条目：PATCH → DELETE（覆盖改与删的审计与无损性）
s, k2 = post(f"/projects/{pid_a}/knowledge",
             {"body": "临时条目（将被更新并删除）", "source_event_id": src_event_id})
k2_id = (k2 or {}).get("id")
check("K2 created", s == 200 and bool(k2_id), f"{s} {k2}")
s, k2u = patch(f"/knowledge/{k2_id}", {"body": "临时条目（已更新）", "applies_to": "仅本任务"})
check("K2 PATCH changes body/applies_to and bumps updated_at",
      s == 200 and k2u.get("body") == "临时条目（已更新）" and k2u.get("applies_to") == "仅本任务"
      and k2u.get("updated_at", 0) >= k2.get("updated_at", 0),
      f"{s} {k2u}")
s, k2g = get(f"/knowledge/{k2_id}")
check("K2 GET reflects the update", s == 200 and k2g == k2u, f"{s} {k2g}")

s, _ = delete(f"/knowledge/{k2_id}")
s2, lst2 = get(f"/projects/{pid_a}/knowledge")
ids2 = [x.get("id") for x in lst2] if s2 == 200 else []
check("K2 DELETE removes only K2 (K1 intact)",
      s == 200 and k2_id not in ids2 and k1_id in ids2, f"{s} {ids2}")

# 审计事件：created x2 / updated / deleted
def has_audit(evtype, pred):
    return any(e["type"] == evtype and pred(e.get("payload") or {}) for e in events(pid_a))
check("knowledge.created audit x2 with payloads",
      has_audit("knowledge.created", lambda p: p.get("id") == k1_id)
      and has_audit("knowledge.created", lambda p: p.get("id") == k2_id))
check("knowledge.updated audit reflects new fields",
      has_audit("knowledge.updated", lambda p: (p.get("entry") or {}).get("body") == "临时条目（已更新）"))
check("knowledge.deleted audit snapshots K2",
      has_audit("knowledge.deleted", lambda p: p.get("id") == k2_id))

# ======================================================================
# 3) 项目 A：三类角色提示词注入
# ======================================================================
print("[knowledge-smoke] 3) planner/executor/acceptor prompt injection (project A)")
s, b = post(f"/projects/{pid_a}/plan", {"engine": "opencode", "planner_agent_id": planner["id"]}, timeout=90)
check("project A planned by mock planner", s == 200 and b.get("tasks_added"), f"{s} {str(b)[:200]}")

check("planner prompt contains knowledge body (A)",
      any_prompt_contains(pid_a, "planner", "plan", KBODY))
check("planner prompt starts with knowledge section (A)",
      any(t.startswith(SECTION_MARK) for t in prompt_texts(pid_a, "planner", "plan")))

s, _ = post(f"/projects/{pid_a}/confirm", {})
check("project A confirmed (running)", s == 200, s)
wait_tasks_done(pid_a, 2, 90)

check("executor(plan_drafting) prompt contains knowledge body (A)",
      any_prompt_contains(pid_a, "executor", "plan_drafting", KBODY))
check("executor(execute) prompt contains knowledge body (A)",
      any_prompt_contains(pid_a, "executor", "execute", KBODY))
check("acceptor(plan_review) prompt contains knowledge body (A)",
      any_prompt_contains(pid_a, "acceptor", "plan_review", KBODY))
check("acceptor(accept) prompt contains knowledge body (A)",
      any_prompt_contains(pid_a, "acceptor", "accept", KBODY))
check("all assembled role prompts in A start with knowledge section",
      all_prompts_start_with_mark(pid_a))

# 来源可追溯：source_event_id → 正确的人工指导事件
s, evlist = get(f"/events?project_id={pid_a}&type=task.guidance_added")
src_ev = next((e for e in evlist if e.get("id") == src_event_id), None) if s == 200 else None
check("K1.source_event_id resolves to the guidance event",
      src_ev is not None and src_ev["type"] == "task.guidance_added"
      and (src_ev.get("payload") or {}).get("text") == GUID
      and (src_ev.get("payload") or {}).get("guidance_id") == guid_id,
      src_ev)

# ======================================================================
# 4) 项目 B：未注入 → 提示词不含该段
# ======================================================================
print("[knowledge-smoke] 4) project B isolation before promotion")
pid_b = new_project("知识库B")
check("project B created", bool(pid_b), pid_b)
s, b = post(f"/projects/{pid_b}/plan", {"engine": "opencode", "planner_agent_id": planner["id"]}, timeout=90)
check("project B planned by mock planner", s == 200 and b.get("tasks_added"), f"{s} {str(b)[:200]}")
s, _ = post(f"/projects/{pid_b}/confirm", {})
check("project B confirmed (running)", s == 200, s)
wait_tasks_done(pid_b, 2, 90)

s, blist = get(f"/projects/{pid_b}/knowledge")
b_ids = [x.get("id") for x in blist] if s == 200 else []
check("project B list excludes A's project-scope entry", k1_id not in b_ids, b_ids)
check("B planner prompt does NOT contain A's knowledge body",
      not any_prompt_contains(pid_b, "planner", "plan", KBODY))
check("B executor prompt does NOT contain A's knowledge body",
      not any_prompt_contains(pid_b, "executor", "execute", KBODY))
check("B acceptor prompt does NOT contain A's knowledge body",
      not any_prompt_contains(pid_b, "acceptor", "accept", KBODY))
check("B prompts carry no knowledge section mark",
      not any(t.startswith(SECTION_MARK)
              for role, ph in [("planner", "plan"), ("executor", "execute"), ("acceptor", "accept")]
              for t in prompt_texts(pid_b, role, ph)))

# ======================================================================
# 5) 提升为全局：B 可见 + 注入引用
# ======================================================================
print("[knowledge-smoke] 5) promote K1 to global and cross-project reference")
s, kp = post(f"/knowledge/{k1_id}/promote", {})
check("K1 promoted to global (scope=global)",
      s == 200 and kp.get("scope") == "global", f"{s} {kp}")
check("knowledge.promoted audit event emitted",
      has_audit("knowledge.promoted", lambda p: (p.get("entry") or {}).get("id") == k1_id))
s, again = post(f"/knowledge/{k1_id}/promote", {})
check("second promote is idempotent-conflict (409)", s == 409, f"{s} {again}")

s, blist2 = get(f"/projects/{pid_b}/knowledge")
b_ids2 = [x.get("id") for x in blist2] if s == 200 else []
check("project B now sees the global entry", k1_id in b_ids2, b_ids2)

# B 后续任务的新提示词应引用全局条目正文
new_tid = create_task(pid_b, "提升后新任务（引用全局规范）", "验证全局条目注入", exec_a["id"], acc_a["id"])
check("B post-promotion task created", bool(new_tid), new_tid)
wait_until(lambda: any_prompt_contains(pid_b, "executor", "execute", KBODY)
           and any_prompt_contains(pid_b, "acceptor", "accept", KBODY), 60)
check("B executor prompt now contains global knowledge body",
      any_prompt_contains(pid_b, "executor", "execute", KBODY))
check("B acceptor prompt now contains global knowledge body",
      any_prompt_contains(pid_b, "acceptor", "accept", KBODY))

# ======================================================================
# 6) 快照 + kill -9 重启恢复
# ======================================================================
print("[knowledge-smoke] 6) snapshot + kill -9 restart recovery")
s, snap_a = get(f"/projects/{pid_a}/knowledge")
s2, snap_b = get(f"/projects/{pid_b}/knowledge")
check("pre-restart snapshots captured", s == 200 and s2 == 200 and len(snap_a) >= 1,
      f"{s}/{s2} a={len(snap_a)}")

time.sleep(0.5)          # 让 WAL 落盘
kill9(SERVER)            # 硬重启（不走优雅退出）
SERVER = start_server()
if not wait_up():
    print("[knowledge-smoke] server did not come up after kill -9 restart")
    sys.exit(1)

s, after_a = get(f"/projects/{pid_a}/knowledge")
s2, after_b = get(f"/projects/{pid_b}/knowledge")
check("restart: project A entries identical field-by-field",
      s == 200 and after_a == snap_a, f"a={after_a} vs {snap_a}")
check("restart: project B entries identical field-by-field",
      s2 == 200 and after_b == snap_b, f"b={after_b} vs {snap_b}")

# 重启后 source_event_id 仍解析回原始指导事件
k1_after = next((x for x in after_a if x.get("id") == k1_id), None)
s3, evlist2 = get(f"/events?project_id={pid_a}&type=task.guidance_added")
src2 = next((e for e in evlist2 if e.get("id") == (k1_after or {}).get("source_event_id")), None) if s3 == 200 else None
check("restart: source_event_id still resolves to the original guidance event",
      k1_after is not None and src2 is not None
      and (src2.get("payload") or {}).get("text") == GUID
      and (src2.get("payload") or {}).get("guidance_id") == guid_id,
      src2)
check("restart: knowledge audit events survived",
      "knowledge.created" in etypes(pid_a) and "knowledge.promoted" in etypes(pid_a))

# ======================================================================
# 7) 前端入口（构建产物含知识库视图标记）
# ======================================================================
print("[knowledge-smoke] 7) web bundle markers")
bundle = ""
idx = os.path.join(DIST, "index.html")
if os.path.exists(idx):
    for fn in os.listdir(os.path.join(DIST, "assets")):
        if fn.endswith(".js"):
            with open(os.path.join(DIST, "assets", fn), encoding="utf-8", errors="ignore") as f:
                bundle += f.read()
check("web bundle contains knowledge view markers",
      "项目知识库" in bundle and "沉淀为规范" in bundle and "提升为全局" in bundle,
      "missing knowledge markers in dist")

# ======================================================================
# 8) 检查集一致性（可重复执行且检查集稳定）
# ======================================================================
EXPECTED_CHECKS = [
    "planner agent registered",
    "executor agent registered",
    "acceptor agent registered",
    "project A created",
    "seed task created in A",
    "guidance posted returns guidance id",
    "guidance_added event carries guidance_id",
    "guidance event text matches what was sent",
    "create rejected: empty body → 400",
    "create rejected: unknown source_event_id → 400",
    "K1 created with full fields",
    "K1 in project A list, field-by-field matches",
    "K1 detail GET field-by-field matches",
    "knowledge.viewed audit event emitted",
    "K2 created",
    "K2 PATCH changes body/applies_to and bumps updated_at",
    "K2 GET reflects the update",
    "K2 DELETE removes only K2 (K1 intact)",
    "knowledge.created audit x2 with payloads",
    "knowledge.updated audit reflects new fields",
    "knowledge.deleted audit snapshots K2",
    "project A planned by mock planner",
    "planner prompt contains knowledge body (A)",
    "planner prompt starts with knowledge section (A)",
    "project A confirmed (running)",
    "executor(plan_drafting) prompt contains knowledge body (A)",
    "executor(execute) prompt contains knowledge body (A)",
    "acceptor(plan_review) prompt contains knowledge body (A)",
    "acceptor(accept) prompt contains knowledge body (A)",
    "all assembled role prompts in A start with knowledge section",
    "K1.source_event_id resolves to the guidance event",
    "project B created",
    "project B planned by mock planner",
    "project B confirmed (running)",
    "project B list excludes A's project-scope entry",
    "B planner prompt does NOT contain A's knowledge body",
    "B executor prompt does NOT contain A's knowledge body",
    "B acceptor prompt does NOT contain A's knowledge body",
    "B prompts carry no knowledge section mark",
    "K1 promoted to global (scope=global)",
    "knowledge.promoted audit event emitted",
    "second promote is idempotent-conflict (409)",
    "project B now sees the global entry",
    "B post-promotion task created",
    "B executor prompt now contains global knowledge body",
    "B acceptor prompt now contains global knowledge body",
    "pre-restart snapshots captured",
    "restart: project A entries identical field-by-field",
    "restart: project B entries identical field-by-field",
    "restart: source_event_id still resolves to the original guidance event",
    "restart: knowledge audit events survived",
    "web bundle contains knowledge view markers",
]
check("check set matches EXPECTED_CHECKS (repeatable)",
      CHECK_NAMES == EXPECTED_CHECKS,
      f"executed={len(CHECK_NAMES)} expected={len(EXPECTED_CHECKS)} "
      f"diff={[n for n in CHECK_NAMES if n not in EXPECTED_CHECKS] + [n for n in EXPECTED_CHECKS if n not in CHECK_NAMES]}")

failed = [n for n, ok, _ in CHECKS if not ok]
print()
print(f"[knowledge-smoke] {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
if failed:
    print("[knowledge-smoke] FAILED:")
    for n in failed:
        print("  - " + n)
    sys.exit(1)
print("[knowledge-smoke] ALL PASS")
sys.exit(0)
PY
RC=$?

if [[ $RC -ne 0 ]]; then
  echo "[knowledge-smoke] server log tail:"
  tail -40 "$LOG" 2>/dev/null || true
fi
exit $RC
