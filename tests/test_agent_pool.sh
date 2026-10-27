#!/usr/bin/env bash
# DOMO 项目级 Agent 池与任务级 Agent 选择测试。
#
# 覆盖范围：
#   1. 项目创建时从全局 Agent 配置快照初始化，并包含禁用条目；
#   2. 规划、执行、验收分别按角色从项目池中启用且优先级最高的 Agent 选人；
#   3. 调整项目池顺序后，新作业按新顺序选人；
#   4. 禁用项目池中最高优先级的 Agent 后自动回退到下一优先级；
#   5. 任务级指定执行/验收 Agent 优先于项目池，清除后恢复项目池规则；
#   6. 项目池配置在服务重启后保持；
#   7. 全局 Agent 删除后项目池条目标记为不可用，并给出可读的阻塞原因；
#   8. 项目池角色校验拒绝未知角色。
#
# 使用内置 mock Agent，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}。
# 退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-agent-pool.$$"
PORT="${DOMO_AGENT_POOL_PORT:-$(python3 - <<'PY'
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
  DOMO_DATA_ROOT="$TMP_ROOT/data" \
  DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
  DOMO_LOG_ROOT="$TMP_ROOT/log" \
  DOMO_SERVER_PORT="$PORT" \
  DOMO_MOCK_DELAY_MS=80 \
  DOMO_MOCK_EXECUTOR_DELAY_MS=800 \
  "$ROOT/server/target/debug/domo-server" >>"$TMP_ROOT/server.log" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 100); do
    if curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null; then
      return 0
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "服务启动失败" >&2
      tail -120 "$TMP_ROOT/server.log" >&2
      exit 1
    fi
    sleep 0.1
  done
  echo "等待服务启动超时" >&2
  exit 1
}

echo "==> 启动 DOMO (port $PORT)"
start_server

PROJECT2_ID="$(python3 - "$PORT" "$TMP_ROOT/repo" "$TMP_ROOT/state.json" <<'PY'
import json
import sys
import time
import urllib.error
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
repo = sys.argv[2]
state_path = sys.argv[3]


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
        raise RuntimeError(
            f"{method} {path} -> {error.code}: {error.read().decode()}"
        ) from error


def expect_error(method, path, data=None):
    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(
        base + path,
        data=body,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request) as response:
            raise AssertionError(
                f"{method} {path} 应被拒绝，实际返回 {response.status}"
            )
    except urllib.error.HTTPError as error:
        return error.code


def wait_for(project_id, predicate, label, timeout=120):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = call("GET", f"/api/projects/{project_id}")
        if predicate(last):
            return last
        time.sleep(0.12)
    raise AssertionError(
        f"等待 {label} 超时；最后状态：{last['project']} "
        f"{[(t['key'], t['stage'], t['status']) for t in last['tasks']]}"
    )


def create_project(name, goal):
    return call(
        "POST",
        "/api/projects",
        {"name": name, "goal": goal, "repo_path": repo, "base_branch": "main"},
    )["project"]["id"]


def confirm(project_id):
    call("POST", f"/api/projects/{project_id}/confirm", {})


def task_by_key(detail, key):
    return next(task for task in detail["tasks"] if task["key"] == key)


def pool_role(project_id, role):
    pool = call("GET", f"/api/projects/{project_id}/agents")
    return next(group for group in pool["roles"] if group["role"] == role)


def role_jobs(detail, role):
    return [job for job in detail["jobs"] if job["role"] == role]


def wait_role_job(project_id, role, key, timeout=60):
    def found(detail):
        task = task_by_key(detail, key)
        return any(
            job["task_id"] == task["id"] and job["role"] == role
            for job in detail["jobs"]
        )

    detail = wait_for(project_id, found, f"{key} 的{role}作业", timeout)
    task = task_by_key(detail, key)
    return next(
        job
        for job in detail["jobs"]
        if job["task_id"] == task["id"] and job["role"] == role
    )


def wait_completed(project_id, label):
    return wait_for(
        project_id,
        lambda detail: detail["project"]["stage"] == "summarizing"
        and detail["project"]["report"],
        label,
    )


for agent in call("GET", "/api/agents"):
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})

high = call(
    "POST",
    "/api/agents",
    {
        "name": "Pool High",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "priority": 0,
        "max_concurrency": 2,
    },
)
low = call(
    "POST",
    "/api/agents",
    {
        "name": "Pool Low",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "priority": 1,
        "max_concurrency": 2,
    },
)

# 1. 默认顺序：三个角色都使用各自角色下优先级最高的 Agent。
basic_id = create_project("PoolBasic", "项目池按角色选择最高优先级 Agent")
confirm(basic_id)
basic = wait_completed(basic_id, "基础项目完成")
goal_jobs = role_jobs(basic, "goal")
exec_jobs = role_jobs(basic, "executor")
verify_jobs = role_jobs(basic, "verifier")
assert goal_jobs and goal_jobs[0]["agent_id"] == high["id"], goal_jobs
assert exec_jobs and all(job["agent_id"] == high["id"] for job in exec_jobs), exec_jobs
assert verify_jobs and all(
    job["agent_id"] == high["id"] for job in verify_jobs
), verify_jobs

# 2. 调整执行角色的项目池顺序后，新作业使用新的最高优先级 Agent。
reordered_id = create_project("PoolReorder", "调整顺序后按新顺序选人")
planner_before = pool_role(reordered_id, "planner")["entries"]
executor_before = pool_role(reordered_id, "executor")["entries"]
assert [entry["agent_id"] for entry in executor_before if entry["enabled"]][:2] == [
    high["id"],
    low["id"],
], executor_before
assert [entry["priority"] for entry in executor_before[:3]] == [0, 1, 2], executor_before
updated = call(
    "PATCH",
    f"/api/projects/{reordered_id}/agents/executor/{low['id']}",
    {"direction": "up"},
)
executor_after = next(
    group for group in updated["roles"] if group["role"] == "executor"
)["entries"]
assert [entry["agent_id"] for entry in executor_after if entry["enabled"]][:2] == [
    low["id"],
    high["id"],
], executor_after
assert [entry["priority"] for entry in executor_after[:3]] == [0, 1, 2], executor_after
planner_after = next(
    group for group in updated["roles"] if group["role"] == "planner"
)["entries"]
assert planner_after == planner_before, "调整执行角色不应改变规划角色顺序"

# 全局配置在项目创建后变化，不应影响已经快照的项目池。
call("PATCH", f"/api/agents/{high['id']}", {"enabled": False, "priority": 9})
decoupled = pool_role(reordered_id, "planner")["entries"]
assert next(entry for entry in decoupled if entry["agent_id"] == high["id"])[
    "enabled"
] is True, decoupled

confirm(reordered_id)
reordered = wait_completed(reordered_id, "调整顺序后项目完成")
reordered_execs = role_jobs(reordered, "executor")
assert reordered_execs and all(
    job["agent_id"] == low["id"] for job in reordered_execs
), reordered_execs
assert role_jobs(reordered, "goal")[0]["agent_id"] == high["id"], reordered["jobs"]
assert all(
    job["agent_id"] == high["id"] for job in role_jobs(reordered, "verifier")
), reordered["jobs"]
call("PATCH", f"/api/agents/{high['id']}", {"enabled": True, "priority": 0})

# 3. 禁用最高优先级 Agent 后自动回退到下一优先级。
fallback_id = create_project("PoolFallback", "禁用最高优先级后自动回退")
for role in ("planner", "executor", "verifier"):
    call(
        "PATCH",
        f"/api/projects/{fallback_id}/agents/{role}/{high['id']}",
        {"enabled": False},
    )
fallback_executor = pool_role(fallback_id, "executor")["entries"]
high_entry = next(
    entry for entry in fallback_executor if entry["agent_id"] == high["id"]
)
assert high_entry["enabled"] is False, high_entry
assert [entry["agent_id"] for entry in fallback_executor if entry["enabled"]][0] == (
    low["id"]
), fallback_executor
confirm(fallback_id)
fallback = wait_completed(fallback_id, "回退后项目完成")
assert role_jobs(fallback, "goal")[0]["agent_id"] == low["id"], fallback["jobs"]
assert all(
    job["agent_id"] == low["id"] for job in role_jobs(fallback, "executor")
), fallback["jobs"]
assert all(
    job["agent_id"] == low["id"] for job in role_jobs(fallback, "verifier")
), fallback["jobs"]

# 4. 任务级指定执行/验收 Agent，优先于项目池。
override_id = create_project("TaskOverride", "任务级指定执行与验收 Agent")
confirm(override_id)
wait_for(
    override_id,
    lambda detail: any(
        task["key"] == "T1"
        and any(
            job["task_id"] == task["id"]
            and job["role"] == "executor"
            and job["status"] in ("running", "succeeded")
            for job in detail["jobs"]
        )
        for task in detail["tasks"]
    ),
    "T1 默认执行作业",
)
detail = call("GET", f"/api/projects/{override_id}")
t1 = task_by_key(detail, "T1")
t2 = task_by_key(detail, "T2")
default_t1_exec = next(
    job
    for job in detail["jobs"]
    if job["task_id"] == t1["id"] and job["role"] == "executor"
)
assert default_t1_exec["agent_id"] == high["id"], default_t1_exec
updated_task = call(
    "PATCH",
    f"/api/projects/{override_id}/tasks/{t2['id']}/agents",
    {"executor_agent_id": low["id"], "verifier_agent_id": low["id"]},
)
assert updated_task["executor_agent_id"] == low["id"], updated_task
assert updated_task["verifier_agent_id"] == low["id"], updated_task
t2_exec = wait_role_job(override_id, "executor", "T2")
assert t2_exec["agent_id"] == low["id"], t2_exec
t2_verify = wait_role_job(override_id, "verifier", "T2")
assert t2_verify["agent_id"] == low["id"], t2_verify
wait_completed(override_id, "任务覆盖项目完成")

# 5. 清除任务覆盖后恢复项目池规则。
clear_id = create_project("TaskOverrideClear", "清除任务覆盖后恢复项目池")
confirm(clear_id)
wait_for(
    clear_id,
    lambda detail: any(
        task["key"] == "T1"
        and any(
            job["task_id"] == task["id"]
            and job["role"] == "executor"
            and job["status"] in ("running", "succeeded")
            for job in detail["jobs"]
        )
        for task in detail["tasks"]
    ),
    "T1 执行作业",
)
detail = call("GET", f"/api/projects/{clear_id}")
t2 = task_by_key(detail, "T2")
call(
    "PATCH",
    f"/api/projects/{clear_id}/tasks/{t2['id']}/agents",
    {"executor_agent_id": low["id"], "verifier_agent_id": low["id"]},
)
cleared = call(
    "PATCH",
    f"/api/projects/{clear_id}/tasks/{t2['id']}/agents",
    {"executor_agent_id": None, "verifier_agent_id": None},
)
assert cleared["executor_agent_id"] is None, cleared
assert cleared["verifier_agent_id"] is None, cleared
t2_exec = wait_role_job(clear_id, "executor", "T2")
assert t2_exec["agent_id"] == high["id"], t2_exec
wait_completed(clear_id, "清除覆盖后项目完成")

# 6. 全局 Agent 删除后，项目池条目标记不可用并给出阻塞原因。
call("PATCH", f"/api/agents/{high['id']}", {"enabled": False})
call("PATCH", f"/api/agents/{low['id']}", {"enabled": False})
doomed_a = call(
    "POST",
    "/api/agents",
    {
        "name": "Doomed A",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "priority": 0,
        "max_concurrency": 1,
    },
)
doomed_b = call(
    "POST",
    "/api/agents",
    {
        "name": "Doomed B",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "priority": 1,
        "max_concurrency": 1,
    },
)
missing_id = create_project("PoolMissing", "全局 Agent 删除后应明确阻塞")
missing_pool = pool_role(missing_id, "planner")["entries"]
assert any(
    entry["agent_id"] == doomed_a["id"] and entry["available"] for entry in missing_pool
), missing_pool
call("DELETE", f"/api/agents/{doomed_a['id']}")
call("DELETE", f"/api/agents/{doomed_b['id']}")
missing_pool = pool_role(missing_id, "planner")["entries"]
unavailable = [
    entry
    for entry in missing_pool
    if entry["agent_id"] in (doomed_a["id"], doomed_b["id"])
]
assert len(unavailable) == 2, missing_pool
assert all(entry["available"] is False for entry in unavailable), unavailable
assert all(
    entry["unavailable_reason"] and "删除" in entry["unavailable_reason"]
    for entry in unavailable
), unavailable
assert {entry["name"] for entry in unavailable} == {"Doomed A", "Doomed B"}, unavailable
confirm(missing_id)
missing = wait_for(
    missing_id,
    lambda detail: detail["project"]["blocked_reason"]
    and "Doomed A" in detail["project"]["blocked_reason"]
    and "Doomed B" in detail["project"]["blocked_reason"],
    "全局 Agent 删除后的阻塞原因",
    30,
)
reason = missing["project"]["blocked_reason"]
assert "不可用" in reason and "删除" in reason, reason
code = expect_error(
    "PATCH",
    f"/api/projects/{reordered_id}/agents/invalid/{high['id']}",
    {"enabled": False},
)
assert code == 400, code

# 保存重启后需要核对的顺序。
json.dump(
    {
        "project_id": reordered_id,
        "executor_order": [entry["agent_id"] for entry in executor_after],
        "planner_order": [entry["agent_id"] for entry in planner_after],
        "missing_project_id": missing_id,
        "missing_agents": [doomed_a["id"], doomed_b["id"]],
    },
    open(state_path, "w", encoding="utf-8"),
    ensure_ascii=False,
)
print(reordered_id)
PY
)"

echo "==> 重启服务，验证项目池配置持久化"
kill "$SERVER_PID"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
start_server

python3 - "$PORT" "$TMP_ROOT/state.json" <<'PY'
import json
import sys
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
state = json.load(open(sys.argv[2], encoding="utf-8"))


def call(path):
    with urllib.request.urlopen(base + path) as response:
        return json.load(response)


pool = call(f"/api/projects/{state['project_id']}/agents")
roles = {group["role"]: group["entries"] for group in pool["roles"]}
assert [entry["agent_id"] for entry in roles["executor"]] == state["executor_order"], roles[
    "executor"
]
assert [entry["agent_id"] for entry in roles["planner"]] == state["planner_order"], roles[
    "planner"
]
assert [entry["priority"] for entry in roles["executor"]] == list(
    range(len(roles["executor"]))
), roles["executor"]

missing = call(f"/api/projects/{state['missing_project_id']}/agents")
missing_entries = {
    group["role"]: group["entries"] for group in missing["roles"]
}
for role, entries in missing_entries.items():
    for entry in entries:
        if entry["agent_id"] in state["missing_agents"]:
            assert entry["available"] is False, (role, entry)
            assert "删除" in (entry["unavailable_reason"] or ""), (role, entry)

print("项目池配置重启后保持")
PY

echo "==> 通过：$(basename "$0")"
