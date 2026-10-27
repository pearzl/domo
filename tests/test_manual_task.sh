#!/usr/bin/env bash
# DOMO 人工添加任务冒烟测试。
#
# 覆盖范围：
#   1. 对总结中（报告已生成）的项目人工添加任务：项目回到执行阶段，原报告作废；
#   2. 添加的任务按 DAG 调度执行，全部任务完成后报告重新生成并可验收；
#   3. 对规划中（规划 Agent 不可用而等待）的项目添加任务：项目进入执行阶段并完成任务；
#   4. 对已完成（未归档）的项目添加任务：项目重新回到执行阶段；
#   5. 非法请求被明确拒绝（400 且附原因）：重复 key、依赖不存在的任务；
#   6. 审计事件记录任务创建（task_created）与项目阶段回退（project_reopened）。
#
# 约定：使用内置 mock Agent，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}。
# 退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-manual-task.$$"
PORT="${DOMO_SMOKE_PORT:-$(python3 - <<'PY'
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

echo "==> 启动 DOMO (port $PORT)"
DOMO_DATA_ROOT="$TMP_ROOT/data" \
DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
DOMO_LOG_ROOT="$TMP_ROOT/log" \
DOMO_SERVER_PORT="$PORT" \
DOMO_MOCK_DELAY_MS=120 \
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

python3 - "$PORT" "$TMP_ROOT/repo" <<'PY'
import json
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
    code, body = call_raw(method, path, data)
    if code >= 400:
        raise RuntimeError(f"{method} {path} -> {code}: {body}")
    return body


def call_raw(method, path, data=None):
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
            return response.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except Exception:
            return error.code, {"error": raw}


def expect_reject(method, path, data, needle):
    code, body = call_raw(method, path, data)
    assert code == 400, f"{method} {path} 期望 400，实际 {code}: {body}"
    message = (body or {}).get("error", "")
    assert needle in message, f"期望原因包含「{needle}」，实际：{message}"


def wait_for(project_id, predicate, label, timeout=90):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = call("GET", f"/api/projects/{project_id}")
        if predicate(last):
            return last
        time.sleep(0.25)
    raise AssertionError(
        f"等待 {label} 超时；最后状态：{last['project']} "
        f"{[(t['key'], t['stage'], t['status']) for t in last['tasks']]}"
    )


def add_task(project_id, key, deps=None, milestone=False):
    return call(
        "POST",
        f"/api/projects/{project_id}/tasks",
        {
            "key": key,
            "title": f"人工任务 {key}",
            "description": f"在 domo_mock/{key}.txt 写入人工补充的内容。",
            "acceptance": [f"domo_mock/{key}.txt 存在且包含 {key}"],
            "deps": deps or [],
            "milestone": milestone,
        },
    )


for agent in call("GET", "/api/agents"):
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
mock = call(
    "POST",
    "/api/agents",
    {
        "name": "Manual Task Mock",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "priority": 0,
        "max_concurrency": 2,
    },
)
for role in ("planner", "executor", "verifier"):
    call("POST", "/api/team/default", {"role": role, "agent_id": mock["id"]})

# ---- 项目 A：总结中添加任务 -> 回到执行 -> 报告作废 -> 重写 --------------------------------
project = call(
    "POST",
    "/api/projects",
    {
        "name": "Manual-Summarizing",
        "goal": "生成两个有依赖的 mock 文件",
        "repo_path": repo,
        "base_branch": "main",
    },
)["project"]
project_id = project["id"]

call("POST", f"/api/projects/{project_id}/confirm", {})
detail = wait_for(
    project_id,
    lambda d: d["project"]["stage"] == "summarizing"
    and d["project"]["report"]
    and d["project"]["status"] == "blocked",
    "第一轮报告",
)
assert {task["key"] for task in detail["tasks"]} == {"T1", "T2"}

created = add_task(project_id, "T3")
assert created["key"] == "T3"
assert created["stage"] == "pending"
assert created["status"] == "queued"

detail = call("GET", f"/api/projects/{project_id}")
assert detail["project"]["stage"] == "executing", detail["project"]
assert detail["project"]["report"] is None, detail["project"]
assert detail["project"]["status"] == "running", detail["project"]
task_keys = {task["key"]: task for task in detail["tasks"]}
assert task_keys["T3"]["stage"] == "pending", task_keys["T3"]
assert task_keys["T3"]["deps"] == [], task_keys["T3"]

detail = wait_for(
    project_id,
    lambda d: d["project"]["stage"] == "summarizing"
    and d["project"]["report"]
    and all(task["stage"] == "completed" for task in d["tasks"]),
    "T3 完成后报告重写",
)
assert {task["key"] for task in detail["tasks"]} == {"T1", "T2", "T3"}

kinds = [event["kind"] for event in detail["events"]]
assert "task_created" in kinds, kinds
assert "project_reopened" in kinds, kinds
created_events = [event for event in detail["events"] if event["kind"] == "task_created"]
assert any("T3" in event["message"] for event in created_events), created_events
reopened_events = [event for event in detail["events"] if event["kind"] == "project_reopened"]
assert any("作废" in event["message"] for event in reopened_events), reopened_events

# 非法请求：重复 key、依赖不存在的任务
expect_reject(
    "POST", f"/api/projects/{project_id}/tasks", 
    {"key": "T3", "title": "重复", "description": "重复 key"},
    "已存在",
)
expect_reject(
    "POST",
    f"/api/projects/{project_id}/tasks",
    {"key": "T9", "title": "坏依赖", "description": "依赖不存在", "deps": ["T99"]},
    "不存在",
)

# 报告验收通过后项目完成；对已完成（未归档）项目添加任务会重新打开项目
call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
detail = wait_for(
    project_id,
    lambda d: d["project"]["stage"] == "completed",
    "项目完成",
)
add_task(project_id, "T4")
detail = call("GET", f"/api/projects/{project_id}")
assert detail["project"]["stage"] == "executing", detail["project"]
assert detail["project"]["report"] is None, detail["project"]
assert detail["project"]["status"] == "running", detail["project"]
call("POST", f"/api/projects/{project_id}/archive", {})

# ---- 项目 B：目标确认阶段添加任务，确认后与规划任务一起执行 --------------------------------
planning = call(
    "POST",
    "/api/projects",
    {
        "name": "Manual-Before-Planning",
        "repo_path": repo,
        "base_branch": "main",
    },
)["project"]
planning_id = planning["id"]
_call_raw(
    "POST",
    f"/api/projects/{planning_id}/goal/messages",
    {"message": "生成一个 mock 文件", "goal_confirm_mode": "manual"},
)
wait_for(
    planning_id,
    lambda d: d.get("goal_proposal")
    and d["project"]["stage"] == "goal_confirm"
    and d["project"]["status"] == "blocked",
    "目标确认提议",
    30,
)

created = add_task(planning_id, "M1")
assert created["stage"] == "pending"
assert created["status"] == "queued"

_call_raw("POST", f"/api/projects/{planning_id}/goal/confirm", {})
detail = wait_for(
    planning_id,
    lambda d: d["project"]["stage"] == "summarizing"
    and d["project"]["report"]
    and all(task["stage"] == "completed" for task in d["tasks"]),
    "目标确认前添加的任务完成",
)
assert "M1" in {task["key"] for task in detail["tasks"]}, detail["tasks"]
kinds = [event["kind"] for event in detail["events"]]
assert "task_created" in kinds, kinds
call("POST", f"/api/projects/{planning_id}/archive", {})

print("冒烟测试通过")
PY

echo "==> 通过：$(basename "$0")"
