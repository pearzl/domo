#!/usr/bin/env bash
# DOMO 项目级作业（规划者）失败降级测试。
#
# 覆盖范围：
#   1. 项目分析时高优先级规划 Agent 被限流：Agent 进入冷却，项目不阻塞，
#      由下一优先级的规划 Agent 自动接手并给出方案；
#   2. 总结时高优先级规划 Agent 被限流：同样自动切换，总结作业由下一
#      优先级的规划 Agent 完成；
#   3. 项目池里只剩一个规划 Agent 且被限流：项目阻塞并记录限流原因，调度器
#      不会自行重派；人工重试失败作业会重新派给该 Agent，并在派发成功后
#      清除它的冷却。
#
# 使用内置 mock Agent 与脚本化 CLI Agent，不访问外网；临时数据全部放在
# ${TMPDIR:-/tmp}。退出码 0 表示全部通过。

set -euo pipefail
# Git for Windows defaults to the GBK code page; keep Python on UTF-8.
export PYTHONUTF8=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-project-failure.$$"
PORT="${DOMO_PROJECT_FAILURE_PORT:-$(python3 - <<'PY'
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

cat >"$TMP_ROOT/agents/planner.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
job_dir="$1"
role="${DOMO_JOB_ROLE:-}"
case "$role" in
  analysis)
    printf '{"kind":"plan","summary":"脚本化规划者给出方案","assumptions":[],"acceptance":["产物符合目标描述"],"tasks":[{"key":"T1","title":"写入基础文件","description":"在 domo_mock/T1.txt 写入 T1。","acceptance":["domo_mock/T1.txt 存在"],"deps":[]}],"skip_confirmation":true}\n' >"$job_dir/result.json"
    ;;
  summary)
    printf '{"kind":"report","summary":"脚本化报告：全部任务已完成。"}\n' >"$job_dir/result.json"
    ;;
  *)
    printf '{"kind":"failed","reason":"脚本化规划 Agent 只承担项目分析"}\n' >"$job_dir/result.json"
    ;;
esac
SH
chmod +x "$TMP_ROOT/agents/planner.sh"

echo "==> 构建后端"
cargo build --manifest-path "$ROOT/server/Cargo.toml" >/dev/null

echo "==> 启动 DOMO (port $PORT)"
# 高优先级规划 Agent 每次被调用都返回 429，重试窗口足够长以覆盖整个用例。
DOMO_DATA_ROOT="$TMP_ROOT/data" \
DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
DOMO_LOG_ROOT="$TMP_ROOT/log" \
DOMO_SERVER_PORT="$PORT" \
DOMO_MOCK_DELAY_MS=60 \
DOMO_MOCK_EXECUTOR_DELAY_MS=60 \
DOMO_MOCK_PLANNER_RATE_LIMIT=1 \
DOMO_MOCK_PLANNER_RATE_LIMIT_AGENT="Flaky Planner" \
DOMO_MOCK_PLANNER_RATE_LIMIT_RETRY=4 \
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


def wait_for(project_id, predicate, label, timeout=60):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = call("GET", f"/api/projects/{project_id}")
        if predicate(last):
            return last
        time.sleep(0.05)
    raise AssertionError(
        f"等待 {label} 超时；项目：{last['project']['stage']}/{last['project']['status']} "
        f"{last['project']['blocked_reason']}；"
        f"作业：{[(j['role'], j['agent_name'], j['status'], j['failure_kind']) for j in last['jobs']]}"
    )


def agent_id_by_name(name):
    return next(agent for agent in call("GET", "/api/agents") if agent["name"] == name)["id"]


def planner_entries(project_id):
    pool = call("GET", f"/api/projects/{project_id}/agents")
    return next(role["entries"] for role in pool["roles"] if role["role"] == "planner")


def create_project(name, goal):
    return call(
        "POST",
        "/api/projects",
        {"name": name, "goal": goal, "repo_path": repo, "base_branch": "main"},
    )["project"]["id"]


def submit(project_id, goal):
    call(
        "POST",
        f"/api/projects/{project_id}/analysis/messages",
        {"message": goal, "confirm_mode": "auto"},
    )


def role_jobs(detail, role):
    return [job for job in detail["jobs"] if job["role"] == role]


for agent in call("GET", "/api/agents"):
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})

flaky = call(
    "POST",
    "/api/agents",
    {
        "name": "Flaky Planner",
        "kind": "mock",
        "model": "mock",
        "roles": ["planner"],
        "max_concurrency": 1,
    },
)["id"]
backup = call(
    "POST",
    "/api/agents",
    {
        "name": "Backup Planner",
        "kind": "cli",
        "model": "scripted",
        "roles": ["planner"],
        "command": ["bash", f"{tmp_root}/agents/planner.sh", "{job_dir}"],
        "max_concurrency": 1,
    },
)["id"]
worker = call(
    "POST",
    "/api/agents",
    {
        "name": "Mock Worker",
        "kind": "mock",
        "model": "mock",
        "roles": ["executor", "verifier"],
        "max_concurrency": 2,
    },
)["id"]

# 默认团队决定新项目的项目 Agent 池。Steady Planner 是普通 mock，用于
# 让不涉及限流的项目正常完成；Flaky Planner 会被限流，Backup Planner 是
# 它的降级备选。
steady = call(
    "POST",
    "/api/agents",
    {
        "name": "Steady Planner",
        "kind": "mock",
        "model": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 2,
    },
)["id"]

# 场景一：项目分析被限流，自动切换到下一优先级的规划 Agent。
project_id = create_project("Planner Limit", "写入 domo_mock 产物")
call("PATCH", f"/api/projects/{project_id}/agents/planner/{steady}", {"enabled": False})
entries = [
    entry["agent_id"]
    for entry in planner_entries(project_id)
    if entry["enabled"]
]
assert entries == [flaky, backup], entries

submit(project_id, "写入 domo_mock 产物")
detail = wait_for(
    project_id,
    lambda detail: any(
        job["role"] == "analysis"
        and job["agent_id"] == backup
        and job["status"] == "succeeded"
        for job in detail["jobs"]
    ),
    "项目分析切换到下一优先级",
)
jobs = role_jobs(detail, "analysis")
assert [(job["agent_id"], job["status"]) for job in jobs] == [
    (flaky, "failed"),
    (backup, "succeeded"),
], jobs
assert jobs[0]["failure_kind"] == "rate_limit", jobs
assert "429" in (jobs[0]["error"] or ""), jobs
assert detail["project"]["status"] != "blocked", detail["project"]
assert detail["tasks"], detail

flaky_row = next(
    agent for agent in call("GET", "/api/agents") if agent["id"] == flaky
)
assert flaky_row["cooldown_until"], flaky_row
assert flaky_row["failure_kind"] == "rate_limit", flaky_row

messages = [(event["kind"], event["message"]) for event in detail["events"]]
assert (
    "project_retry",
    "项目分析失败（限流），已切换 Agent 重试："
    "HTTP 429 too many requests; retry after 4 seconds",
) in messages, messages
assert any(kind == "job_queued" and "Backup Planner" in message for kind, message in messages), messages

wait_for(
    project_id,
    lambda detail: detail["project"]["stage"] == "summarizing"
    and detail["project"].get("report"),
    "项目产出报告",
)

# 场景二：项目池里唯一的规划 Agent 被限流，项目阻塞并等待人工处理。
call("PATCH", f"/api/team/default/planner/{backup}", {"enabled": False})
project_id = create_project("Only Planner", "写入 domo_mock 产物")
# 项目池只剩 Flaky Planner 可用，它在下一次项目作业时被限流。
call("PATCH", f"/api/projects/{project_id}/agents/planner/{steady}", {"enabled": False})
submit(project_id, "写入 domo_mock 产物")
# 派发后发生限流：Agent 进入冷却，且没有其他可降级的规划 Agent。
detail = wait_for(
    project_id,
    lambda detail: detail["project"]["status"] == "blocked"
    and "429" in (detail["project"]["blocked_reason"] or "")
    and not any(job["status"] == "running" for job in detail["jobs"]),
    "唯一规划 Agent 被限流后阻塞",
)
assert len(role_jobs(detail, "analysis")) == 1, role_jobs(detail, "analysis")
assert role_jobs(detail, "analysis")[0]["failure_kind"] == "rate_limit", role_jobs(
    detail, "analysis"
)
flaky_row = next(agent for agent in call("GET", "/api/agents") if agent["id"] == flaky)
assert flaky_row["cooldown_until"], flaky_row

# 限流期间调度器不自行重派，项目保持阻塞。
time.sleep(1.5)
detail = call("GET", f"/api/projects/{project_id}")
assert detail["project"]["status"] == "blocked", detail["project"]
assert len(role_jobs(detail, "analysis")) == 1, role_jobs(detail, "analysis")

# 人工重试把失败作业重新排队，只有该作业的目标 Agent 获得一次跳过冷却的派发机会。
call("PATCH", f"/api/agents/{flaky}", {"roles": ["planner", "executor", "verifier"]})
call("PATCH", f"/api/agents/{flaky}", {"enabled": True})
failed_job = role_jobs(detail, "analysis")[0]
call("POST", f"/api/jobs/{failed_job['id']}/retry", {})
detail = wait_for(
    project_id,
    lambda detail: detail["project"]["status"] == "blocked"
    and "429" in (detail["project"]["blocked_reason"] or "")
    and len(role_jobs(detail, "analysis")) >= 2
    and not any(job["status"] == "running" for job in detail["jobs"]),
    "人工重试后重新派发并再次限流",
)
resent = role_jobs(detail, "analysis")[1]
assert resent["agent_id"] == flaky, resent
assert resent["failure_kind"] == "rate_limit", resent

print("全部场景通过")
PY

echo "==> 通过：$(basename "$0")"
