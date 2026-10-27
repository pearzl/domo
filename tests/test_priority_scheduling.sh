#!/usr/bin/env bash
# DOMO 严格优先级 Agent 调度测试。
#
# 覆盖范围：
#   1. 高优先级 Agent 可用但并发已满时，新任务保持 queued，等待原因包含该
#      Agent 名称与并发占用，且不会产生低优先级 Agent 的作业；
#   2. 禁用高优先级 Agent 后低优先级 Agent 接手；
#   3. 项目池中全部 Agent 不可用时，阻塞信息逐个列出 Agent 及原因；
#   4. 失败处理决定换 Agent 后，一次性 avoid 状态让下一次派发跳过失败
#      Agent，派发成功后清除，后续任务恢复使用高优先级 Agent。
#
# 使用内置 mock Agent 与脚本化 CLI Agent，不访问外网；临时数据全部放在
# ${TMPDIR:-/tmp}。退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-priority.$$"
PORT="${DOMO_PRIORITY_PORT:-$(python3 - <<'PY'
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

mkdir -p "$TMP_ROOT/repo" "$TMP_ROOT/agents"
git -C "$TMP_ROOT/repo" init -q -b main
git -C "$TMP_ROOT/repo" -c user.name=test -c user.email=test@example.com \
  commit --allow-empty -q -m "init"

cat >"$TMP_ROOT/agents/task_key.py" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    context = json.load(handle)
print((context.get("task") or {}).get("key") or "")
PY

cat >"$TMP_ROOT/agents/high.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
job_dir="$1"
key="$(python3 "$PRIORITY_TEST_ROOT/agents/task_key.py" "$job_dir/context.json")"
if [[ "${DOMO_JOB_ROLE:-}" != "executor" ]]; then
  printf '{"kind":"failed","reason":"高优先级 CLI Agent 只承担执行角色"}\n' >"$job_dir/result.json"
  exit 0
fi
# 占用并发槽位，让同角色后续任务只能排队等待。
sleep 4
mkdir -p "$PWD/domo_mock"
printf '%s\n' "$key" >"$PWD/domo_mock/$key.txt"
printf '{"kind":"done","summary":"高优先级 CLI 完成 %s"}\n' "$key" >"$job_dir/result.json"
SH

cat >"$TMP_ROOT/agents/flaky.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
job_dir="$1"
key="$(python3 "$PRIORITY_TEST_ROOT/agents/task_key.py" "$job_dir/context.json")"
if [[ "${DOMO_JOB_ROLE:-}" != "executor" ]]; then
  printf '{"kind":"failed","reason":"换人 Agent 只承担执行角色"}\n' >"$job_dir/result.json"
  exit 0
fi
if [[ "$key" == "T1" && ! -f "$PRIORITY_TEST_ROOT/flaky-failed-once" ]]; then
  : >"$PRIORITY_TEST_ROOT/flaky-failed-once"
  printf '{"kind":"failed","reason":"永久错误：Agent 配置无效"}\n' >"$job_dir/result.json"
  exit 0
fi
sleep 0.2
mkdir -p "$PWD/domo_mock"
printf '%s\n' "$key" >"$PWD/domo_mock/$key.txt"
printf '{"kind":"done","summary":"换人 Agent 完成 %s"}\n' "$key" >"$job_dir/result.json"
SH
chmod +x "$TMP_ROOT"/agents/*.sh

echo "==> 构建后端"
cargo build --manifest-path "$ROOT/server/Cargo.toml" >/dev/null

echo "==> 启动 DOMO (port $PORT)"
DOMO_DATA_ROOT="$TMP_ROOT/data" \
DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
DOMO_LOG_ROOT="$TMP_ROOT/log" \
DOMO_SERVER_PORT="$PORT" \
DOMO_MOCK_DELAY_MS=60 \
DOMO_MOCK_EXECUTOR_DELAY_MS=60 \
PRIORITY_TEST_ROOT="$TMP_ROOT" \
"$ROOT/server/target/debug/domo-server" >>"$TMP_ROOT/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 100); do
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
if ! curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null; then
  echo "等待服务启动超时" >&2
  tail -120 "$TMP_ROOT/server.log" >&2
  exit 1
fi

python3 - "$PORT" "$TMP_ROOT/repo" "$TMP_ROOT" <<'PY'
import json
import sys
import time
import urllib.error
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
repo = sys.argv[2]
tmp_root = sys.argv[3]

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
        raise RuntimeError(
            f"{method} {path} -> {error.code}: {error.read().decode()}"
        ) from error


def wait_for(project_id, predicate, label, timeout=120):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = call("GET", f"/api/projects/{project_id}")
        if predicate(last):
            return last
        time.sleep(0.05)
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


def role_jobs(detail, role):
    return [job for job in detail["jobs"] if job["role"] == role]


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
        "name": "Priority High",
        "kind": "cli",
        "roles": ["executor"],
        "max_concurrency": 1,
        "command": ["bash", f"{tmp_root}/agents/high.sh", "{job_dir}"],
    },
)
flaky = call(
    "POST",
    "/api/agents",
    {
        "name": "Priority Flaky",
        "kind": "cli",
        "roles": ["executor"],
        "max_concurrency": 1,
        "command": ["bash", f"{tmp_root}/agents/flaky.sh", "{job_dir}"],
    },
)
low = call(
    "POST",
    "/api/agents",
    {
        "name": "Priority Low",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 1,
    },
)
for role in ("planner", "verifier"):
    call("POST", "/api/team/default", {"role": role, "agent_id": low["id"]})
for agent in (high, flaky, low):
    call("POST", "/api/team/default", {"role": "executor", "agent_id": agent["id"]})

# 1. 高优先级可用但并发已满：任务排队等待，低优先级不接单。
busy_id = create_project("PriorityBusy", "并发占满时严格等待高优先级 [parallel]")
call(
    "PATCH",
    f"/api/projects/{busy_id}/agents/executor/{flaky['id']}",
    {"enabled": False},
)
confirm(busy_id)
wait_for(
    busy_id,
    lambda detail: any(
        job["agent_id"] == high["id"]
        and job["role"] == "executor"
        and job["status"] == "running"
        for job in detail["jobs"]
    ),
    "高优先级执行作业",
)
waiting = wait_for(
    busy_id,
    lambda detail: any(
        task["status"] == "queued"
        and task["waiting_reason"]
        and "Priority High" in task["waiting_reason"]
        and "并发 1/1" in task["waiting_reason"]
        for task in detail["tasks"]
    ),
    "高优先级并发占满时的等待原因",
    30,
)
waited = next(
    task
    for task in waiting["tasks"]
    if task["waiting_reason"]
    and "Priority High" in task["waiting_reason"]
    and "并发 1/1" in task["waiting_reason"]
)
assert waited["status"] == "queued", waited
assert not [
    job
    for job in role_jobs(waiting, "executor")
    if job["agent_id"] == low["id"]
], "高优先级忙碌期间不应产生低优先级执行作业"
assert "Priority High" in (waiting["project"]["blocked_reason"] or ""), waiting["project"]
assert "并发 1/1" in (waiting["project"]["blocked_reason"] or ""), waiting["project"]

# 2. 禁用高优先级后，低优先级接手。
call("PATCH", f"/api/agents/{high['id']}", {"enabled": False})
handover = wait_for(
    busy_id,
    lambda detail: any(
        job["agent_id"] == low["id"] and job["role"] == "executor"
        for job in detail["jobs"]
    ),
    "低优先级接手执行",
    30,
)
assert any(
    job["agent_id"] == low["id"] and job["role"] == "executor"
    for job in handover["jobs"]
), handover["jobs"]
busy = wait_completed(busy_id, "并发项目完成")
assert any(
    job["agent_id"] == low["id"] and job["role"] == "executor"
    for job in busy["jobs"]
), busy["jobs"]

# 3. 项目池全部不可用时，阻塞原因逐个列出 Agent。
blocked_id = create_project("PriorityBlocked", "全部执行 Agent 不可用")
for agent in (high, flaky, low):
    call(
        "PATCH",
        f"/api/projects/{blocked_id}/agents/executor/{agent['id']}",
        {"enabled": False},
    )
confirm(blocked_id)
blocked = wait_for(
    blocked_id,
    lambda detail: detail["project"]["blocked_reason"]
    and all(name in detail["project"]["blocked_reason"] for name in ("Priority High", "Priority Flaky", "Priority Low"))
    and "项目池中已禁用" in detail["project"]["blocked_reason"],
    "全部 Agent 不可用的阻塞原因",
    30,
)
reason = blocked["project"]["blocked_reason"]
for name in ("Priority High", "Priority Flaky", "Priority Low"):
    assert f"「{name}」（项目池中已禁用）" in reason, reason
assert task_by_key(blocked, "T1")["status"] == "queued", task_by_key(blocked, "T1")

# 4. 失败换人后一次性跳过失败 Agent，派发成功后恢复高优先级选择。
switch_id = create_project("PrioritySwitch", "失败换人后恢复高优先级")
call(
    "PATCH",
    f"/api/projects/{switch_id}/agents/executor/{high['id']}",
    {"enabled": False},
)
confirm(switch_id)
switched = wait_for(
    switch_id,
    lambda detail: any(
        job["agent_id"] == low["id"]
        and job["role"] == "executor"
        and job["task_id"] == task_by_key(detail, "T1")["id"]
        for job in detail["jobs"]
    ),
    "失败后低优先级接手 T1",
    60,
)
failed = next(
    job
    for job in switched["jobs"]
    if job["agent_id"] == flaky["id"] and job["role"] == "executor"
)
assert failed["status"] in ("failed", "succeeded"), failed
assert any(
    job["agent_id"] == low["id"]
    and job["role"] == "executor"
    and job["task_id"] == task_by_key(switched, "T1")["id"]
    for job in switched["jobs"]
), switched["jobs"]
restored = wait_for(
    switch_id,
    lambda detail: any(
        job["agent_id"] == flaky["id"]
        and job["role"] == "executor"
        and job["task_id"] == task_by_key(detail, "T2")["id"]
        for job in detail["jobs"]
    ),
    "一次性 avoid 清除后恢复高优先级",
    60,
)
assert any(
    job["agent_id"] == flaky["id"]
    and job["role"] == "executor"
    and job["task_id"] == task_by_key(restored, "T2")["id"]
    for job in restored["jobs"]
), restored["jobs"]
wait_completed(switch_id, "换人项目完成")

print("严格优先级调度断言全部通过")
PY

echo "==> 通过：$(basename "$0")"
