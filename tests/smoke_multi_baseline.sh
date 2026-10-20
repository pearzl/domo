#!/usr/bin/env bash
# ARCH §15 第 1 步（项目基线）—— 多仓端到端验收冒烟（无模型依赖）。
#
# 覆盖：
#   1. 多仓工程确认项目 A：逐仓创建/切换到 domo/iter-<A>，baseline 记录每仓分支；
#   2. A 运行中确认 B → 409，且 A 的仓库分支不被切换；
#   3. A 暂停后确认 B → 成功，各仓切到 domo/iter-<B>，baseline 同步更新；
#   4. 恢复已暂停项目 = 切换项目：脏工作区阻止并返回 dirty[{repo,files}]，项目保持暂停；
#      清理后恢复 B → 成功；再从 B 切换回 A → 各仓回到 domo/iter-<A>；
#   5. 某仓库只读时确认 C → 409 并列出失败仓库，其他仓库不切换、C 分支不创建；
#      确认 C 前工作区有脏文件时同样 409 dirty 且不切换；
#   6. 多仓任务验收通过后逐仓提交：有改动写一次提交、无改动跳过、提交失败进收件箱。
#
# 用法：bash tests/smoke_multi_baseline.sh
#       DOMO_BASELINE_SMOKE_PORT=xxxx bash tests/smoke_multi_baseline.sh
# 退出码 0 = 全部通过；结束清理自己启动的进程与临时目录。
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${DOMO_BASELINE_SMOKE_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()' 2>/dev/null || echo 8799)}"
RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/domo_baseline_smoke.XXXXXX")"
DB="$RUN_DIR/domo.db"
WS="$RUN_DIR/workspace"
LOG="$RUN_DIR/server.log"
mkdir -p "$WS"

cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  chmod -R u+rwX "$RUN_DIR" 2>/dev/null || true
  rm -rf "$RUN_DIR"
}
trap cleanup EXIT

echo "[baseline-smoke] building server…"
(cd "$ROOT/server" && cargo build --quiet) || { echo "[baseline-smoke] build failed"; exit 1; }

echo "[baseline-smoke] starting server on :$PORT (db $DB)"
(
  cd "$ROOT/server"
  DOMO_DB="$DB" DOMO_PORT="$PORT" DOMO_LOG="$LOG" DOMO_LOG_LEVEL=warn \
    DOMO_WORKSPACE_ROOT="$RUN_DIR/domo-ws" \
    exec "$ROOT/server/target/debug/domo-server"
) >"$RUN_DIR/stdout.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 80); do
  if curl -sf --max-time 1 "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1; then break; fi
  sleep 0.1
done
if ! curl -sf --max-time 2 "http://127.0.0.1:$PORT/api/agents" >/dev/null 2>&1; then
  echo "[baseline-smoke] server did not come up; log:"; tail -30 "$RUN_DIR/stdout.log"
  exit 1
fi

python3 - "$PORT" "$WS" <<'PY'
import json, os, subprocess, sys, time, urllib.error, urllib.request

PORT, WS = sys.argv[1], sys.argv[2]
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


def branch(dir):
    return git(dir, "rev-parse", "--abbrev-ref", "HEAD")


def wait_task(pid, tid, wanted, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        s, r = req("GET", f"/projects/{pid}")
        t = next((x for x in (r or {}).get("tasks", []) if x.get("id") == tid), None)
        if t and t.get("status") in wanted:
            return t
        time.sleep(0.2)
    return None


def events(pid):
    s, r = req("GET", f"/events?project_id={pid}")
    return r if s == 200 else []


def task_events(pid, tid, kind):
    return [e for e in events(pid) if e.get("task_id") == tid and e.get("type") == kind]


def make_ready():
    for repo in ("api", "web"):
        d = os.path.join(WS, repo)
        os.makedirs(d, exist_ok=True)
        git(d, "init", "-q")
        with open(os.path.join(d, "README.md"), "w") as f:
            f.write(repo + "\n")
        git(d, "add", "-A")
        git(d, "-c", "user.name=domo", "-c", "user.email=domo@example.invalid", "commit", "-q", "-m", "init")


def chmod_read_only(path):
    for root, dirs, files in os.walk(path):
        for f in files:
            os.chmod(os.path.join(root, f), 0o444)
        for d in dirs:
            os.chmod(os.path.join(root, d), 0o555)
    os.chmod(path, 0o555)


def chmod_writable(path):
    for root, dirs, files in os.walk(path):
        os.chmod(root, 0o755)
        for f in files:
            os.chmod(os.path.join(root, f), 0o644)


make_ready()

s, e = req("POST", "/engineerings", {"name": "多仓基线冒烟", "workspace_kind": "multi", "workspace_path": WS})
check("create multi engineering", s == 200, f"{s} {e}")
eid = e.get("id", "")

s, a = req("POST", "/projects", {"name": "A", "brief": "first", "engineering_id": eid})
check("create project A", s == 200 and a.get("id"), f"{s} {a}")
pa = a.get("id", "")
s, b = req("POST", "/projects", {"name": "B", "brief": "second", "engineering_id": eid})
check("create project B", s == 200 and b.get("id"), f"{s} {b}")
pb = b.get("id", "")

ba = "domo/iter-" + pa.rsplit("_", 1)[-1]
bb = "domo/iter-" + pb.rsplit("_", 1)[-1]

s, r = req("POST", f"/projects/{pa}/confirm", {})
check("confirm A", s == 200, f"{s} {r}")
check("A switches api branch", branch(os.path.join(WS, "api")) == ba, branch(os.path.join(WS, "api")))
check("A switches web branch", branch(os.path.join(WS, "web")) == ba, branch(os.path.join(WS, "web")))
s, r = req("GET", f"/projects/{pa}")
base = (((r or {}).get("project") or {}).get("baseline") or {}).get("repos") or {}
check("A baseline records each repo", base.get("api") == ba and base.get("web") == ba, base)

s, r = req("POST", f"/projects/{pb}/confirm", {})
check("B confirm is 409 while A is running", s == 409, f"{s} {r}")
check("409 explains multi-repo serial rule", "多仓工程同一时刻只能运行一个项目" in json.dumps(r, ensure_ascii=False), r)
check("rejected B does not switch A repos", branch(os.path.join(WS, "api")) == ba, branch(os.path.join(WS, "api")))

s, r = req("POST", f"/projects/{pa}/pause", {})
check("pause A", s == 200, f"{s} {r}")
s, r = req("POST", f"/projects/{pb}/confirm", {})
check("confirm B after A paused", s == 200, f"{s} {r}")
check("B switches api branch", branch(os.path.join(WS, "api")) == bb, branch(os.path.join(WS, "api")))
check("B switches web branch", branch(os.path.join(WS, "web")) == bb, branch(os.path.join(WS, "web")))
s, r = req("GET", f"/projects/{pb}")
base = (((r or {}).get("project") or {}).get("baseline") or {}).get("repos") or {}
check("B baseline records each repo", base.get("api") == bb and base.get("web") == bb, base)

s, r = req("POST", f"/projects/{pb}/pause", {})
check("pause B", s == 200, f"{s} {r}")

# 恢复已暂停项目 = 切换项目。脏工作区必须阻止，且项目保持 paused。
dirty = os.path.join(WS, "web", "dirty.txt")
with open(dirty, "w") as f:
    f.write("dirty\n")
s, r = req("POST", f"/projects/{pb}/resume", {})
check("resume B is 409 with dirty worktree", s == 409, f"{s} {r}")
dirty_repos = [x.get("repo") for x in (r or {}).get("dirty", [])]
check("409 dirty lists web", dirty_repos == ["web"], r)
check("dirty files include dirty.txt", any("dirty.txt" in x for x in ((r or {}).get("dirty") or [{}])[0].get("files", [])), r)
check("dirty resume leaves B branches unchanged", branch(os.path.join(WS, "api")) == bb, branch(os.path.join(WS, "api")))
s, r = req("GET", f"/projects/{pb}")
check("dirty resume keeps B paused", ((r or {}).get("project") or {}).get("status") == "paused", r)
os.remove(dirty)

s, r = req("POST", f"/projects/{pb}/resume", {})
check("resume B after cleaning dirty files", s == 200, f"{s} {r}")
check("B resume keeps B branches", branch(os.path.join(WS, "api")) == bb, branch(os.path.join(WS, "api")))

# 暂停 B 后恢复 A：逐仓切回 A 的迭代分支。
s, r = req("POST", f"/projects/{pb}/pause", {})
check("pause B again", s == 200, f"{s} {r}")
s, r = req("POST", f"/projects/{pa}/resume", {})
check("switch back to A by resuming it", s == 200, f"{s} {r}")
check("A switch restores api branch", branch(os.path.join(WS, "api")) == ba, branch(os.path.join(WS, "api")))
check("A switch restores web branch", branch(os.path.join(WS, "web")) == ba, branch(os.path.join(WS, "web")))

# 多仓任务验收通过后：在项目分支逐仓提交，不建 worktree、不做跨仓合并。
s, ex = req("POST", "/agents", {"name": "baseline-exec", "adapter": "mock", "roles": ["executor"],
                                "capabilities": {"max_parallel": 2}, "config": {"duration_ms": 50}})
s2, acc = req("POST", "/agents", {"name": "baseline-acc", "adapter": "mock", "roles": ["acceptor"],
                                  "capabilities": {"max_parallel": 2}, "config": {"duration_ms": 50}})
check("create mock exec/acceptor", s == 200 and s2 == 200, (ex, acc))

def new_task(title):
    s, t = req("POST", f"/projects/{pa}/tasks", {
        "title": title, "description": "多仓任务提交冒烟", "depends_on": [],
        "executor_agent_id": ex.get("id"), "acceptor_agent_id": acc.get("id"),
        "acceptance_required": True, "acceptance_criteria": "产物可验收",
    })
    return s, t

s, t1 = new_task("提交改动")
check("create multi task 1", s == 200 and t1.get("id"), f"{s} {t1}")
with open(os.path.join(WS, "api", "api-change.txt"), "w") as f:
    f.write("api change\n")
with open(os.path.join(WS, "web", "web-change.txt"), "w") as f:
    f.write("web change\n")
t1 = wait_task(pa, t1.get("id", ""), {"done", "escalated", "suspended"}, timeout=60)
check("multi task 1 reaches done", (t1 or {}).get("status") == "done", t1)
subject_api = git(os.path.join(WS, "api"), "log", "-1", "--format=%s")
subject_web = git(os.path.join(WS, "web"), "log", "-1", "--format=%s")
check("api project branch committed once", "domo: task " in subject_api, subject_api)
check("web project branch committed once", "domo: task " in subject_web, subject_web)
check("task commit leaves both repos clean", not git(os.path.join(WS, "api"), "status", "--porcelain")
      and not git(os.path.join(WS, "web"), "status", "--porcelain"))
check("task branches still A", branch(os.path.join(WS, "api")) == ba and branch(os.path.join(WS, "web")) == ba)
commit_ev = task_events(pa, t1.get("id", ""), "task.commit_completed")
check("task.commit_completed recorded with 2 commits", commit_ev and len(commit_ev[-1].get("payload", {}).get("commits", [])) == 2, commit_ev)
check("no worktree merge event for multi task", not task_events(pa, t1.get("id", ""), "task.merge_completed"))
s, all_inbox = req("GET", "/inbox")
check("multi task does not raise sandbox violation", not any(
    (i.get("payload") or {}).get("reason") == "sandbox_violation"
    and (i.get("payload") or {}).get("task_id") == t1.get("id") for i in (all_inbox or [])))

s, t2 = new_task("无改动")
check("create multi task 2", s == 200 and t2.get("id"), f"{s} {t2}")
t2 = wait_task(pa, t2.get("id", ""), {"done", "escalated", "suspended"}, timeout=60)
check("multi task 2 without changes reaches done", (t2 or {}).get("status") == "done", t2)
skip_ev = task_events(pa, t2.get("id", ""), "task.commit_skipped")
check("task.commit_skipped recorded", skip_ev and set(skip_ev[-1].get("payload", {}).get("repos", [])) == {"api", "web"}, skip_ev)
check("no empty commit created", git(os.path.join(WS, "api"), "log", "-1", "--format=%s") == subject_api)

# 提交失败 → task.commit_failed + 收件箱，任务不静默 done。
hook_dir = os.path.join(WS, "web", ".git", "hooks")
os.makedirs(hook_dir, exist_ok=True)
hook = os.path.join(hook_dir, "pre-commit")
with open(hook, "w") as f:
    f.write("#!/bin/sh\nexit 1\n")
os.chmod(hook, 0o755)
s, t3 = new_task("提交失败")
check("create multi task 3", s == 200 and t3.get("id"), f"{s} {t3}")
with open(os.path.join(WS, "web", "fail.txt"), "w") as f:
    f.write("fail\n")
t3 = wait_task(pa, t3.get("id", ""), {"done", "escalated", "suspended"}, timeout=60)
check("commit failure escalates task", (t3 or {}).get("status") == "escalated", t3)
fail_ev = task_events(pa, t3.get("id", ""), "task.commit_failed")
check("task.commit_failed recorded", fail_ev and fail_ev[-1].get("payload", {}).get("failures"), fail_ev)
s, inbox = req("GET", "/inbox")
inbox_ok = any(i.get("status") == "open" and i.get("type") == "escalation"
               and (i.get("payload") or {}).get("reason") == "multi_commit_failed"
               and (i.get("payload") or {}).get("task_id") == t3.get("id") for i in (inbox or []))
check("commit failure lands in inbox", inbox_ok, inbox)
os.remove(hook)
os.remove(os.path.join(WS, "web", "fail.txt"))
git(os.path.join(WS, "web"), "reset", "-q")
check("failed repo cleaned for later checks", not git(os.path.join(WS, "web"), "status", "--porcelain"))

# 人工验收路径：项目暂停时验收不被自动派发，人接管后同样在该项目分支提交。
s, slow = req("POST", "/agents", {"name": "baseline-slow", "adapter": "mock", "roles": ["executor"],
                                 "capabilities": {"max_parallel": 1}, "config": {"duration_ms": 1500}})
check("create slow mock executor", s == 200, slow)
s, t4 = req("POST", f"/projects/{pa}/tasks", {
    "title": "人工验收提交", "description": "暂停后人工验收", "depends_on": [],
    "executor_agent_id": slow.get("id"), "acceptor_agent_id": acc.get("id"),
    "acceptance_required": True, "acceptance_criteria": "人工验收",
})
check("create multi task 4", s == 200 and t4.get("id"), f"{s} {t4}")
with open(os.path.join(WS, "web", "human-change.txt"), "w") as f:
    f.write("human change\n")
t4 = wait_task(pa, t4.get("id", ""), {"executing"}, timeout=30)
check("task 4 executing before pause", (t4 or {}).get("status") == "executing", t4)
s, r = req("POST", f"/projects/{pa}/pause", {})
check("pause A during task 4", s == 200, f"{s} {r}")
t4 = wait_task(pa, t4.get("id", ""), {"awaiting_acceptance"}, timeout=30)
check("task 4 waits for human acceptance while paused", (t4 or {}).get("status") == "awaiting_acceptance", t4)
s, r = req("GET", f"/projects/{pa}")
check("project A paused before human acceptance", (r or {}).get("project", {}).get("status") == "paused", r)
s, r = req("POST", f"/tasks/{t4.get('id')}/accept", {"comment": "人工验收通过"})
check("human accept on multi task", s == 200 and (r or {}).get("status") == "done", f"{s} {r}")
check("human accept commits web branch",
      "domo: task " in git(os.path.join(WS, "web"), "log", "-1", "--format=%s"),
      git(os.path.join(WS, "web"), "log", "-1", "--format=%s"))
check("human accept commit event", bool(task_events(pa, t4.get("id", ""), "task.commit_completed")))
check("A remains paused before C", req("GET", f"/projects/{pa}")[1].get("project", {}).get("status") == "paused")

s, c = req("POST", "/projects", {"name": "C", "brief": "third", "engineering_id": eid})
check("create project C", s == 200 and c.get("id"), f"{s} {c}")
pc = c.get("id", "")
bc = "domo/iter-" + pc.rsplit("_", 1)[-1]

# 首次确认 C 也走同一套干净检查。
with open(dirty, "w") as f:
    f.write("dirty\n")
s, r = req("POST", f"/projects/{pc}/confirm", {})
check("C confirm is 409 with dirty worktree", s == 409, f"{s} {r}")
check("confirm dirty lists web", "web" in [x.get("repo") for x in (r or {}).get("dirty", [])], r)
check("dirty confirm leaves api on A branch", branch(os.path.join(WS, "api")) == ba, branch(os.path.join(WS, "api")))
check("dirty confirm creates no C branch", subprocess.run(
    ["git", "-C", os.path.join(WS, "api"), "rev-parse", "--verify", "--quiet", f"refs/heads/{bc}"],
    capture_output=True).returncode != 0)
os.remove(dirty)

chmod_read_only(os.path.join(WS, "web"))
try:
    s, r = req("POST", f"/projects/{pc}/confirm", {})
finally:
    chmod_writable(os.path.join(WS, "web"))
check("C confirm is 409 on read-only repo", s == 409, f"{s} {r}")
failed = [x.get("repo") for x in (r or {}).get("repos", [])]
check("409 lists the failed repo", "web" in failed, r)
check("failed C leaves api on A branch", branch(os.path.join(WS, "api")) == ba, branch(os.path.join(WS, "api")))
check("failed C creates no C branch", subprocess.run(
    ["git", "-C", os.path.join(WS, "api"), "rev-parse", "--verify", "--quiet", f"refs/heads/{bc}"],
    capture_output=True).returncode != 0)

failed_checks = [n for n, ok in CHECKS if not ok]
print(f"\n[baseline-smoke] {len(CHECKS) - len(failed_checks)}/{len(CHECKS)} checks passed")
sys.exit(1 if failed_checks else 0)
PY
STATUS=$?
exit "$STATUS"
