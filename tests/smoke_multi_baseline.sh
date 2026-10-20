#!/usr/bin/env bash
# ARCH §15 第 1 步（项目基线）—— 多仓端到端验收冒烟（无模型依赖）。
#
# 覆盖：
#   1. 多仓工程确认项目 A：逐仓创建/切换到 domo/iter-<A>，baseline 记录每仓分支；
#   2. A 运行中确认 B → 409，且 A 的仓库分支不被切换；
#   3. A 暂停后确认 B → 成功，各仓切到 domo/iter-<B>，baseline 同步更新；
#   4. 某仓库只读时确认 C → 409 并列出失败仓库，其他仓库不切换、C 分支不创建。
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
import json, os, subprocess, sys, urllib.error, urllib.request

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
s, c = req("POST", "/projects", {"name": "C", "brief": "third", "engineering_id": eid})
check("create project C", s == 200 and c.get("id"), f"{s} {c}")
pc = c.get("id", "")
bc = "domo/iter-" + pc.rsplit("_", 1)[-1]
chmod_read_only(os.path.join(WS, "web"))
try:
    s, r = req("POST", f"/projects/{pc}/confirm", {})
finally:
    chmod_writable(os.path.join(WS, "web"))
check("C confirm is 409 on read-only repo", s == 409, f"{s} {r}")
failed = [x.get("repo") for x in (r or {}).get("repos", [])]
check("409 lists the failed repo", "web" in failed, r)
check("failed C leaves api on B branch", branch(os.path.join(WS, "api")) == bb, branch(os.path.join(WS, "api")))
check("failed C creates no C branch", subprocess.run(
    ["git", "-C", os.path.join(WS, "api"), "rev-parse", "--verify", "--quiet", f"refs/heads/{bc}"],
    capture_output=True).returncode != 0)

failed_checks = [n for n, ok in CHECKS if not ok]
print(f"\n[baseline-smoke] {len(CHECKS) - len(failed_checks)}/{len(CHECKS)} checks passed")
sys.exit(1 if failed_checks else 0)
PY
STATUS=$?
exit "$STATUS"
