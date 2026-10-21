#!/usr/bin/env bash
# 方案 B（共享目录）—— 自动识别 + 串行 + 不做 git 分支/提交 冒烟（无模型依赖）。
#
# 覆盖：
#   1. 工程类型自动识别：git 仓库根 → single；其余目录 → plain（共享目录）；
#   2. 共享目录工程：多仓项目直接在目录内工作，不创建/切换 domo/iter-* 分支；
#   3. 项目级串行：A 运行时确认 B → 409；A 暂停后可确认 B；
#   4. 任务级串行：同项目两个任务不会同时执行；
#   5. 验收通过不自动提交：用户未提交改动保持原样，HEAD 不变；
#   6. 脏工作区不再阻止恢复/确认（方案 B 把 git 生命周期交还用户）。
#
# 用法：bash tests/smoke_shared_dir.sh
#       DOMO_SHARED_SMOKE_PORT=xxxx bash tests/smoke_shared_dir.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_SHARED_SMOKE_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()' 2>/dev/null || echo 8795)}"
RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/domo_shared_smoke.XXXXXX")"
DB="$RUN_DIR/domo.db"
WS="$RUN_DIR/workspace"
LOG="$RUN_DIR/server.log"
mkdir -p "$WS"

cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ "${DOMO_KEEP:-0}" == "1" ]]; then
    echo "[shared-smoke] kept run dir: $RUN_DIR"
  else
    rm -rf "$RUN_DIR"
  fi
}
trap cleanup EXIT

echo "[shared-smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[shared-smoke] build failed"; exit 1; }

echo "[shared-smoke] starting server on :$PORT (db $DB)"
(
  cd "$ROOT/server"
  DOMO_DB="$DB" DOMO_PORT="$PORT" DOMO_LOG="$LOG" DOMO_LOG_LEVEL="${DOMO_LOG_LEVEL:-warn}" \
    DOMO_WORKSPACE_ROOT="$RUN_DIR/domo-ws" \
    exec "$ROOT/server/target/debug/domo-server"
) >"$RUN_DIR/stdout.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 100); do
  curl -sf --max-time 1 "http://127.0.0.1:$PORT/api/projects" >/dev/null 2>&1 && break
  sleep 0.1
done
if ! curl -sf --max-time 2 "http://127.0.0.1:$PORT/api/projects" >/dev/null 2>&1; then
  echo "[shared-smoke] server did not come up; log:"; tail -30 "$RUN_DIR/stdout.log"
  exit 1
fi

python3 - "$PORT" "$WS" "$RUN_DIR/domo-ws" <<'PY'
import json, os, subprocess, sys, time, urllib.error, urllib.request

PORT, WS, WS_ROOT = sys.argv[1], sys.argv[2], sys.argv[3]
BASE = f"http://127.0.0.1:{PORT}/api"
CHECKS = []


def check(name, cond, detail=""):
    cond = bool(cond)
    CHECKS.append((name, cond))
    print(("  PASS  " if cond else "  FAIL  ") + name + (("  -- " + str(detail)) if detail and not cond else ""), flush=True)


def req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(BASE + path, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=20) as resp:
            return resp.status, json.load(resp)
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.load(e)
        except Exception:
            return e.code, {}


def git(dir, *args):
    return subprocess.run(["git", "-C", dir, *args], check=True, capture_output=True, text=True).stdout.strip()


def init_repo(dir):
    os.makedirs(dir, exist_ok=True)
    git(dir, "init", "-q")
    with open(os.path.join(dir, "README.md"), "w") as f:
        f.write("init\n")
    git(dir, "add", "-A")
    git(dir, "-c", "user.name=domo", "-c", "user.email=domo@example.invalid", "commit", "-q", "-m", "init")


def wait_until(fn, timeout=40):
    end = time.time() + timeout
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(0.1)
    return None


def project(pid):
    s, r = req("GET", f"/projects/{pid}")
    return (r or {}).get("project") or {}


def tasks(pid):
    s, r = req("GET", f"/projects/{pid}")
    return (r or {}).get("tasks") or []


def events(pid):
    s, r = req("GET", f"/events?project_id={pid}")
    return r if s == 200 else []


API = os.path.join(WS, "api")
WEB = os.path.join(WS, "web")
init_repo(API)
init_repo(WEB)
api_head = git(API, "rev-parse", "HEAD")
web_head = git(WEB, "rev-parse", "HEAD")

# 1) 自动识别：仓库根 → single；多仓父目录 → plain
s, single = req("POST", "/engineerings", {"name": "单仓", "workspace_path": API})
check("repo root detected as single", s == 200 and single.get("workspace_kind") == "single", single)
s, shared = req("POST", "/engineerings", {"name": "共享目录", "workspace_path": WS})
check("parent directory detected as plain", s == 200 and shared.get("workspace_kind") == "plain", shared)
sid, eid = single.get("id", ""), shared.get("id", "")

# 2) 共享目录项目 A/B + 项目级串行
s, a = req("POST", "/projects", {"name": "A", "brief": "shared A", "engineering_id": eid})
s2, b = req("POST", "/projects", {"name": "B", "brief": "shared B", "engineering_id": eid})
check("create shared projects A/B", s == 200 and s2 == 200, (a, b))
pa, pb = a.get("id", ""), b.get("id", "")
s, _ = req("POST", f"/projects/{pa}/confirm", {})
check("confirm A (shared)", s == 200, s)
# P2 完成后钩子：项目完成时在工程目录执行一次
s, _ = req("PATCH", f"/projects/{pa}", {"hooks": {"after_complete": "echo hook-ok > hook-ran.txt"}})
check("set project completion hook", s == 200, s)
check("no project branches created", not git(API, "branch", "--list", "domo/iter-*")
      and not git(WEB, "branch", "--list", "domo/iter-*"))
s, r = req("POST", f"/projects/{pb}/confirm", {})
check("B confirm is 409 while A running", s == 409, f"{s} {r}")

# 3) 任务级串行 + 不自动提交
s, ex = req("POST", "/agents", {"name": "shared-exec", "adapter": "mock", "roles": ["executor"],
                                "capabilities": {"max_parallel": 2}, "config": {"duration_ms": 1200}})
s2, acc = req("POST", "/agents", {"name": "shared-acc", "adapter": "mock", "roles": ["acceptor"],
                                  "capabilities": {"max_parallel": 2}, "config": {"duration_ms": 50}})
check("create mock agents", s == 200 and s2 == 200, (ex, acc))

def new_task(title):
    s, t = req("POST", f"/projects/{pa}/tasks", {
        "title": title, "description": "shared dir serial", "depends_on": [],
        "executor_agent_id": ex.get("id"), "acceptor_agent_id": acc.get("id"),
        "acceptance_required": True, "acceptance_criteria": "可验收",
    })
    return s, t

# 用户未提交改动：方案 B 不应自动提交，也不应阻止任务。
with open(os.path.join(API, "user-change.txt"), "w") as f:
    f.write("user\n")
with open(os.path.join(WEB, "user-change.txt"), "w") as f:
    f.write("user\n")

s1, t1 = new_task("串行一")
s2, t2 = new_task("串行二")
check("create two shared tasks", s1 == 200 and s2 == 200, (t1, t2))
seen_both = False
end = time.time() + 60
while time.time() < end:
    ts = {t["id"]: t["status"] for t in tasks(pa)}
    if ts.get(t1.get("id")) == "executing" and ts.get(t2.get("id")) == "executing":
        seen_both = True
    if all(ts.get(x) in {"done", "escalated", "suspended", "cancelled"} for x in (t1.get("id"), t2.get("id"))):
        break
    time.sleep(0.1)
check("two tasks never execute at the same time", not seen_both)
ts = {t["id"]: t["status"] for t in tasks(pa)}
check("both shared tasks completed", ts.get(t1.get("id")) == "done" and ts.get(t2.get("id")) == "done", ts)
hook_ev = wait_until(lambda: next((e for e in events(pa) if e.get("type") == "project.hook_completed"), None), 30)
check("project completion hook ran", hook_ev is not None, hook_ev)
check("project completion hook produced file", os.path.exists(os.path.join(WS, "hook-ran.txt")))
check("uncommitted user changes are preserved", "user-change.txt" in git(API, "status", "--porcelain")
      and "user-change.txt" in git(WEB, "status", "--porcelain"))
check("no auto commit on api", git(API, "rev-parse", "HEAD") == api_head)
check("no auto commit on web", git(WEB, "rev-parse", "HEAD") == web_head)
check("shared tasks create no worktree", not os.path.exists(os.path.join(WS_ROOT, pa, "wt")))

# 4) A 已全部完成；B 可直接确认（A 不再运行）。脏工作区不阻止暂停/恢复。
s, r = req("POST", f"/projects/{pb}/confirm", {})
check("confirm B after A completed", s == 200, f"{s} {r}")
check("B still has no project branch", not git(API, "branch", "--list", "domo/iter-*"))
s, _ = req("POST", f"/projects/{pb}/pause", {})
check("pause B", s == 200, s)
s, r = req("POST", f"/projects/{pb}/resume", {})
check("resume B works with dirty shared directory", s == 200, f"{s} {r}")

# 5) 单仓项目仍走 worktree（不污染工程仓库）
s, sp = req("POST", "/projects", {"name": "S", "brief": "single", "engineering_id": sid})
spid = sp.get("id", "")
s, _ = req("POST", f"/projects/{spid}/confirm", {})
check("confirm single project", s == 200, s)
s, st = req("POST", f"/projects/{spid}/tasks", {
    "title": "worktree任务", "description": "single worktree", "depends_on": [],
    "executor_agent_id": ex.get("id"), "acceptor_agent_id": acc.get("id"),
    "acceptance_required": True, "acceptance_criteria": "可验收",
})
stid = st.get("id", "")
st = wait_until(lambda: next((t for t in tasks(spid) if t["id"] == stid and t["status"] in {"done", "escalated"}), None), 60)
check("single task done", (st or {}).get("status") == "done", st)
sev = [e for e in events(spid) if e.get("type") == "task.sandbox_ready"]
cwd = (sev[-1].get("payload") or {}).get("cwd", "") if sev else ""
check("single task uses worktree cwd", WS_ROOT in cwd and API not in cwd, cwd)
check("single repo HEAD unchanged by worktree", git(API, "rev-parse", "HEAD") == api_head)

failed = [n for n, ok in CHECKS if not ok]
print(f"\n[shared-smoke] {len(CHECKS) - len(failed)}/{len(CHECKS)} checks passed")
sys.exit(1 if failed else 0)
PY
