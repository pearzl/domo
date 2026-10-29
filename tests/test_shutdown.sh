#!/usr/bin/env bash
# DOMO 关闭流程测试。
#
# 场景：浏览器保持 SSE 长连接、执行者 Agent 子进程正在运行，向前端服务发送 SIGINT。
# 验收标准：
#   1. 服务在关闭宽限期内自行退出，不需要第二次信号或 kill -9；
#   2. SSE 长连接随服务关闭而结束；
#   3. 运行中的 Agent 子进程被终止，不留下孤儿进程。
#
# 使用内置 mock Agent 与本地 sleep 进程，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-shutdown.$$"
PORT="${DOMO_SHUTDOWN_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)}"
SERVER_PID=""
SSE_PID=""
AGENT_PID=""

cleanup() {
  if [[ -n "$SSE_PID" ]] && kill -0 "$SSE_PID" 2>/dev/null; then
    kill "$SSE_PID" 2>/dev/null || true
  fi
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill -9 "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ -n "$AGENT_PID" ]]; then
    kill -9 "$AGENT_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

fail() {
  echo "失败：$*" >&2
  echo "--- 服务日志 ---" >&2
  tail -80 "$TMP_ROOT/server.log" >&2 2>/dev/null || true
  exit 1
}

mkdir -p "$TMP_ROOT/repo"
git -C "$TMP_ROOT/repo" init -q -b main
git -C "$TMP_ROOT/repo" -c user.name=test -c user.email=test@example.com \
  commit --allow-empty -q -m "init"

echo "==> 构建后端"
cargo build --manifest-path "$ROOT/server/Cargo.toml" >/dev/null

echo "==> 启动服务"
DOMO_DATA_ROOT="$TMP_ROOT/data" \
DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
DOMO_LOG_ROOT="$TMP_ROOT/log" \
DOMO_SERVER_PORT="$PORT" \
"$ROOT/server/target/debug/domo-server" >>"$TMP_ROOT/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 100); do
  if curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null; then
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    fail "服务启动失败"
  fi
  sleep 0.1
done

echo "==> 准备一个执行中的 CLI Agent 作业"
AGENT_PID="$(python3 - "$PORT" "$TMP_ROOT/repo" <<'PY'
import json
import os
import sys
import time
import urllib.error
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
repo = sys.argv[2]

_pending_goals = {}


def call(method, path, data=None):
    if method == "POST" and path == "/api/projects" and isinstance(data, dict) and "goal" in data:
        goal = data.get("goal")
        payload = {key: value for key, value in data.items() if key != "goal"}
        result = _call_raw("POST", path, payload)
        _pending_goals[result["project"]["id"]] = goal
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
                {"message": _pending_goals.pop(project_id), "confirm_mode": "auto"},
            )
        deadline = time.time() + 30
        while time.time() < deadline:
            detail = _call_raw("GET", f"/api/projects/{project_id}")
            if detail["project"]["stage"] != "analysis":
                return detail
            if detail.get("proposal") and not detail["jobs"]:
                return _call_raw(
                    "POST", f"/api/projects/{project_id}/analysis/confirm", {}
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

planner = call("POST", "/api/agents", {
    "name": "Shutdown Planner", "kind": "mock",
    "roles": ["planner"], "max_concurrency": 1,
})
verifier = call("POST", "/api/agents", {
    "name": "Shutdown Verifier", "kind": "mock",
    "roles": ["verifier"], "max_concurrency": 1,
})
executor = call("POST", "/api/agents", {
    "name": "Shutdown Executor", "kind": "cli",
    "roles": ["executor"], "max_concurrency": 1,
    "command": ["sleep", "300"],
})
for role, agent in (("planner", planner), ("executor", executor), ("verifier", verifier)):
    call("POST", "/api/team/default", {"role": role, "agent_id": agent["id"]})

project = call("POST", "/api/projects", {
    "name": "Shutdown", "goal": "验证关闭流程",
    "repo_path": repo, "base_branch": "main",
})["project"]
call("POST", f"/api/projects/{project['id']}/confirm", {})

deadline = time.time() + 60
detail = None
while time.time() < deadline:
    detail = call("GET", f"/api/projects/{project['id']}")
    for job in detail["jobs"]:
        if job["role"] != "executor" or job["status"] != "running":
            continue
        pid_file = os.path.join(detail["project"]["workspace"], "jobs", job["id"], "pid")
        if os.path.exists(pid_file):
            with open(pid_file) as handle:
                print(handle.read().split(":")[0])
            raise SystemExit(0)
    time.sleep(0.1)
raise SystemExit(
    f"等待执行中作业超时：{detail['project']} "
    f"{[(t['key'], t['stage'], t['status']) for t in detail['tasks']]} "
    f"{[(j['role'], j['status'], j.get('error')) for j in detail['jobs']]}"
)
PY
)" || fail "未取得运行中 Agent 的 pid"

echo "==> 建立 SSE 长连接"
curl -sN -D "$TMP_ROOT/sse.headers" "http://127.0.0.1:$PORT/api/events/stream" \
  >"$TMP_ROOT/sse.log" 2>/dev/null &
SSE_PID=$!
for _ in $(seq 1 50); do
  if grep -q " 200" "$TMP_ROOT/sse.headers" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
grep -q " 200" "$TMP_ROOT/sse.headers" 2>/dev/null || fail "SSE 连接未建立"

echo "==> 发送 SIGINT"
START_MS="$(python3 -c 'import time; print(int(time.time() * 1000))')"
kill -INT "$SERVER_PID"
EXITED=0
for _ in $(seq 1 150); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    EXITED=1
    break
  fi
  sleep 0.1
done
ELAPSED_MS=$(( $(python3 -c 'import time; print(int(time.time() * 1000))') - START_MS ))
[[ "$EXITED" == "1" ]] || fail "服务在 15 秒内未退出"
(( ELAPSED_MS < 10000 )) || fail "服务退出耗时 ${ELAPSED_MS}ms，超过关闭宽限期"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""

echo "==> 校验 SSE 连接与 Agent 子进程"
for _ in $(seq 1 50); do
  if ! kill -0 "$SSE_PID" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
kill -0 "$SSE_PID" 2>/dev/null && fail "SSE 连接未随关闭结束"
SSE_PID=""

for _ in $(seq 1 50); do
  STATE="$(ps -o stat= -p "$AGENT_PID" 2>/dev/null || true)"
  if [[ -z "$STATE" || "$STATE" == Z* ]]; then
    break
  fi
  sleep 0.1
done
STATE="$(ps -o stat= -p "$AGENT_PID" 2>/dev/null || true)"
[[ -z "$STATE" || "$STATE" == Z* ]] || fail "Agent 子进程 $AGENT_PID 仍在运行（$STATE）"
AGENT_PID=""

grep -q "DOMO 已关闭" "$TMP_ROOT/server.log" || fail "缺少正常关闭日志"
grep -q "关闭超时" "$TMP_ROOT/server.log" && fail "关闭走了超时强制退出路径"

echo "==> 通过：$(basename "$0")"
