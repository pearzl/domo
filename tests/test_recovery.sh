#!/usr/bin/env bash
# DOMO 重启恢复测试。
#
# 场景：执行者作业运行中直接 kill -9 服务，随后用同一数据目录重启。
# 验收标准：
#   1. 中断作业被标记失败，并产生恢复事件；
#   2. 任务在重启后重新调度并最终完成；
#   3. 项目仍能产出报告，不会出现重复的活跃作业。
#
# 使用内置 mock Agent，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-recovery.$$"
PORT="${DOMO_RECOVERY_PORT:-$(python3 - <<'PY'
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
  local delay="$1"
  DOMO_DATA_ROOT="$TMP_ROOT/data" \
  DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
  DOMO_LOG_ROOT="$TMP_ROOT/log" \
  DOMO_SERVER_PORT="$PORT" \
  DOMO_MOCK_DELAY_MS="$delay" \
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

echo "==> 首次启动，准备一个执行中作业"
start_server 1500

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
        and not path.endswith("/goal/confirm")
    ):
        project_id = path.split("/")[-2]
        detail = _call_raw("GET", f"/api/projects/{project_id}")
        if not detail["messages"] and project_id in _pending_goals:
            _call_raw(
                "POST",
                f"/api/projects/{project_id}/goal/messages",
                {
                    "message": _pending_goals.pop(project_id),
                    "goal_confirm_mode": "auto",
                },
            )
        message = (data or {}).get("message")
        if message:
            _call_raw(
                "POST",
                f"/api/projects/{project_id}/goal/messages",
                {"message": message, "goal_confirm_mode": "auto"},
            )
        deadline = time.time() + 30
        while time.time() < deadline:
            detail = _call_raw("GET", f"/api/projects/{project_id}")
            if detail["project"]["stage"] != "goal_confirm":
                return detail
            if detail.get("goal_proposal") and not detail["jobs"]:
                return _call_raw(
                    "POST",
                    f"/api/projects/{project_id}/goal/confirm",
                    {"message": None},
                )
            last = detail["messages"][-1] if detail["messages"] else None
            if last and last["role"] == "planner" and not detail.get("goal_proposal"):
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
    "name": "Recovery Mock", "kind": "mock",
    "roles": ["planner", "executor", "verifier"],
    "max_concurrency": 2,
})
# 新建项目的项目池继承默认团队，把测试 Agent 加入默认团队供继承。
for role in ("planner", "executor", "verifier"):
    call("POST", "/api/team/default", {"role": role, "agent_id": mock["id"]})
project = call("POST", "/api/projects", {
    "name": "Recovery", "goal": "验证服务重启恢复",
    "repo_path": repo, "base_branch": "main",
})["project"]
call("POST", f"/api/projects/{project['id']}/confirm", {})
deadline = time.time() + 60
while time.time() < deadline:
    detail = call("GET", f"/api/projects/{project['id']}")
    if any(job["role"] == "executor" and job["status"] == "running" for job in detail["jobs"]):
        print(project["id"])
        break
    time.sleep(0.2)
else:
    raise SystemExit("等待执行中作业超时")
PY
)"

if [[ -z "${PROJECT_ID:-}" ]]; then
  echo "未取得项目 ID" >&2
  exit 1
fi

echo "==> kill -9 服务"
kill -9 "$SERVER_PID"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""

echo "==> 使用同一数据目录重启"
start_server 80

python3 - "$PORT" "$PROJECT_ID" <<'PY'
import json
import sys
import time
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
project_id = sys.argv[2]

def call(path):
    with urllib.request.urlopen(base + path) as response:
        return json.load(response)

deadline = time.time() + 120
last = None
while time.time() < deadline:
    detail = call(f"/api/projects/{project_id}")
    last = detail
    if detail["project"]["stage"] == "summarizing" and detail["project"]["report"]:
        break
    time.sleep(0.3)
else:
    raise AssertionError(f"重启后未能完成项目：{last['project']} {[(t['key'], t['stage'], t['status']) for t in last['tasks']]}")

assert all(task["stage"] == "completed" for task in detail["tasks"]), detail["tasks"]
assert any(event["kind"] == "job_recovered" for event in detail["events"]), "缺少恢复事件"
assert not any(job["status"] == "running" for job in detail["jobs"]), "存在重复或残留的活跃作业"
assert detail["project"]["report"], "缺少项目报告"
print("重启恢复测试通过")
PY

echo "==> 通过：$(basename "$0")"
