#!/usr/bin/env bash
# DOMO 队列调序、任务暂停与失败退避测试。
#
# 覆盖范围：
#   1. 就绪任务按稳定排队顺序派发，API 返回排队位置与等待原因；
#   2. 调整尚未开始任务的排队顺序后，执行顺序随之变化并写入审计事件；
#   3. 暂停排队任务后停止派发，恢复后继续；
#   4. 优雅暂停运行中任务：当前作业结束后进入已暂停，期间不派发后续作业；
#   5. 强制暂停终止在途进程，任务进入已暂停且项目不会因此错误阻塞；
#   6. 项目强制暂停终止所有在途作业并进入已暂停；
#   7. 限流、供应商波动、永久错误分别进入冷却、有限重试、直接换人；
#   8. Agent 池耗尽后任务阻塞并可通过人工裁决入口处理；
#   9. 冷却截止时间与失败分类在项目详情中可读。
#
# 使用内置 mock Agent 与本地 sleep CLI Agent，不访问外网；
# 临时数据全部放在 ${TMPDIR:-/tmp}，退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-queue-pause.$$"
PORT="${DOMO_QUEUE_PAUSE_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)}"
SERVER_PID=""

cleanup() {
  local status=$?
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ $status -ne 0 && -f "$TMP_ROOT/server.log" ]]; then
    echo "--- 服务日志 ---" >&2
    tail -200 "$TMP_ROOT/server.log" >&2
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
DOMO_MOCK_DELAY_MS=80 \
DOMO_MOCK_EXECUTOR_DELAY_MS=1000 \
DOMO_MOCK_RATE_LIMIT=1 \
DOMO_MOCK_RATE_LIMIT_GOAL='[ratelimit]' \
DOMO_MOCK_RATE_LIMIT_AGENT='Rate Limited' \
DOMO_MOCK_RATE_LIMIT_RETRY=900 \
DOMO_MOCK_TRANSIENT=1 \
DOMO_MOCK_TRANSIENT_GOAL='[transient]' \
DOMO_MOCK_TRANSIENT_AGENT='Transient Agent' \
DOMO_MOCK_PERMANENT=1 \
DOMO_MOCK_PERMANENT_GOAL='[permanent]' \
DOMO_MOCK_PERMANENT_AGENT='Permanent Agent' \
"$ROOT/server/target/debug/domo-server" >"$TMP_ROOT/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 120); do
  if curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null; then
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "服务启动失败" >&2
    tail -120 "$TMP_ROOT/server.log" >&2
    exit 1
  fi
  sleep 0.1
done
curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null

python3 - "$PORT" "$TMP_ROOT/repo" <<'PY'
import json
import os
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

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
            raise AssertionError(f"{method} {path} 应被拒绝，实际 {response.status}")
    except urllib.error.HTTPError as error:
        return error.code


def wait_for(project_id, predicate, label, timeout=90):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = call("GET", f"/api/projects/{project_id}")
        if predicate(last):
            return last
        time.sleep(0.05)
    raise AssertionError(
        f"等待 {label} 超时；最后状态：{last['project']} "
        f"{[(t['key'], t['stage'], t['status'], t['queue_position']) for t in last['tasks']]}"
    )


def parse_time(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()


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


def role_jobs(detail, role):
    return [job for job in detail["jobs"] if job["role"] == role]


def agent_by_id(detail, agent_id):
    return next(agent for agent in detail["agents"] if agent["id"] == agent_id)


def pool_entry(detail, role, agent_id):
    group = next(item for item in detail["agent_pool"]["roles"] if item["role"] == role)
    return next(entry for entry in group["entries"] if entry["agent_id"] == agent_id)


def wait_project_done(project_id, label):
    detail = wait_for(
        project_id,
        lambda item: item["project"]["stage"] == "summarizing"
        and bool(item["project"]["report"]),
        label,
        timeout=120,
    )
    call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
    return detail


def restrict_executor(project_id, keep_ids):
    pool = call("GET", f"/api/projects/{project_id}/agents")
    group = next(item for item in pool["roles"] if item["role"] == "executor")
    for entry in group["entries"]:
        if entry["agent_id"] not in keep_ids and entry["enabled"]:
            call(
                "PATCH",
                f"/api/projects/{project_id}/agents/executor/{entry['agent_id']}",
                {"enabled": False},
            )


for agent in call("GET", "/api/agents"):
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})

base_agent = call(
    "POST",
    "/api/agents",
    {
        "name": "Base Mock",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 1,
    },
)
rate_limited = call(
    "POST",
    "/api/agents",
    {
        "name": "Rate Limited",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 1,
    },
)
rate_backup = call(
    "POST",
    "/api/agents",
    {
        "name": "Rate Backup",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 1,
    },
)
transient_agent = call(
    "POST",
    "/api/agents",
    {
        "name": "Transient Agent",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 1,
    },
)
transient_backup = call(
    "POST",
    "/api/agents",
    {
        "name": "Transient Backup",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 1,
    },
)
permanent_agent = call(
    "POST",
    "/api/agents",
    {
        "name": "Permanent Agent",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 1,
    },
)
permanent_backup = call(
    "POST",
    "/api/agents",
    {
        "name": "Permanent Backup",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 1,
    },
)
cli_executor = call(
    "POST",
    "/api/agents",
    {
        "name": "Sleep CLI",
        "kind": "cli",
        "roles": ["executor"],
        "max_concurrency": 1,
        "command": ["bash", "-c", "sleep 30"],
    },
)

# 新建项目的项目池继承默认团队；按角色把测试 Agent 按原优先级顺序加入默认团队。
team_order = {
    "planner": [
        rate_limited,
        rate_backup,
        transient_agent,
        transient_backup,
        permanent_agent,
        permanent_backup,
        base_agent,
    ],
    "executor": [
        cli_executor,
        rate_limited,
        rate_backup,
        transient_agent,
        transient_backup,
        permanent_agent,
        permanent_backup,
        base_agent,
    ],
    "verifier": [
        rate_limited,
        rate_backup,
        transient_agent,
        transient_backup,
        permanent_agent,
        permanent_backup,
        base_agent,
    ],
}
for role, agents in team_order.items():
    for agent in agents:
        call("POST", "/api/team/default", {"role": role, "agent_id": agent["id"]})

# 1. 排队位置、调序与暂停排队任务。
queue_id = create_project("Queue", "[parallel] 队列调序与暂停排队任务")
restrict_executor(queue_id, [base_agent["id"]])
confirm(queue_id)
detail = wait_for(
    queue_id,
    lambda item: len(item["tasks"]) == 3
    and any(
        task["key"] == "T1" and task["status"] == "running"
        for task in item["tasks"]
    ),
    "T1 开始执行",
)
t1 = task_by_key(detail, "T1")
t2 = task_by_key(detail, "T2")
t3 = task_by_key(detail, "T3")
assert t1["status"] == "running", t1
assert t2["status"] == "queued" and t3["status"] == "queued", (t2, t3)
assert t2["queue_position"] == 1, t2
assert t3["queue_position"] == 2, t3
assert t2["waiting_reason"], t2
assert t3["waiting_reason"], t3

reordered = call(
    "PATCH",
    f"/api/projects/{queue_id}/tasks/{t3['id']}/queue",
    {"direction": "up"},
)
assert task_by_key(reordered, "T3")["queue_position"] == 1, reordered["tasks"]
assert task_by_key(reordered, "T2")["queue_position"] == 2, reordered["tasks"]
assert any(
    event["kind"] == "task_queue_reordered" for event in reordered["events"]
), reordered["events"]

started = expect_error(
    "PATCH",
    f"/api/projects/{queue_id}/tasks/{t1['id']}/queue",
    {"direction": "up"},
)
assert started == 400, started

detail = wait_for(
    queue_id,
    lambda item: any(
        task["key"] == "T3"
        and any(
            job["task_id"] == task["id"]
            and job["role"] == "executor"
            and job["status"] in ("running", "succeeded")
            for job in item["jobs"]
        )
        for task in item["tasks"]
    ),
    "调序后 T3 先于 T2 开始执行",
)
t2 = task_by_key(detail, "T2")
assert t2["status"] == "queued", t2
assert not [
    job
    for job in detail["jobs"]
    if job["task_id"] == t2["id"] and job["role"] == "executor"
], "调序后 T3 应先于 T2 执行"
started_map = {task["id"]: task["key"] for task in detail["tasks"]}
started_keys = [
    started_map[job["task_id"]]
    for job in sorted(
        role_jobs(detail, "executor"), key=lambda job: job["created_at"]
    )
]
assert started_keys[:2] == ["T1", "T3"], started_keys

paused = call(
    "POST",
    f"/api/projects/{queue_id}/tasks/{t2['id']}/pause",
    {},
)
assert task_by_key(paused, "T2")["status"] == "paused", paused["tasks"]
assert task_by_key(paused, "T2")["queue_position"] is None, paused["tasks"]
assert task_by_key(paused, "T2")["waiting_reason"] == "任务已暂停", paused["tasks"]

detail = wait_for(
    queue_id,
    lambda item: any(
        task["key"] == "T3" and task["stage"] == "completed"
        for task in item["tasks"]
    ),
    "T3 执行并验收完成",
)
t2_exec_jobs = [
    job
    for job in detail["jobs"]
    if job["task_id"] == task_by_key(detail, "T2")["id"]
    and job["role"] == "executor"
]
assert not t2_exec_jobs, "暂停的任务不应产生执行作业"
assert detail["project"]["status"] != "blocked", detail["project"]

resumed = call(
    "POST",
    f"/api/projects/{queue_id}/tasks/{t2['id']}/resume",
    {},
)
assert task_by_key(resumed, "T2")["status"] == "queued", resumed["tasks"]
detail = wait_project_done(queue_id, "恢复暂停任务后项目完成")
executor_order = [
    job["task_id"]
    for job in sorted(
        role_jobs(detail, "executor"), key=lambda job: job["created_at"]
    )
]
key_by_task_id = {task["id"]: task["key"] for task in detail["tasks"]}
assert [key_by_task_id[task_id] for task_id in executor_order] == [
    "T1",
    "T3",
    "T2",
], executor_order

# 2. 优雅暂停运行中任务。
graceful_id = create_project("Graceful", "[single] 优雅暂停运行中任务")
restrict_executor(graceful_id, [base_agent["id"]])
confirm(graceful_id)
wait_for(
    graceful_id,
    lambda item: any(
        task["key"] == "T1" and task["status"] == "running"
        for task in item["tasks"]
    ),
    "T1 运行",
)
detail = call("GET", f"/api/projects/{graceful_id}")
graceful_t1 = task_by_key(detail, "T1")
queued_pause = call(
    "POST",
    f"/api/projects/{graceful_id}/tasks/{graceful_t1['id']}/pause",
    {},
)
assert task_by_key(queued_pause, "T1")["status"] == "pausing", queued_pause["tasks"]
detail = wait_for(
    graceful_id,
    lambda item: any(
        task["key"] == "T1" and task["status"] == "paused"
        for task in item["tasks"]
    )
    and any(
        task["key"] == "T1"
        and any(
            job["task_id"] == task["id"]
            and job["role"] == "executor"
            and job["status"] == "succeeded"
            for job in item["jobs"]
        )
        for task in item["tasks"]
    ),
    "优雅暂停完成",
)
jobs_at_pause = len(detail["jobs"])
time.sleep(1.2)
still_paused = call("GET", f"/api/projects/{graceful_id}")
assert len(still_paused["jobs"]) == jobs_at_pause, "优雅暂停后不应派发新作业"
assert not any(
    job["task_id"] == task_by_key(still_paused, "T1")["id"]
    and job["role"] == "verifier"
    for job in still_paused["jobs"]
), "暂停的任务不应进入验收"
call(
    "POST",
    f"/api/projects/{graceful_id}/tasks/{graceful_t1['id']}/resume",
    {},
)
wait_project_done(graceful_id, "优雅暂停恢复后完成")

# 3. 强制暂停在途进程，以及项目强制暂停。
force_id = create_project("Force", "[single] 强制暂停终止进程")
confirm(force_id)
detail = wait_for(
    force_id,
    lambda item: any(
        task["key"] == "T1"
        and task["status"] == "running"
        and any(
            job["task_id"] == task["id"]
            and job["role"] == "executor"
            and job["status"] == "running"
            for job in item["jobs"]
        )
        for task in item["tasks"]
    ),
    "CLI 执行作业开始",
)
force_t1 = task_by_key(detail, "T1")
force_job = next(
    job
    for job in detail["jobs"]
    if job["task_id"] == force_t1["id"]
    and job["role"] == "executor"
    and job["status"] == "running"
)
workspace = detail["project"]["workspace"]
pid_file = os.path.join(workspace, "jobs", force_job["id"], "pid")
deadline = time.time() + 10
while not os.path.isfile(pid_file) and time.time() < deadline:
    time.sleep(0.05)
assert os.path.isfile(pid_file), "在途进程没有写入 pid"
pid = int(open(pid_file, encoding="utf-8").read().split(":")[0])

started_at = time.time()
forced = call(
    "POST",
    f"/api/projects/{force_id}/tasks/{force_t1['id']}/pause",
    {"force": True},
)
assert time.time() - started_at < 3, "强制暂停应立即返回"
assert task_by_key(forced, "T1")["status"] == "paused", forced["tasks"]
assert task_by_key(forced, "T1")["queue_position"] is None, forced["tasks"]
time.sleep(0.5)
try:
    os.kill(pid, 0)
except ProcessLookupError:
    pass
else:
    raise AssertionError(f"强制暂停后进程 {pid} 仍在运行")
detail = call("GET", f"/api/projects/{force_id}")
assert all(
    job["status"] == "canceled"
    for job in detail["jobs"]
    if job["id"] == force_job["id"]
), detail["jobs"]
assert detail["project"]["status"] != "blocked", detail["project"]

call(
    "POST",
    f"/api/projects/{force_id}/tasks/{force_t1['id']}/resume",
    {},
)
wait_for(
    force_id,
    lambda item: any(
        task["key"] == "T1"
        and any(
            job["task_id"] == task["id"]
            and job["role"] == "executor"
            and job["status"] == "running"
            for job in item["jobs"]
        )
        for task in item["tasks"]
    ),
    "恢复后新的执行作业",
)
forced_project = call(
    "POST", f"/api/projects/{force_id}/pause", {"force": True}
)
assert forced_project["forced"] is True, forced_project
detail = call("GET", f"/api/projects/{force_id}")
assert detail["project"]["status"] == "paused", detail["project"]
assert task_by_key(detail, "T1")["status"] == "paused", detail["tasks"]
assert not [job for job in detail["jobs"] if job["status"] == "running"], detail["jobs"]
time.sleep(0.5)
detail = call("GET", f"/api/projects/{force_id}")
assert detail["project"]["status"] == "paused", detail["project"]
assert not [job for job in detail["jobs"] if job["status"] == "running"], detail["jobs"]
call("POST", f"/api/projects/{force_id}/archive", {})

# 4. 限流：立即冷却并换人，冷却时间取限流信息。
rate_id = create_project("RateLimit", "[ratelimit] 限流后换人")
restrict_executor(rate_id, [rate_limited["id"], rate_backup["id"]])
started = time.time()
confirm(rate_id)
detail = wait_project_done(rate_id, "限流场景完成")
rate_execs = [
    job
    for job in role_jobs(detail, "executor")
    if job["task_id"] == task_by_key(detail, "T1")["id"]
]
assert len(rate_execs) >= 2, rate_execs
assert rate_execs[0]["agent_id"] == rate_limited["id"], rate_execs
assert rate_execs[0]["status"] == "failed", rate_execs
assert rate_execs[0]["failure_kind"] == "rate_limit", rate_execs
assert "429" in (rate_execs[0]["error"] or ""), rate_execs
assert rate_execs[1]["agent_id"] == rate_backup["id"], rate_execs
assert rate_execs[1]["status"] == "succeeded", rate_execs
assert task_by_key(detail, "T1")["last_failure_kind"] == "rate_limit", detail["tasks"]

rate_agent = agent_by_id(detail, rate_limited["id"])
assert rate_agent["failure_kind"] == "rate_limit", rate_agent
assert rate_agent["cooldown_until"], rate_agent
cooldown = parse_time(rate_agent["cooldown_until"])
assert 800 <= cooldown - started <= 1000, rate_agent["cooldown_until"]
entry = pool_entry(detail, "executor", rate_limited["id"])
assert entry["cooldown_until"] == rate_agent["cooldown_until"], entry

# 5. 供应商波动：同一 Agent 有限重试后冷却并换人。
transient_id = create_project("Transient", "[transient] 供应商波动有限重试")
restrict_executor(
    transient_id, [transient_agent["id"], transient_backup["id"]]
)
started = time.time()
confirm(transient_id)
detail = wait_project_done(transient_id, "供应商波动场景完成")
transient_execs = [
    job
    for job in role_jobs(detail, "executor")
    if job["task_id"] == task_by_key(detail, "T1")["id"]
]
on_primary = [
    job for job in transient_execs if job["agent_id"] == transient_agent["id"]
]
assert len(on_primary) == 3, transient_execs
assert all(job["failure_kind"] == "transient" for job in on_primary), on_primary
switch_job = next(
    job
    for job in transient_execs
    if job["agent_id"] == transient_backup["id"]
)
assert switch_job["status"] == "succeeded", switch_job
transient_state = agent_by_id(detail, transient_agent["id"])
assert transient_state["failure_kind"] == "transient", transient_state
assert transient_state["cooldown_until"], transient_state
cooldown = parse_time(transient_state["cooldown_until"])
assert 1700 <= cooldown - started <= 1900, transient_state["cooldown_until"]
assert task_by_key(detail, "T1")["last_failure_kind"] == "transient", detail["tasks"]

# 6. 永久错误：直接在下一个 Agent 重试。
permanent_id = create_project("Permanent", "[permanent] 永久错误直接换人")
restrict_executor(
    permanent_id, [permanent_agent["id"], permanent_backup["id"]]
)
started = time.time()
confirm(permanent_id)
detail = wait_project_done(permanent_id, "永久错误场景完成")
permanent_execs = [
    job
    for job in role_jobs(detail, "executor")
    if job["task_id"] == task_by_key(detail, "T1")["id"]
]
on_permanent = [
    job for job in permanent_execs if job["agent_id"] == permanent_agent["id"]
]
assert len(on_permanent) == 1, permanent_execs
assert on_permanent[0]["failure_kind"] == "permanent", on_permanent
assert next(
    job
    for job in permanent_execs
    if job["agent_id"] == permanent_backup["id"]
)["status"] == "succeeded", permanent_execs
assert agent_by_id(detail, permanent_agent["id"])["failure_kind"] == "permanent"
assert task_by_key(detail, "T1")["last_failure_kind"] == "permanent", detail["tasks"]

# 7. 池耗尽：任务阻塞，保留人工裁决入口与冷却数据。
exhausted_agent_global = call(
    "POST",
    "/api/agents",
    {
        "name": "Rate Limited",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 1,
    },
)
for role in ("planner", "executor", "verifier"):
    call(
        "POST",
        "/api/team/default",
        {"role": role, "agent_id": exhausted_agent_global["id"]},
    )
exhausted_id = create_project("Exhausted", "[ratelimit] 池耗尽进入人工裁决")
pool = call("GET", f"/api/projects/{exhausted_id}/agents")
for group in pool["roles"]:
    for entry in group["entries"]:
        if entry["agent_id"] != exhausted_agent_global["id"] and entry["enabled"]:
            call(
                "PATCH",
                f"/api/projects/{exhausted_id}/agents/{group['role']}/{entry['agent_id']}",
                {"enabled": False},
            )
confirm(exhausted_id)
detail = wait_for(
    exhausted_id,
    lambda item: item["project"]["status"] == "blocked"
    and any(
        task["key"] == "T1" and task["status"] == "blocked"
        for task in item["tasks"]
    ),
    "池耗尽后任务阻塞",
)
t1 = task_by_key(detail, "T1")
assert t1["last_failure_kind"] == "rate_limit", t1
assert t1["waiting_reason"] == "等待人工裁决", t1
assert detail["project"]["status"] == "blocked", detail["project"]
exhausted_agent = agent_by_id(detail, exhausted_agent_global["id"])
assert exhausted_agent["failure_kind"] == "rate_limit", exhausted_agent
assert exhausted_agent["cooldown_until"], exhausted_agent
assert pool_entry(detail, "executor", exhausted_agent_global["id"])["cooldown_until"], detail
context = call(
    "GET", f"/api/projects/{exhausted_id}/tasks/{t1['id']}/arbitration"
)
assert "switch_agent" in context["actions"], context["actions"]
assert any(
    agent["cooldown_until"] for agent in context["agents"]
), context["agents"]

print("队列调序、任务暂停与失败退避测试通过")
PY

echo "==> 通过：$(basename "$0")"
