#!/usr/bin/env bash
# DOMO 任务专用 Agent 池测试。
#
# 覆盖范围：
#   1. 新建任务从项目池对应角色快照初始化；修改任务池只影响该任务，
#      项目池与其他任务保持不变；
#   2. 项目池在任务创建后变化不改写已有任务池，新任务按当时项目池快照初始化；
#   3. 任务池某角色只保留一个启用 Agent 时等价于指定该 Agent；该 Agent 全局
#      不可用时作业给出明确的“任务 Agent 池”等待原因，不回退项目池、
#      不转给其他 Agent；
#   4. 旧 executor_agent_id 覆盖迁移为单 Agent 池，清除覆盖重置为项目池快照，
#      旧字段不再作为调度依据；
#   5. 任务池修改在服务重启后保持。
#
# 使用内置 mock Agent，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}。
# 退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-task-agent-pool.$$"
PORT="${DOMO_TASK_AGENT_POOL_PORT:-$(python3 - <<'PY'
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
  # keep for debugging
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
  DOMO_MOCK_DELAY_MS=60 \
  DOMO_MOCK_EXECUTOR_DELAY_MS=2000 \
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

stop_server() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
}

echo "==> 启动 DOMO (port $PORT)"
start_server

python3 - "$PORT" "$TMP_ROOT/repo" "$TMP_ROOT/state.json" <<'PY'
import json
import sys
import time
import urllib.error
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
repo = sys.argv[2]
state_path = sys.argv[3]

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
            raise AssertionError(f"{method} {path} 应被拒绝，实际返回 {response.status}")
    except urllib.error.HTTPError as error:
        return error.code


def wait_for(project_id, predicate, label, timeout=150):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = call("GET", f"/api/projects/{project_id}")
        if predicate(last):
            return last
        time.sleep(0.1)
    raise AssertionError(
        f"等待 {label} 超时；最后状态：{last['project']} "
        f"{[(t['key'], t['stage'], t['status'], t['waiting_reason']) for t in last['tasks']]}"
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


def task_pool_role(project_id, task_id, role):
    pool = call("GET", f"/api/projects/{project_id}/tasks/{task_id}/agents")
    return next(group for group in pool["roles"] if group["role"] == role)


def summarize(group):
    return [[e["agent_id"], e["priority"], e["enabled"]] for e in group["entries"]]


def add_task(project_id, key, deps=()):
    return call(
        "POST",
        f"/api/projects/{project_id}/tasks",
        {
            "key": key,
            "title": f"{key} 标题",
            "description": f"{key} 描述",
            "acceptance": ["产物存在"],
            "deps": list(deps),
        },
    )


def wait_t1_executor_running(project_id, label):
    return wait_for(
        project_id,
        lambda d: any(
            task["key"] == "T1"
            and any(
                j["task_id"] == task["id"]
                and j["role"] == "executor"
                and j["status"] == "running"
                for j in d["jobs"]
            )
            for task in d["tasks"]
        ),
        label,
    )


def clear_team():
    # 把存量 Agent 按默认团队和全局启停都清理掉，项目池从空基线快照。
    for agent in call("GET", "/api/agents"):
        call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
        for role in ("planner", "executor", "verifier"):
            try:
                call("DELETE", f"/api/team/default/{role}/{agent['id']}")
            except RuntimeError:
                pass


clear_team()

high = call(
    "POST",
    "/api/agents",
    {
        "name": "Task High",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 2,
    },
)
low = call(
    "POST",
    "/api/agents",
    {
        "name": "Task Low",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 1,
    },
)
for role in ("planner", "executor", "verifier"):
    for agent in (high, low):
        call("POST", "/api/team/default", {"role": role, "agent_id": agent["id"]})

state = {}
state["high"] = high["id"]
state["low"] = low["id"]

# ---------- 1. 快照初始化与独立性 ----------
snap_id = create_project("TaskSnap", "新建任务获得项目池角色快照")
confirm(snap_id)
detail = wait_t1_executor_running(snap_id, "TaskSnap T1 执行作业")
t1 = task_by_key(detail, "T1")
t2 = task_by_key(detail, "T2")

pool_view = call("GET", f"/api/projects/{snap_id}/tasks/{t2['id']}/agents")
assert [g["role"] for g in pool_view["roles"]] == ["executor", "verifier"], pool_view
initial_exec = summarize(pool_role(snap_id, "executor"))
assert summarize(task_pool_role(snap_id, t2["id"], "executor")) == initial_exec, (
    summarize(task_pool_role(snap_id, t2["id"], "executor"))
)

# 未知角色被拒绝
code = expect_error(
    "PATCH",
    f"/api/projects/{snap_id}/tasks/{t2['id']}/agents/planner/{high['id']}",
    {"enabled": True},
)
assert code == 400, code

# 修改 T2 任务池：排序、启停、移除、加入。
call("PATCH", f"/api/projects/{snap_id}/tasks/{t2['id']}/agents/executor/{low['id']}", {"position": 0})
code = expect_error(
    "PATCH",
    f"/api/projects/{snap_id}/tasks/{t2['id']}/agents/executor/{low['id']}",
    {"position": 5},
)
assert code == 400, code
call("PATCH", f"/api/projects/{snap_id}/tasks/{t2['id']}/agents/executor/{low['id']}", {"enabled": False})
call("DELETE", f"/api/projects/{snap_id}/tasks/{t2['id']}/agents/verifier/{low['id']}")
call("POST", f"/api/projects/{snap_id}/tasks/{t2['id']}/agents", {"role": "verifier", "agent_id": low["id"]})
# 任务池中已有的 Agent 不能重复加入
code = expect_error(
    "POST",
    f"/api/projects/{snap_id}/tasks/{t2['id']}/agents",
    {"role": "executor", "agent_id": high["id"]},
)
assert code == 400, code

# 项目池与其他任务不受影响
assert summarize(pool_role(snap_id, "executor")) == initial_exec, summarize(
    pool_role(snap_id, "executor")
)
assert summarize(task_pool_role(snap_id, t1["id"], "executor")) == initial_exec, (
    summarize(task_pool_role(snap_id, t1["id"], "executor"))
)
exec_ids = [e["agent_id"] for e in task_pool_role(snap_id, t2["id"], "executor")["entries"]]
assert exec_ids == [low["id"], high["id"]], exec_ids
verifier_sum = [
    (e["agent_id"], e["priority"])
    for e in task_pool_role(snap_id, t2["id"], "verifier")["entries"]
]
assert verifier_sum == [(high["id"], 0), (low["id"], 1)], verifier_sum
state["initial_executor"] = initial_exec
state["t1_executor"] = summarize(task_pool_role(snap_id, t1["id"], "executor"))
state["t2_executor"] = summarize(task_pool_role(snap_id, t2["id"], "executor"))
state["t2_verifier"] = summarize(task_pool_role(snap_id, t2["id"], "verifier"))

# ---------- 2. 项目池变化不改写既有任务池 ----------
call("PATCH", f"/api/projects/{snap_id}/agents/executor/{low['id']}", {"position": 0})
assert [e["agent_id"] for e in task_pool_role(snap_id, t2["id"], "executor")["entries"]] == [
    low["id"],
    high["id"],
], task_pool_role(snap_id, t2["id"], "executor")
new_task = add_task(snap_id, "T3", deps=["T2"])
snapshot_t3 = [
    (e["agent_id"], e["priority"])
    for e in task_pool_role(snap_id, new_task["id"], "executor")["entries"]
]
assert snapshot_t3 == [(low["id"], 0), (high["id"], 1)], snapshot_t3
project_executor = pool_role(snap_id, "executor")
state["project_executor"] = summarize(project_executor)

# ---------- 3. 单 Agent 等价指定与阻塞 ----------
uniq_id = create_project("TaskUnique", "任务池单 Agent 等价指定且不可用时不静默回退")
confirm(uniq_id)
detail = wait_t1_executor_running(uniq_id, "TaskUnique T1 执行作业")
u2 = task_by_key(detail, "T2")
# 等 u1 的验收作业派发后，任务池已完成过一次调度
wait_for(
    uniq_id,
    lambda d: any(
        j["task_id"] == task_by_key(d, "T1")["id"]
        and j["role"] == "verifier"
        and j["status"] in ("running", "succeeded")
        for j in d["jobs"]
    ),
    "TaskUnique T1 验收作业",
)
# u2 的任务池执行者与验收者都只剩 high
call("DELETE", f"/api/projects/{uniq_id}/tasks/{u2['id']}/agents/executor/{low['id']}")
call("DELETE", f"/api/projects/{uniq_id}/tasks/{u2['id']}/agents/verifier/{low['id']}")
# 把 high 全局禁用：u2 只能等待，不能静默回退到 low
call("PATCH", f"/api/agents/{high['id']}", {"enabled": False})
detail = wait_for(
    uniq_id,
    lambda d: any(
        task["key"] == "T2"
        and (task["waiting_reason"] or "").find("任务 Agent 池") != -1
        for task in d["tasks"]
    ),
    "u2 执行等待原因",
    timeout=60,
)
# 禁用前已经完成的历史作业不影响判断：不得有任何 T2 作业落到池外的 low。
fallback_jobs = [
    j
    for j in detail["jobs"]
    if j["task_id"] == u2["id"] and j["agent_id"] != high["id"]
]
assert fallback_jobs == [], fallback_jobs

# 恢复 high 后 u2 完全由唯一 Agent 推进
call("PATCH", f"/api/agents/{high['id']}", {"enabled": True})
detail = wait_for(
    uniq_id,
    lambda d: d["project"]["stage"] == "summarizing" and d["project"]["report"],
    "TaskUnique 完成",
)
for job in detail["jobs"]:
    if job["task_id"] == u2["id"] and job["role"] in ("executor", "verifier"):
        assert job["agent_id"] == high["id"], job

# ---------- 4. 旧覆盖迁移与清除重置 ----------
legacy_id = create_project("TaskLegacy", "旧覆盖字段迁移为单 Agent 池")
confirm(legacy_id)
detail = wait_t1_executor_running(legacy_id, "TaskLegacy T1 执行作业")
lt2 = task_by_key(detail, "T2")
legacy_task = call(
    "PATCH",
    f"/api/projects/{legacy_id}/tasks/{lt2['id']}/agents",
    {"executor_agent_id": low["id"]},
)
assert legacy_task["executor_agent_id"] == low["id"], legacy_task
legacy_pool = task_pool_role(legacy_id, lt2["id"], "executor")
assert [e["agent_id"] for e in legacy_pool["entries"]] == [low["id"]], legacy_pool
cleared = call(
    "PATCH",
    f"/api/projects/{legacy_id}/tasks/{lt2['id']}/agents",
    {"executor_agent_id": None},
)
assert cleared["executor_agent_id"] is None, cleared
legacy_pool = task_pool_role(legacy_id, lt2["id"], "executor")
assert [e["agent_id"] for e in legacy_pool["entries"]] == [high["id"], low["id"]], (
    legacy_pool
)
# 调度依据任务池：T2 的作业交给池内最高优先级 high
detail = wait_for(
    legacy_id,
    lambda d: d["project"]["stage"] == "summarizing" and d["project"]["report"],
    "TaskLegacy 完成",
)
t2_exec = next(
    j for j in detail["jobs"] if j["task_id"] == lt2["id"] and j["role"] == "executor"
)
assert t2_exec["agent_id"] == high["id"], t2_exec

state["t1_project"] = snap_id
json.dump(state, open(state_path, "w"))
print("OK")
PY

echo "==> 重启服务验证持久化"
stop_server
start_server

python3 - "$PORT" "$TMP_ROOT/state.json" <<'PY'
import json
import sys
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
state = json.load(open(sys.argv[2]))


def call(method, path, data=None):
    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(
        base + path, data=body, method=method, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(request) as response:
        raw = response.read()
        return json.loads(raw) if raw else None


snap_id = state["t1_project"]
detail = call("GET", f"/api/projects/{snap_id}")
t1 = next(t for t in detail["tasks"] if t["key"] == "T1")
t2 = next(t for t in detail["tasks"] if t["key"] == "T2")


def task_pool_role(task_id, role):
    pool = call("GET", f"/api/projects/{snap_id}/tasks/{task_id}/agents")
    return next(g for g in pool["roles"] if g["role"] == role)


def summarize(group):
    return [[e["agent_id"], e["priority"], e["enabled"]] for e in group["entries"]]


assert summarize(task_pool_role(t2["id"], "executor")) == state["t2_executor"], (
    summarize(task_pool_role(t2["id"], "executor")),
    state["t2_executor"],
)
assert summarize(task_pool_role(t1["id"], "executor")) == state["t1_executor"], (
    summarize(task_pool_role(t1["id"], "executor")),
    state["t1_executor"],
)
assert summarize(task_pool_role(t2["id"], "verifier")) == state["t2_verifier"], (
    summarize(task_pool_role(t2["id"], "verifier")),
    state["t2_verifier"],
)
project = call("GET", f"/api/projects/{snap_id}/agents")
project_executor = next(g for g in project["roles"] if g["role"] == "executor")
assert summarize(project_executor) == state["project_executor"], (
    summarize(project_executor),
    state["project_executor"],
)
print("OK")
PY
echo "==> 全部通过"
