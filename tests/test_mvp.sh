#!/usr/bin/env bash
# DOMO MVP 冒烟测试。
#
# 覆盖范围：
#   1. 目标确认后由规划者生成含依赖的 DAG；
#   2. 任务在独立 worktree 执行，验收通过后按依赖顺序合并到项目分支；
#   3. 验收拒绝后退回执行，携带原因重试；
#   4. 项目暂停 -> 已暂停 -> 恢复 -> 总结 -> 人工通过报告；
#   5. 归档立即停止项目；
#   6. 全流程作业输入、输出、事件均可查询。
#
# 约定：使用内置 mock Agent，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}。
# 退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-smoke.$$"
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
DOMO_MOCK_REJECT_ONCE=1 \
DOMO_MOCK_FAIL_ONCE=1 \
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


def wait_for(project_id, predicate, label, timeout=90):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = call("GET", f"/api/projects/{project_id}")
        if predicate(last):
            return last
        time.sleep(0.25)
    raise AssertionError(f"等待 {label} 超时；最后状态：{last['project']} {[(t['key'], t['stage'], t['status']) for t in last['tasks']]}")


agents = call("GET", "/api/agents")
for agent in agents:
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
mock = call(
    "POST",
    "/api/agents",
    {
        "name": "Smoke Mock",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 2,
    },
)
# 新建项目的项目池继承默认团队，把测试 Agent 加入默认团队供继承。
for role in mock["roles"]:
    call("POST", "/api/team/default", {"role": role, "agent_id": mock["id"]})

project = call(
    "POST",
    "/api/projects",
    {
        "name": "Smoke",
        "goal": "生成两个有依赖的 mock 文件",
        "repo_path": repo,
        "base_branch": "main",
    },
)["project"]
project_id = project["id"]

call("POST", f"/api/projects/{project_id}/confirm", {})
call("POST", f"/api/projects/{project_id}/pause", {})

paused = wait_for(
    project_id,
    lambda detail: detail["project"]["status"] == "paused",
    "项目暂停",
    30,
)
assert paused["project"]["stage"] in ("planning", "goal_confirm", "executing"), paused["project"]

call("POST", f"/api/projects/{project_id}/resume", {})
detail = wait_for(
    project_id,
    lambda detail: detail["project"]["stage"] == "summarizing"
    and detail["project"]["status"] == "blocked",
    "项目报告",
)

tasks = {task["key"]: task for task in detail["tasks"]}
assert set(tasks) == {"T1", "T2"}, tasks
assert all(task["stage"] == "completed" for task in detail["tasks"]), detail["tasks"]
assert detail["project"]["report"], detail["project"]

events = detail["events"]
assert any(event["kind"] == "task_rejected" for event in events), "未覆盖验收拒绝路径"
assert any(event["kind"] == "task_retry" for event in events), "未覆盖执行失败重试路径"
assert any(event["kind"] == "plan_created" for event in events), "缺少规划事件"
assert any(event["kind"] == "task_completed" for event in events), "缺少完成事件"

jobs = detail["jobs"]
roles = {job["role"] for job in jobs}
assert {"goal", "executor", "verifier", "summary"} <= roles, roles
assert any(job["status"] == "failed" for job in jobs), "未覆盖失败作业审计"
assert any(
    job["role"] == "verifier" and job["attempt"] > 1 for job in jobs
), "未覆盖验收拒绝后的再次验收"
assert tasks["T1"]["attempts"] >= 1, tasks["T1"]

branch = detail["project"]["branch"]
import subprocess

t1 = subprocess.check_output(
    ["git", "-C", repo, "show", f"{branch}:domo_mock/T1.txt"], text=True
)
t2 = subprocess.check_output(
    ["git", "-C", repo, "show", f"{branch}:domo_mock/T2.txt"], text=True
)
assert "T1" in t1
assert "dep T1 was merged" in t2

branch_log = subprocess.check_output(
    ["git", "-C", repo, "log", "--format=%s", branch], text=True
).splitlines()
assert not any("Merge" in line for line in branch_log), branch_log
assert branch_log[0].startswith("domo(task-T2)"), branch_log
assert branch_log[1].startswith("domo(task-T1)"), branch_log

call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
completed = call("GET", f"/api/projects/{project_id}")
assert completed["project"]["stage"] == "completed", completed["project"]
assert completed["project"]["status"] is None

qa = call(
    "POST",
    "/api/projects",
    {
        "name": "Clarify",
        "goal": "[ask] 先澄清再执行",
        "repo_path": repo,
        "base_branch": "main",
    },
)["project"]
call("POST", f"/api/projects/{qa['id']}/confirm", {})
qa_detail = wait_for(
    qa["id"],
    lambda detail: detail["project"]["stage"] == "goal_confirm"
    and detail["project"]["status"] == "blocked"
    and any(message["role"] == "planner" for message in detail["messages"]),
    "规划者提问",
    30,
)
assert any("哪个使用场景" in message["content"] for message in qa_detail["messages"])
call(
    "POST",
    f"/api/projects/{qa['id']}/confirm",
    {"message": "没有偏好，请直接规划。"},
)
wait_for(
    qa["id"],
    lambda detail: len(detail["tasks"]) > 0
    or detail["project"]["stage"] in ("summarizing", "completed"),
    "澄清后生成任务",
    30,
)
call("POST", f"/api/projects/{qa['id']}/archive", {})
archived_detail = call("GET", f"/api/projects/{qa['id']}")
assert archived_detail["project"]["status"] == "archived", archived_detail["project"]

print("冒烟测试通过")
print(f"  项目分支: {branch}")
print(f"  作业数量: {len(jobs)}")
print(f"  事件数量: {len(events)}")
PY

echo "==> 通过：$(basename "$0")"
