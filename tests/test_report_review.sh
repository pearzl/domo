#!/usr/bin/env bash
# DOMO 项目总结报告多轮评议冒烟测试。
#
# 覆盖范围：
#   1. 人工对报告提交意见（评议记录持久化为 messages，事件与作业提示词可回溯）；
#   2. 报告被拒绝后规划者依据评议意见重写报告，多轮拒绝/重写可重复；
#   3. 处置「要求重新规划」：项目回到规划阶段并生成规划作业；
#   4. 处置「增加新任务」：复用人工加任务端点，项目回到执行阶段、新任务按 DAG 调度、原报告作废；
#   5. 处置结果（report_rejected / replan_requested / task_created / project_reopened / project_completed）在事件流可查询。
#
# 约定：使用内置 mock Agent，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}；
# 脚本退出码 0 表示全部通过，结束时清理自己启动的进程与临时文件。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-report-review.$$"
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
DOMO_MOCK_DELAY_MS=80 \
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
    raise AssertionError(f"等待 {label} 超时；最后状态：{last['project']} {[t['key'] for t in last['tasks']]}")


def project_report_ready(project_id, label):
    return wait_for(
        project_id,
        lambda d: d["project"]["stage"] == "summarizing"
        and d["project"]["status"] == "blocked"
        and bool(d["project"]["report"]),
        label,
    )


def all_events(project_id):
    return call("GET", f"/api/events?project_id={project_id}")


agents = call("GET", "/api/agents")
for agent in agents:
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
mock = call(
    "POST",
    "/api/agents",
    {
        "name": "Report Review Mock",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "priority": 0,
        "max_concurrency": 3,
    },
)
assert mock["enabled"], mock
# 默认团队随时快照，新项目从此继承；把 mock 配置成默认团队三个角色。
for role in ["planner", "executor", "verifier"]:
    call("POST", "/api/team/default", {"role": role, "agent_id": mock["id"]})


def make_project(name, goal):
    project = call(
        "POST",
        "/api/projects",
        {"name": name, "goal": goal, "repo_path": repo, "base_branch": "main"},
    )["project"]
    call("POST", f"/api/projects/{project['id']}/confirm", {})
    return project["id"]


# ---- 阶段 A：多轮评议 + 重写 -------------------------------------------------
pid_a = make_project("ReviewA", "[single] 生成单任务 mock 文件")
detail = project_report_ready(pid_a, "首轮报告")
assert detail["project"]["report"].strip(), detail["project"]

# 第 1 轮：先只记录意见（讨论，不触发处置）
call("POST", f"/api/projects/{pid_a}/report", {
    "action": "comment",
    "feedback": "请补充 ReqB 的实现细节说明",
})
detail = call("GET", f"/api/projects/{pid_a}")
reviews = [m for m in detail["messages"] if m["role"] == "review"]
assert len(reviews) == 1 and "ReqB" in reviews[0]["content"], detail["messages"]
assert any(e["kind"] == "report_opinion" and "ReqB" in e["message"]
           for e in detail["events"]), detail["events"]

# 第 1 次拒绝：记录意见并触发重写
call("POST", f"/api/projects/{pid_a}/report", {
    "action": "reject",
    "feedback": "缺少 ReqA 的实现细节，报告不具体",
})
detail = project_report_ready(pid_a, "第 2 轮报告")
# 新版报告生成后，上一轮意见随 report_feedback 清空，历史意见留在评议记录里。
assert detail["project"]["report_feedback"] is None, detail["project"]
assert detail["project"]["report"].count("已回应第") >= 1, detail["project"]["report"]
assert "缺少 ReqA 的实现细节" in detail["project"]["report"], detail["project"]["report"]
assert detail["project"]["status"] == "blocked", detail["project"]

summary_jobs = [j for j in detail["jobs"] if j["role"] == "summary"]
assert len(summary_jobs) >= 2, summary_jobs
rewrite_jobs = sorted(summary_jobs, key=lambda j: j["created_at"])
rewrite = call("GET", f"/api/jobs/{rewrite_jobs[-1]['id']}")
assert "评议记录" in rewrite["prompt"], rewrite["prompt"]
assert "缺少 ReqA 的实现细节" in rewrite["prompt"], rewrite["prompt"]
assert "ReqB" in rewrite["prompt"], rewrite["prompt"]

# 第 2 次拒绝：多轮可重复
call("POST", f"/api/projects/{pid_a}/report", {
    "action": "reject",
    "feedback": "再补充验证情况说明",
})
detail = project_report_ready(pid_a, "第 3 轮报告")
reviews = [m for m in detail["messages"] if m["role"] == "review"]
assert len(reviews) == 3, detail["messages"]
summary_jobs = [j for j in detail["jobs"] if j["role"] == "summary"]
assert len(summary_jobs) >= 3, summary_jobs

events = all_events(pid_a)
kinds = [e["kind"] for e in events]
assert kinds.count("report_opinion") >= 3, kinds
assert kinds.count("report_rejected") >= 2, kinds
assert kinds.count("report_ready") >= 3, kinds

# 人工再次确认
call("POST", f"/api/projects/{pid_a}/report", {"action": "accept"})
detail = call("GET", f"/api/projects/{pid_a}")
assert detail["project"]["stage"] == "completed", detail["project"]
assert detail["project"]["status"] is None, detail["project"]

# ---- 阶段 B：处置「要求重新规划」---------------------------------------------
pid_b = make_project("ReviewB", "[single] 生成单任务 mock 文件")
project_report_ready(pid_b, "阶段 B 报告")
call("POST", f"/api/projects/{pid_b}/report", {
    "action": "replan",
    "feedback": "范围太小，需要纳入对 ReqC 的支持，请重新规划",
})
detail = wait_for(
    pid_b,
    lambda d: d["project"]["stage"] == "planning" and d["project"]["status"] == "running",
    "回到规划阶段",
    45,
)
assert detail["project"]["report"] is None, detail["project"]
events = all_events(pid_b)
kinds = [e["kind"] for e in events]
assert "replan_requested" in kinds and "plan_superseded" in kinds, kinds
replan_event = next(e for e in events if e["kind"] == "replan_requested")
assert "人工" in replan_event["message"] and "ReqC" in replan_event["message"], replan_event
goal_jobs = [j for j in detail["jobs"] if j["role"] == "goal"]
assert goal_jobs, detail["jobs"]
plan_job = call("GET", f"/api/jobs/{sorted(goal_jobs, key=lambda j: j['created_at'])[-1]['id']}")
assert "ReqC" in plan_job["prompt"], plan_job["prompt"]

# 重规划按现有路径继续走完：新任务执行并再次总结
detail = wait_for(
    pid_b,
    lambda d: d["project"]["stage"] == "summarizing"
    and bool(d["project"]["report"]),
    "重规划后的报告",
)
assert detail["project"]["replan_reason"].endswith("请重新规划"), detail["project"]
call("POST", f"/api/projects/{pid_b}/report", {"action": "accept"})
assert call("GET", f"/api/projects/{pid_b}")["project"]["stage"] == "completed"

# ---- 阶段 C：处置「增加新任务」（复用人工加任务端点）--------------------------
pid_c = make_project("ReviewC", "[single] 生成单任务 mock 文件")
project_report_ready(pid_c, "阶段 C 报告")
old_report = call("GET", f"/api/projects/{pid_c}")["project"]["report"]
assert old_report, old_report

call("POST", f"/api/projects/{pid_c}/tasks", {
    "key": "T2",
    "title": "补充产物核查文件",
    "description": "在 domo_mock/T2.txt 写入核查记录，并确认 domo_mock/T1.txt 存在。",
    "acceptance": ["domo_mock/T2.txt 存在且包含 T2"],
    "deps": ["T1"],
})
detail = call("GET", f"/api/projects/{pid_c}")
assert detail["project"]["stage"] == "executing", detail["project"]
assert detail["project"]["report"] is None, detail["project"]
tasks = {task["key"]: task for task in detail["tasks"]}
assert tasks["T2"]["deps"] == ["T1"], tasks["T2"]
assert tasks["T2"]["stage"] == "pending", tasks["T2"]

events = all_events(pid_c)
kinds = [e["kind"] for e in events]
assert "task_created" in kinds and "project_reopened" in kinds, kinds
reopened = next(e for e in events if e["kind"] == "project_reopened")
assert "作废" in reopened["message"], reopened

detail = wait_for(
    pid_c,
    lambda d: d["project"]["stage"] == "summarizing" and bool(d["project"]["report"]),
    "加任务后的新报告",
)
task_t2 = next(task for task in detail["tasks"] if task["key"] == "T2")
assert task_t2["stage"], task_t2
call("POST", f"/api/projects/{pid_c}/report", {"action": "accept"})
assert call("GET", f"/api/projects/{pid_c}")["project"]["stage"] == "completed"

# ---- 反合法输入 --------------------------------------------------------------
errors = []
try:
    call("POST", f"/api/projects/{pid_c}/report", {"action": "reject", "feedback": "  "})
except RuntimeError as error:
    errors.append(str(error))
assert any("填写意见" in text or "等待验收" in text for text in errors), errors

print("报告多轮评议冒烟测试通过")
print(f"  评议记录: {len(reviews)} 条")
PY

echo "==> 通过：$(basename "$0")"
