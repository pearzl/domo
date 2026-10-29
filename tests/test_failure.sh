#!/usr/bin/env bash
# DOMO 失败与超时恢复测试。
#
# 场景：
#   1. 执行者连续超时，任务在 3 次失败后阻塞，项目进入人工处理状态；
#   2. 重启服务并降低执行延迟后，人工重试项目；
#   3. 项目继续完成并产出报告。
#
# 使用内置 mock Agent，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-failure.$$"
PORT="${DOMO_FAILURE_PORT:-$(python3 - <<'PY'
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
git -C "$TMP_ROOT/repo" -c user.name=test -c user.email=test@example.com \
  commit --allow-empty -q -m "init"

echo "==> 构建后端"
cargo build --manifest-path "$ROOT/server/Cargo.toml" >/dev/null

start_server() {
  local executor_delay="$1"
  DOMO_DATA_ROOT="$TMP_ROOT/data" \
  DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
  DOMO_LOG_ROOT="$TMP_ROOT/log" \
  DOMO_SERVER_PORT="$PORT" \
  DOMO_JOB_TIMEOUT_SECS=1 \
  DOMO_MOCK_DELAY_MS=50 \
  DOMO_MOCK_EXECUTOR_DELAY_MS="$executor_delay" \
  "$ROOT/server/target/debug/domo-server" >>"$TMP_ROOT/server.log" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 100); do
    if curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null; then
      return 0
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "服务启动失败" >&2
      tail -80 "$TMP_ROOT/server.log" >&2
      exit 1
    fi
    sleep 0.1
  done
  echo "等待服务启动超时" >&2
  exit 1
}

echo "==> 首次启动（执行者超时）"
start_server 2500

PROJECT_ID="$(python3 - "$PORT" "$TMP_ROOT/repo" <<'PY'
import json
import sys
import time
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
repo = sys.argv[2]

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
    request = urllib.request.Request(base + path, data=body, method=method,
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request) as response:
        raw = response.read()
        return json.loads(raw) if raw else None

for agent in call("GET", "/api/agents"):
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
mock = call("POST", "/api/agents", {
    "name": "Timeout Mock", "kind": "mock",
    "roles": ["planner", "executor", "verifier"],
    "max_concurrency": 2,
})
# 新建项目的项目池继承默认团队，把测试 Agent 加入默认团队供继承。
for role in ("planner", "executor", "verifier"):
    call("POST", "/api/team/default", {"role": role, "agent_id": mock["id"]})
project = call("POST", "/api/projects", {
    "name": "Timeout", "goal": "验证超时与人工重试",
    "repo_path": repo, "base_branch": "main",
})["project"]
call("POST", f"/api/projects/{project['id']}/confirm", {})

deadline = time.time() + 60
while time.time() < deadline:
    detail = call("GET", f"/api/projects/{project['id']}")
    if detail["project"]["status"] == "blocked":
        break
    time.sleep(0.25)
else:
    raise AssertionError("等待超时阻塞失败")

task = detail["tasks"][0]
assert task["attempts"] == 3, task
assert "超时" in (task["last_error"] or ""), task
assert any(event["kind"] == "task_blocked" for event in detail["events"])
failed_jobs = [job for job in detail["jobs"] if job["status"] == "failed"]
assert failed_jobs, detail["jobs"]
for job in failed_jobs:
    summary = job.get("failure_summary")
    assert summary and summary.get("message"), job
    assert len(summary["message"]) <= 300, job
    assert job.get("error") and "超时" in job["error"], job
print(project["id"])
PY
)"

if [[ -z "${PROJECT_ID:-}" ]]; then
  echo "未取得项目 ID" >&2
  exit 1
fi

echo "==> 重启服务并缩短执行延迟"
kill "$SERVER_PID"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
start_server 10

python3 - "$PORT" "$PROJECT_ID" "$TMP_ROOT/repo" <<'PY'
import json
import subprocess
import sys
import time
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
project_id = sys.argv[2]
repo = sys.argv[3]

def call(method, path, data=None):
    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(base + path, data=body, method=method,
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request) as response:
        raw = response.read()
        return json.loads(raw) if raw else None

call("POST", f"/api/projects/{project_id}/retry", {})
deadline = time.time() + 90
while time.time() < deadline:
    detail = call("GET", f"/api/projects/{project_id}")
    if detail["project"]["stage"] == "summarizing" and detail["project"]["report"]:
        break
    time.sleep(0.3)
else:
    raise AssertionError(f"重试后未完成：{detail['project']}")

assert all(task["stage"] == "completed" for task in detail["tasks"]), detail["tasks"]
call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
detail = call("GET", f"/api/projects/{project_id}")
assert detail["project"]["stage"] == "completed", detail["project"]

branch = detail["project"]["branch"]
subprocess.check_output(["git", "-C", repo, "show", f"{branch}:domo_mock/T1.txt"], text=True)
subprocess.check_output(["git", "-C", repo, "show", f"{branch}:domo_mock/T2.txt"], text=True)
print("超时与人工重试测试通过")
PY

echo "==> 通过：$(basename "$0")"
