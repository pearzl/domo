#!/usr/bin/env bash
# DOMO 真实 Agent 自迭代验收。
#
# 只有在设置 DOMO_RT_MODEL 时运行，例如：
#   DOMO_RT_MODEL=<provider/model> tests/test_real_agent.sh
#
# 场景：把仓库临时副本交给 DOMO，用真实 opencode Agent 完成一个最小的、
# 可客观验收的功能；验证规划、执行、验收、合并、报告与人工确认全链路。
# 不依赖固定的模型名，不访问除模型服务外的外部资源。

set -euo pipefail

if [[ -z "${DOMO_RT_MODEL:-}" ]]; then
  echo "跳过：未设置 DOMO_RT_MODEL（真实模型用例）"
  exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-real.$$"
PORT="${DOMO_REAL_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)}"
SERVER_PID=""

cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

mkdir -p "$TMP_ROOT/repo"
git -C "$TMP_ROOT/repo" init -q -b main
printf '# Real agent test\n' >"$TMP_ROOT/repo/README.md"
git -C "$TMP_ROOT/repo" add .
git -C "$TMP_ROOT/repo" -c user.name=test -c user.email=test@example.com \
  commit -q -m "init"

echo "==> 构建后端"
cargo build --manifest-path "$ROOT/server/Cargo.toml" >/dev/null

echo "==> 启动 DOMO（真实 Agent，模型 ${DOMO_RT_MODEL}）"
DOMO_DATA_ROOT="$TMP_ROOT/data" \
DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
DOMO_LOG_ROOT="$TMP_ROOT/log" \
DOMO_SERVER_PORT="$PORT" \
DOMO_JOB_TIMEOUT_SECS="${DOMO_JOB_TIMEOUT_SECS:-900}" \
"$ROOT/server/target/debug/domo-server" >"$TMP_ROOT/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 100); do
  if curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null; then
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "服务启动失败" >&2
    tail -80 "$TMP_ROOT/server.log" >&2
    exit 1
  fi
  sleep 0.1
done
curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null

python3 - "$PORT" "$TMP_ROOT/repo" "$DOMO_RT_MODEL" <<'PY'
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
repo = sys.argv[2]
model = sys.argv[3]


_pending_goals = {}


def call(method, path, data=None):
    if method == "POST" and path == "/api/projects" and isinstance(data, dict) and "goal" in data:
        goal = data.get("goal")
        payload = {key: value for key, value in data.items() if key != "goal"}
        result = _call_raw("POST", path, payload)
        project_id = result["project"]["id"]
        _pending_goals[project_id] = goal
        return result
    if (
        method == "POST"
        and path.startswith("/api/projects/")
        and path.endswith("/confirm")
        and not path.endswith("/analysis/confirm")
    ):
        project_id = path.split("/")[-2]
        detail = _call_raw("GET", f"/api/projects/{project_id}")
        if not detail["messages"] and project_id in _pending_goals:
            _call_raw(
                "POST",
                f"/api/projects/{project_id}/analysis/messages",
                {
                    "message": _pending_goals.pop(project_id),
                    "confirm_mode": "auto",
                },
            )
        message = (data or {}).get("message")
        if message:
            _call_raw(
                "POST",
                f"/api/projects/{project_id}/analysis/messages",
                {"message": message, "confirm_mode": "auto"},
            )
        deadline = time.time() + 30
        while time.time() < deadline:
            detail = _call_raw("GET", f"/api/projects/{project_id}")
            if detail["project"]["stage"] != "analysis":
                return detail
            if detail.get("proposal") and not detail["jobs"]:
                return _call_raw(
                    "POST",
                    f"/api/projects/{project_id}/analysis/confirm",
                    {},
                )
            last = detail["messages"][-1] if detail["messages"] else None
            if last and last["role"] == "planner" and not detail.get("proposal"):
                return detail
            time.sleep(0.1)
        return _call_raw("GET", f"/api/projects/{project_id}")
    return _call_raw(method, path, data)


def _call_raw(method, path, data=None):
    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(
        base + path,
        data=body,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"{method} {path} -> {error.code}: {error.read().decode()}") from error


for agent in call("GET", "/api/agents"):
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
mock = call(
    "POST",
    "/api/agents",
    {
        "name": "Real OpenCode",
        "kind": "opencode",
        "model": model,
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 2,
    },
)
# 新建项目的项目池继承默认团队。

project = call(
    "POST",
    "/api/projects",
    {
        "name": "Real agent",
        "goal": (
            "在仓库根目录创建 hello.txt，内容为一行 hello domo。"
            "不要修改其他文件。验收标准：hello.txt 存在且内容为 hello domo。"
        ),
        "repo_path": repo,
        "base_branch": "main",
    },
)["project"]
project_id = project["id"]
call("POST", f"/api/projects/{project_id}/confirm", {})

deadline = time.time() + 1200
detail = None
while time.time() < deadline:
    detail = call("GET", f"/api/projects/{project_id}")
    state = detail["project"]
    if state["stage"] == "analysis" and state["status"] == "blocked":
        call(
            "POST",
            f"/api/projects/{project_id}/confirm",
            {"message": "没有额外要求，请直接规划并执行。"},
        )
    if state["stage"] == "summarizing" and state["report"]:
        break
    if state["status"] == "blocked" and state["stage"] == "executing":
        raise AssertionError(
            f"项目阻塞：{state['blocked_reason']} "
            f"{[(task['key'], task['status'], task['last_error']) for task in detail['tasks']]}"
        )
    time.sleep(1)
else:
    raise AssertionError(f"等待项目完成超时：{detail['project']}")

call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
detail = call("GET", f"/api/projects/{project_id}")
assert detail["project"]["stage"] == "completed", detail["project"]
assert all(task["stage"] == "completed" for task in detail["tasks"]), detail["tasks"]
assert detail["project"]["report"], "缺少项目报告"

branch = detail["project"]["branch"]
content = subprocess.check_output(
    ["git", "-C", repo, "show", f"{branch}:hello.txt"], text=True
)
assert content.strip() == "hello domo", repr(content)
status = subprocess.check_output(
    ["git", "-C", repo, "status", "--porcelain"], text=True
)
assert not status.strip(), status

print("真实 Agent 自迭代测试通过")
print(f"  交付分支: {branch}")
print(f"  作业数量: {len(detail['jobs'])}")
PY

echo "==> 通过：$(basename "$0")"
