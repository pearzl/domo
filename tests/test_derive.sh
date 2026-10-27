#!/usr/bin/env bash
# DOMO 执行者任务派生测试。
#
# 覆盖范围：
#   1. 首个执行作业返回 derive 后，子任务写入同一项目 DAG；
#   2. 子任务自动成为父任务前置，兄弟任务 deps 为空，父任务保持等待且没有失败；
#   3. 子任务跳过验收，执行成功后直接合入父任务分支；
#   4. 全部子任务完成后父任务再次执行并通过验收，最终合入项目交付分支；
#   5. 父任务 prompt 包含派生子任务及其结果，子任务 prompt 标记派生并禁止再次派生；
#   6. 非法派生使作业失败并进入既有重试流程，不写入子任务；
#   7. 项目报告可生成并完成人工验收。
#
# 使用内置 mock Agent，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}。
# 退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-derive.$$"
PORT="${DOMO_DERIVE_PORT:-$(python3 - <<'PY'
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
DOMO_MOCK_DELAY_MS=60 \
DOMO_MOCK_EXECUTOR_DELAY_MS=350 \
DOMO_MOCK_DERIVE_ONCE=1 \
DOMO_MOCK_DERIVE_GOAL='[derive]' \
DOMO_MOCK_DERIVE_KEY=T1 \
DOMO_MOCK_INVALID_DERIVE_ONCE=1 \
DOMO_MOCK_INVALID_DERIVE_GOAL='[invalid-derive]' \
DOMO_MOCK_INVALID_DERIVE_KEY=T1 \
"$ROOT/server/target/debug/domo-server" >"$TMP_ROOT/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 100); do
  if curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null; then
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "服务启动失败" >&2
    tail -100 "$TMP_ROOT/server.log" >&2
    exit 1
  fi
  sleep 0.1
done

curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null

python3 - "$PORT" "$TMP_ROOT/repo" <<'PY'
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
repo = sys.argv[2]


def call(method, path, data=None):
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


def wait_for(project_id, predicate, label, timeout=120):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = call("GET", f"/api/projects/{project_id}")
        if predicate(last):
            return last
        time.sleep(0.15)
    raise AssertionError(
        f"等待 {label} 超时；最后状态：{last['project']} "
        f"{[(t['key'], t['stage'], t['status'], t['parent_id']) for t in last['tasks']]}"
    )


def new_project(name, goal):
    project = call(
        "POST",
        "/api/projects",
        {
            "name": name,
            "goal": goal,
            "repo_path": repo,
            "base_branch": "main",
        },
    )["project"]
    call("POST", f"/api/projects/{project['id']}/confirm", {})
    return project["id"]


for agent in call("GET", "/api/agents"):
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
mock = call(
    "POST",
    "/api/agents",
    {
        "name": "Derive Mock",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 2,
    },
)
# 新建项目的项目池继承默认团队，把测试 Agent 加入默认团队供继承。
for role in ("planner", "executor", "verifier"):
    call("POST", "/api/team/default", {"role": role, "agent_id": mock["id"]})

# 1. 合法派生：子任务进入 DAG，父任务等待。
project_id = new_project("Derive", "[derive] 将复杂任务派生为两个并行子任务")
detail = wait_for(
    project_id,
    lambda item: any(event["kind"] == "task_derived" for event in item["events"]),
    "派生事件",
    90,
)
tasks = {task["key"]: task for task in detail["tasks"]}
assert {"T1", "T2", "T1.1", "T1.2"} <= set(tasks), tasks
parent = tasks["T1"]
children = [task for task in detail["tasks"] if task["parent_id"] == parent["id"]]
assert {child["key"] for child in children} == {"T1.1", "T1.2"}, children
assert all(child["deps"] == [] for child in children), children
assert all(child["status"] in ("queued", "running") for child in children), children
assert set(parent["deps"]) == {"T1.1", "T1.2"}, parent
assert parent["stage"] == "pending", parent
assert parent["status"] == "queued", parent
assert parent["status"] != "blocked", parent
derive_event = next(
    event for event in detail["events"] if event["kind"] == "task_derived"
)
assert "T1.1" in derive_event["message"] and "T1.2" in derive_event["message"], derive_event

# 2. 全部子任务完成后父任务再次执行并完成，项目产出报告。
detail = wait_for(
    project_id,
    lambda item: item["project"]["stage"] == "summarizing"
    and item["project"]["report"]
    and all(
        task["stage"] == "completed"
        for task in item["tasks"]
        if task["status"] != "canceled"
    ),
    "父任务完成并生成报告",
)
tasks = {task["key"]: task for task in detail["tasks"]}
parent = tasks["T1"]
children = [task for task in detail["tasks"] if task["parent_id"] == parent["id"]]
assert parent["stage"] == "completed", parent
assert parent["status"] is None, parent
assert all(child["stage"] == "completed" for child in children), children
assert all(child["result_summary"] for child in children), children
assert detail["project"]["report"], detail["project"]

jobs = detail["jobs"]
parent_execs = sorted(
    (
        job
        for job in jobs
        if job["task_id"] == parent["id"] and job["role"] == "executor"
    ),
    key=lambda job: job["created_at"],
)
assert len(parent_execs) >= 2, parent_execs
for child in children:
    child_jobs = [
        job for job in jobs if job["task_id"] == child["id"] and job["role"] == "executor"
    ]
    assert child_jobs, child
    assert all(job["status"] == "succeeded" for job in child_jobs), child_jobs
    verifier_jobs = [
        job for job in jobs if job["task_id"] == child["id"] and job["role"] == "verifier"
    ]
    assert not verifier_jobs, verifier_jobs

parent_prompt = call("GET", f"/api/jobs/{parent_execs[-1]['id']}")["prompt"]
assert "## 派生子任务" in parent_prompt, parent_prompt
assert "T1.1" in parent_prompt and "T1.2" in parent_prompt, parent_prompt
assert "已写入 domo_mock/T1.1.txt" in parent_prompt, parent_prompt
child_prompt = call(
    "GET",
    f"/api/jobs/{next(job for job in jobs if job['task_id'] == children[0]['id'] and job['role'] == 'executor')['id']}",
)["prompt"]
assert "## 派生任务约束" in child_prompt, child_prompt
assert "跳过验收阶段" in child_prompt, child_prompt
assert "禁止再次派生新的子任务" in child_prompt, child_prompt
assert children[0]["key"] in child_prompt, child_prompt

# 3. 项目交付分支同时包含父任务和子任务产物。
branch = detail["project"]["branch"]
for name in ("T1", "T1.1", "T1.2", "T2"):
    content = subprocess.check_output(
        ["git", "-C", repo, "show", f"{branch}:domo_mock/{name}.txt"], text=True
    )
    assert name in content, (name, content)
parent_file = subprocess.check_output(
    ["git", "-C", repo, "show", f"{branch}:domo_mock/T1.txt"], text=True
)
assert "dep T1.1 was merged" in parent_file, parent_file
assert "dep T1.2 was merged" in parent_file, parent_file

call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
completed = call("GET", f"/api/projects/{project_id}")
assert completed["project"]["stage"] == "completed", completed["project"]
assert completed["project"]["status"] is None, completed["project"]

# 4. 非法派生：作业失败并进入既有重试流程，不写入子任务。
invalid_id = new_project("InvalidDerive", "[invalid-derive] 非法派生应进入重试")
detail = wait_for(
    invalid_id,
    lambda item: any(
        job["role"] == "executor"
        and job["status"] == "failed"
        and "派生" in (job["error"] or "")
        for job in item["jobs"]
    ),
    "非法派生作业失败",
    90,
)
failed_job = next(
    job
    for job in detail["jobs"]
    if job["role"] == "executor"
    and job["status"] == "failed"
    and "派生" in (job["error"] or "")
)
assert "deps" in failed_job["error"], failed_job
invalid_t1 = next(task for task in detail["tasks"] if task["key"] == "T1")
assert invalid_t1["attempts"] >= 1, invalid_t1
assert invalid_t1["status"] != "blocked", invalid_t1
assert not any(task["parent_id"] for task in detail["tasks"]), detail["tasks"]
assert any(event["kind"] == "task_retry" for event in detail["events"]), detail["events"]

detail = wait_for(
    invalid_id,
    lambda item: item["project"]["stage"] == "summarizing"
    and item["project"]["report"]
    and all(
        task["stage"] == "completed"
        for task in item["tasks"]
        if task["status"] != "canceled"
    ),
    "非法派生重试后完成",
)
assert not any(task["parent_id"] for task in detail["tasks"]), detail["tasks"]
assert {task["key"] for task in detail["tasks"]} == {"T1", "T2"}, detail["tasks"]

print("派生流程测试通过")
print(f"  项目分支: {branch}")
print(f"  父任务执行作业: {len(parent_execs)}")
print(f"  子任务: {', '.join(child['key'] for child in children)}")
PY

echo "==> 通过：$(basename "$0")"
