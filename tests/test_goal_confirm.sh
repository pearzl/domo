#!/usr/bin/env bash
# 目标确认阶段冒烟测试。
#
# 覆盖范围：
#   1. 项目创建后等待输入目标，不派发作业；
#   2. 人工确认模式下，规划者多轮提问、给出目标理解、人工确认后进入规划；
#   3. 自动确认模式下，规划者可以直接跳过人工确认；
#   4. 直接完成模式下，人工确认后才修改仓库，并生成等待验收的报告；
#   5. 自动确认的直接完成不等待人工确认；
#   6. 人工确认模式忽略规划者的跳过请求；
#   7. 作业运行输出可通过接口读取。
#
# 约定：使用内置 mock Agent，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}。
# 退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-goal-confirm.$$"
PORT="${DOMO_GOAL_CONFIRM_PORT:-$(python3 - <<'PY'
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
DOMO_MOCK_DELAY_MS=40 \
"$ROOT/server/target/debug/domo-server" >"$TMP_ROOT/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 120); do
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
        time.sleep(0.1)
    raise AssertionError(f"等待 {label} 超时；最后状态：{last}")


def create_project(name):
    return call(
        "POST",
        "/api/projects",
        {"name": name, "repo_path": repo, "base_branch": "main"},
    )["project"]


def submit_goal(project_id, goal, mode="manual"):
    return call(
        "POST",
        f"/api/projects/{project_id}/goal/messages",
        {"message": goal, "goal_confirm_mode": mode},
    )


def wait_proposal(project_id, kind):
    return wait_for(
        project_id,
        lambda detail: detail.get("goal_proposal")
        and detail["goal_proposal"]["kind"] == kind
        and detail["project"]["stage"] == "goal_confirm"
        and detail["project"]["status"] == "blocked",
        f"{kind} 目标提议",
    )


def wait_report(project_id):
    return wait_for(
        project_id,
        lambda detail: detail["project"]["stage"] == "summarizing"
        and bool(detail["project"]["report"]),
        "项目报告",
    )


for agent in call("GET", "/api/agents"):
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
mock = call(
    "POST",
    "/api/agents",
    {
        "name": "Goal Mock",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 2,
    },
)
for role in ("planner", "executor", "verifier"):
    call("POST", "/api/team/default", {"role": role, "agent_id": mock["id"]})

# 1. 创建后没有目标，也没有作业。
empty = create_project("Empty")
assert empty["goal"] == "", empty
empty_detail = call("GET", f"/api/projects/{empty['id']}")
assert empty_detail["project"]["stage"] == "goal_confirm", empty_detail["project"]
assert empty_detail["project"]["status"] == "blocked", empty_detail["project"]
assert empty_detail["jobs"] == [], empty_detail["jobs"]

# 2. 人工确认：多轮提问 -> 目标理解 -> 确认 -> 规划。
questions = create_project("Questions")
submit_goal(questions["id"], "[ask] 先讨论再规划")
asked = wait_for(
    questions["id"],
    lambda detail: any(message["role"] == "planner" for message in detail["messages"])
    and detail["goal_proposal"] is None
    and detail["project"]["stage"] == "goal_confirm",
    "规划者提问",
)
assert "哪个使用场景" in asked["messages"][-1]["content"], asked["messages"]
call(
    "POST",
    f"/api/projects/{questions['id']}/goal/messages",
    {"message": "没有偏好，请直接规划。"},
)
ready = wait_proposal(questions["id"], "ready")
assert ready["project"]["status"] == "blocked", ready["project"]
call("POST", f"/api/projects/{questions['id']}/goal/confirm", {})
planned = wait_for(
    questions["id"],
    lambda detail: detail["project"]["stage"] in ("executing", "summarizing"),
    "确认后进入规划/执行",
)
assert planned["project"]["stage"] in ("executing", "summarizing"), planned["project"]
assert any(job["role"] == "confirm" for job in planned["jobs"]), planned["jobs"]
assert any(job["role"] == "goal" for job in planned["jobs"]), planned["jobs"]
call("POST", f"/api/projects/{questions['id']}/archive", {})

# 3. 自动确认：规划者直接进入规划，不等待人工确认。
auto = create_project("AutoPlan")
submit_goal(auto["id"], "[skip] 自动确认普通目标", "auto")
auto_detail = wait_for(
    auto["id"],
    lambda detail: detail["project"]["stage"] in ("planning", "executing", "summarizing"),
    "自动确认后进入规划",
)
assert auto_detail["project"]["goal_confirm_mode"] == "auto", auto_detail["project"]
assert any(job["role"] == "confirm" for job in auto_detail["jobs"]), auto_detail["jobs"]
call("POST", f"/api/projects/{auto['id']}/archive", {})

# 4. 人工确认的直接完成：确认前不修改仓库，确认后提交并生成报告。
direct = create_project("Direct")
submit_goal(direct["id"], "[direct] 直接完成一个简单目标")
proposal = wait_proposal(direct["id"], "direct")
branch = proposal["project"]["branch"]
try:
    subprocess.check_output(
        ["git", "-C", repo, "show", f"{branch}:domo_mock/direct.txt"],
        stderr=subprocess.DEVNULL,
    )
    raise AssertionError("人工确认前仓库不应出现产物")
except subprocess.CalledProcessError:
    pass
call("POST", f"/api/projects/{direct['id']}/goal/confirm", {})
direct_done = wait_report(direct["id"])
assert direct_done["project"]["completion_mode"] == "direct", direct_done["project"]
assert direct_done["tasks"] == [], direct_done["tasks"]
assert any(job["role"] == "direct" for job in direct_done["jobs"]), direct_done["jobs"]
content = subprocess.check_output(
    ["git", "-C", repo, "show", f"{branch}:domo_mock/direct.txt"], text=True
)
assert "mock direct completion" in content, content

# 4.1 报告被拒绝后重新直接完成。
call(
    "POST",
    f"/api/projects/{direct['id']}/report",
    {"action": "reject", "feedback": "请重新执行直接完成。"},
)
redone = wait_for(
    direct["id"],
    lambda detail: detail["project"]["report"]
    and sum(1 for job in detail["jobs"] if job["role"] == "direct") >= 2,
    "直接完成重做",
)
assert redone["project"]["completion_mode"] == "direct", redone["project"]
call("POST", f"/api/projects/{direct['id']}/report", {"action": "accept"})

# 5. 自动确认的直接完成：不需要人工确认。
auto_direct = create_project("AutoDirect")
submit_goal(auto_direct["id"], "[direct] 自动直接完成", "auto")
auto_direct_done = wait_report(auto_direct["id"])
assert any(job["role"] == "direct" for job in auto_direct_done["jobs"]), auto_direct_done["jobs"]
assert auto_direct_done["project"]["goal_confirm_mode"] == "auto", auto_direct_done["project"]

# 6. 人工确认模式忽略规划者的跳过请求。
forced = create_project("ForcedManual")
submit_goal(forced["id"], "[skipforce] 人工确认不能被跳过")
forced_ready = wait_proposal(forced["id"], "ready")
assert forced_ready["goal_proposal"]["skip_confirmation"] is True, forced_ready["goal_proposal"]
assert forced_ready["project"]["stage"] == "goal_confirm", forced_ready["project"]
assert forced_ready["project"]["status"] == "blocked", forced_ready["project"]
call("POST", f"/api/projects/{forced['id']}/goal/confirm", {})
wait_for(
    forced["id"],
    lambda detail: detail["project"]["stage"] in ("executing", "summarizing"),
    "人工确认后进入规划",
)

# 7. 作业输出可通过接口读取。
output = call("GET", f"/api/jobs/{forced_ready['jobs'][0]['id']}/output")
assert "mock job" in output["stdout"], output
assert output["running"] is False, output

print("目标确认阶段测试通过")
PY

echo "==> 通过：$(basename "$0")"
