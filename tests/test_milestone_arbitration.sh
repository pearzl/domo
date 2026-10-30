#!/usr/bin/env bash
# DOMO 里程碑人工验收与人工裁决测试。
#
# 覆盖范围：
#   1. 任务可标记/取消里程碑，已完成任务拒绝修改标记；
#   2. 里程碑任务验收通过后不合并，任务与项目进入阻塞并注明等待人工验收；
#   3. 人工拒绝携带理由和指导退回执行，下一轮 prompt 包含拒绝意见；
#   4. 人工拒绝不增加自动验收失败计数；再次验收通过后人工 accept 才合并完成任务；
#   5. 重复人工验收不会重复合并；
#   6. 人工裁决 switch_agent 强制指定 Agent 执行下一轮作业，事件可审计，
#      作业开始后强制设置失效，后续调度恢复项目池优先级；
#   7. 重复人工裁决不会重复派发作业；
#   8. 暂停中的项目只记录裁决不派发作业，恢复后生效；归档项目拒绝裁决。
#   9. 里程碑人工拒绝可选择重新规划：项目回到规划阶段并生成规划者作业，
#      指导意见进入规划 prompt，审计事件记录人工裁决，旧任务被取消。
#
# 使用内置 mock Agent，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}。
# 退出码 0 表示全部通过。

set -euo pipefail
# Git for Windows defaults to the GBK code page; keep Python on UTF-8.
export PYTHONUTF8=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-milestone.$$"
PORT="${DOMO_MILESTONE_PORT:-$(python3 - <<'PY'
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
DOMO_MOCK_EXECUTOR_DELAY_MS=350 \
DOMO_MOCK_FAIL_ALWAYS=1 \
DOMO_MOCK_FAIL_ALWAYS_GOAL='[switch]' \
DOMO_MOCK_FAIL_ALWAYS_KEY=T1 \
DOMO_MOCK_FAIL_ALWAYS_AGENT='Switch Default' \
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
        raise RuntimeError(f"{method} {path} -> {error.code}: {error.read().decode()}") from error


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
        return error.code, error.read().decode()


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
        f"{[(t['key'], t['stage'], t['status'], t['human_gate']) for t in last['tasks']]}"
    )


def wait_job_prompt(job_id, timeout=30):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = call("GET", f"/api/jobs/{job_id}")
        if last["prompt"]:
            return last
        time.sleep(0.1)
    raise AssertionError(f"等待作业 prompt 超时：{last}")


def new_project(name, goal):
    project = call(
        "POST",
        "/api/projects",
        {"name": name, "goal": goal, "repo_path": repo, "base_branch": "main"},
    )["project"]
    call("POST", f"/api/projects/{project['id']}/confirm", {})
    return project["id"]


def task_by_key(detail, key):
    return next(task for task in detail["tasks"] if task["key"] == key)


def goal_jobs(detail):
    return [job for job in detail["jobs"] if job["role"] == "analysis"]


def branch_has(path):
    completed = subprocess.run(
        ["git", "-C", repo, "cat-file", "-e", path],
        capture_output=True,
        text=True,
    )
    return completed.returncode == 0


def task_merge_commits(branch, key):
    log = subprocess.check_output(
        ["git", "-C", repo, "log", "--format=%s", branch], text=True
    ).splitlines()
    return [line for line in log if line.startswith(f"domo(task-{key})")]


for agent in call("GET", "/api/agents"):
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})

default_agent = call(
    "POST",
    "/api/agents",
    {
        "name": "Switch Default",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 2,
    },
)
forced_agent = call(
    "POST",
    "/api/agents",
    {
        "name": "Switch Forced",
        "kind": "mock",
        "roles": ["executor"],
        "max_concurrency": 1,
        "enabled": False,
    },
)
# 新建项目的项目池继承默认团队。

# 1. 里程碑任务：验收通过后阻塞等待人工验收，产物不进入交付分支。
milestone_id = new_project("Milestone", "里程碑任务需要人工验收")
detail = wait_for(
    milestone_id,
    lambda item: item["project"]["stage"] == "executing" and len(item["tasks"]) == 2,
    "规划生成任务",
)
t1 = task_by_key(detail, "T1")
updated = call(
    "POST",
    f"/api/projects/{milestone_id}/tasks/{t1['id']}/milestone",
    {"milestone": True},
)
assert updated["milestone"] is True, updated
branch = detail["project"]["branch"]

detail = wait_for(
    milestone_id,
    lambda item: task_by_key(item, "T1")["status"] == "blocked"
    and task_by_key(item, "T1")["human_gate"] == "milestone",
    "里程碑任务等待人工验收",
)
t1 = task_by_key(detail, "T1")
assert t1["stage"] == "verifying", t1
assert t1["milestone"] is True, t1
assert detail["project"]["status"] == "blocked", detail["project"]
assert "等待人工验收" in (detail["project"]["blocked_reason"] or ""), detail["project"]
assert not branch_has(f"{branch}:domo_mock/T1.txt"), "里程碑任务不应在人工确认前合并"
assert any(
    event["kind"] == "milestone_acceptance_required" for event in detail["events"]
), detail["events"]

# 2. 人工拒绝：理由和指导进入下一轮执行 prompt，且不增加自动拒绝计数。
jobs_before_reject = len(detail["jobs"])
rejected = call(
    "POST",
    f"/api/projects/{milestone_id}/tasks/{t1['id']}/milestone/reject",
    {
        "reason": "人工验收意见：请在 T1 产物中补充人工要求",
        "guidance": "把人工验收意见写入 T1.txt 后再提交",
    },
)
t1 = task_by_key(rejected, "T1")
assert t1["status"] == "queued", t1
assert t1["human_gate"] is None, t1
assert t1["rejections"] == 0, t1
assert "人工验收意见" in (t1["verifier_issues"] or [""])[-1], t1

detail = wait_for(
    milestone_id,
    lambda item: len(
        [
            job
            for job in item["jobs"]
            if job["task_id"] == t1["id"] and job["role"] == "executor"
        ]
    )
    > len(
        [
            job
            for job in rejected["jobs"]
            if job["task_id"] == t1["id"] and job["role"] == "executor"
        ]
    ),
    "人工拒绝后重新执行",
)
next_job = [
    job
    for job in detail["jobs"]
    if job["task_id"] == t1["id"] and job["role"] == "executor"
][-1]
prompt = wait_job_prompt(next_job["id"])["prompt"]
assert "人工验收意见：请在 T1 产物中补充人工要求" in prompt, prompt
assert "把人工验收意见写入 T1.txt 后再提交" in prompt, prompt
assert len(detail["jobs"]) > jobs_before_reject, detail["jobs"]

detail = wait_for(
    milestone_id,
    lambda item: task_by_key(item, "T1")["status"] == "blocked"
    and task_by_key(item, "T1")["human_gate"] == "milestone",
    "再次等待人工验收",
)
assert not branch_has(f"{branch}:domo_mock/T1.txt"), "人工确认前不应合并 T1"

# 3. 人工 accept 后才合并完成任务；重复 accept 不产生第二次合并。
accepted = call(
    "POST",
    f"/api/projects/{milestone_id}/tasks/{t1['id']}/milestone/accept",
    {"reason": "人工确认通过"},
)
t1 = task_by_key(accepted, "T1")
assert t1["stage"] == "completed", t1
assert t1["status"] is None, t1
assert t1["human_gate"] is None, t1
assert branch_has(f"{branch}:domo_mock/T1.txt"), "人工通过后应合并 T1 产物"
assert len(task_merge_commits(branch, "T1")) == 1, task_merge_commits(branch, "T1")
code, _ = expect_error(
    "POST",
    f"/api/projects/{milestone_id}/tasks/{t1['id']}/milestone/accept",
    {"reason": "重复确认"},
)
assert code == 400, code
assert len(task_merge_commits(branch, "T1")) == 1, "重复人工验收产生了重复合并"

detail = wait_for(
    milestone_id,
    lambda item: item["project"]["stage"] == "summarizing" and item["project"]["report"],
    "里程碑项目完成并生成报告",
)
assert all(task["stage"] == "completed" for task in detail["tasks"]), detail["tasks"]
assert branch_has(f"{branch}:domo_mock/T2.txt"), "后续任务应正常完成并合并"
call("POST", f"/api/projects/{milestone_id}/report", {"action": "accept"})
assert call("GET", f"/api/projects/{milestone_id}")["project"]["stage"] == "completed"

# 2. 里程碑人工拒绝可要求重新规划：项目回到规划阶段，意见交给规划者。
replan_id = new_project("MilestoneReplan", "里程碑人工拒绝后要求重新规划")
detail = wait_for(
    replan_id,
    lambda item: item["project"]["stage"] == "executing" and len(item["tasks"]) == 2,
    "重规划场景规划完成",
)
replan_t1 = task_by_key(detail, "T1")
replan_branch = detail["project"]["branch"]
marked = call(
    "POST",
    f"/api/projects/{replan_id}/tasks/{replan_t1['id']}/milestone",
    {"milestone": True},
)
assert marked["milestone"] is True, marked
detail = wait_for(
    replan_id,
    lambda item: task_by_key(item, "T1")["status"] == "blocked"
    and task_by_key(item, "T1")["human_gate"] == "milestone",
    "重规划场景等待人工验收",
)
goal_jobs_before = len(goal_jobs(detail))
replan_reason = "人工验收意见：原计划遗漏里程碑要求的交付物"
replan_guidance = "DOMO-REPLAN-GUIDANCE：重新拆分 T1，补齐交付物后再推进 T2"
rejected = call(
    "POST",
    f"/api/projects/{replan_id}/tasks/{replan_t1['id']}/milestone/reject",
    {"reason": replan_reason, "guidance": replan_guidance, "action": "replan"},
)
project = rejected["project"]
assert project["replan_source"] == "human", project
assert replan_reason in (project["replan_reason"] or ""), project
assert replan_guidance in (project["replan_reason"] or ""), project
replan_t1 = task_by_key(rejected, "T1")
assert replan_t1["status"] == "canceled", replan_t1
assert replan_t1["human_gate"] is None, replan_t1
assert not branch_has(f"{replan_branch}:domo_mock/T1.txt"), "重新规划不应合并未完成任务"
assert any(
    event["kind"] == "replan_requested" and replan_guidance in event["message"]
    for event in rejected["events"]
), rejected["events"]
assert any(
    event["kind"] == "milestone_rejected"
    and "要求重新规划" in event["message"]
    and replan_reason in event["message"]
    for event in rejected["events"]
), rejected["events"]

detail = wait_for(
    replan_id,
    lambda item: len(goal_jobs(item)) > goal_jobs_before,
    "重新规划后生成规划者作业",
)
replan_goal_job = goal_jobs(detail)[-1]
prompt = wait_job_prompt(replan_goal_job["id"])["prompt"]
assert replan_reason in prompt, prompt
assert replan_guidance in prompt, prompt
assert "人工" in prompt, prompt

detail = wait_for(
    replan_id,
    lambda item: item["project"]["stage"] == "executing"
    and len([task for task in item["tasks"] if task["status"] != "canceled"]) == 2,
    "重新规划生成新任务",
)
assert all(task["key"] in ("T1", "T2") for task in detail["tasks"]), detail["tasks"]

# 4. switch_agent：默认 Agent 连续失败阻塞，人工指定停用的另一 Agent 执行。
switch_id = new_project("Switch", "[switch] 默认执行 Agent 持续失败后人工更换 Agent")
detail = wait_for(
    switch_id,
    lambda item: any(
        task["key"] == "T1" and task["status"] == "blocked" and task["attempts"] >= 3
        for task in item["tasks"]
    ),
    "默认 Agent 连续失败后阻塞",
)
switch_branch = detail["project"]["branch"]
t1 = task_by_key(detail, "T1")
assert detail["project"]["status"] == "blocked", detail["project"]
assert not branch_has(f"{switch_branch}:domo_mock/T1.txt"), "失败任务不应合并"

context = call(
    "GET", f"/api/projects/{switch_id}/tasks/{t1['id']}/arbitration"
)
assert context["task"]["id"] == t1["id"], context
assert context["role"] == "executor", context
assert context["task"]["last_error"], context
assert len(context["jobs"]) >= 3, context["jobs"]
assert any(agent["id"] == forced_agent["id"] for agent in context["agents"]), context
assert next(
    agent for agent in context["agents"] if agent["id"] == forced_agent["id"]
)["enabled"] is False, context

switched = call(
    "POST",
    f"/api/projects/{switch_id}/tasks/{t1['id']}/arbitration",
    {"action": "switch_agent", "agent_id": forced_agent["id"]},
)
assert task_by_key(switched, "T1")["status"] == "queued", switched
assert task_by_key(switched, "T1")["forced_agent_id"] == forced_agent["id"], switched
code, _ = expect_error(
    "POST",
    f"/api/projects/{switch_id}/tasks/{t1['id']}/arbitration",
    {"action": "switch_agent", "agent_id": forced_agent["id"]},
)
assert code == 400, code

detail = wait_for(
    switch_id,
    lambda item: any(
        job["task_id"] == t1["id"]
        and job["role"] == "executor"
        and job["agent_id"] == forced_agent["id"]
        for job in item["jobs"]
    ),
    "被指定 Agent 执行作业",
)
forced_job = next(
    job
    for job in detail["jobs"]
    if job["task_id"] == t1["id"]
    and job["role"] == "executor"
    and job["agent_id"] == forced_agent["id"]
)
assert forced_job["status"] in ("running", "succeeded"), forced_job
assert any(
    event["kind"] == "arbitration_switch_agent"
    and forced_agent["name"] in event["message"]
    for event in detail["events"]
), detail["events"]
assert len(
    [
        job
        for job in detail["jobs"]
        if job["task_id"] == t1["id"]
        and job["role"] == "executor"
        and job["agent_id"] == forced_agent["id"]
    ]
) == 1, "重复裁决产生了重复作业"

detail = wait_for(
    switch_id,
    lambda item: task_by_key(item, "T1")["stage"] == "completed"
    and task_by_key(item, "T1")["forced_agent_id"] is None,
    "强制 Agent 任务完成且设置失效",
)
t1 = task_by_key(detail, "T1")
assert branch_has(f"{switch_branch}:domo_mock/T1.txt"), "指定 Agent 的产物应合并"
assert len(task_merge_commits(switch_branch, "T1")) == 1, task_merge_commits(
    switch_branch, "T1"
)

detail = wait_for(
    switch_id,
    lambda item: any(
        job["task_id"] == task_by_key(item, "T2")["id"]
        and job["role"] == "executor"
        and job["agent_id"] == default_agent["id"]
        for job in item["jobs"]
    ),
    "后续任务恢复项目池调度",
)
t2 = task_by_key(detail, "T2")
t2_execs = [
    job
    for job in detail["jobs"]
    if job["task_id"] == t2["id"] and job["role"] == "executor"
]
assert t2_execs, detail["jobs"]
assert t2_execs[0]["agent_id"] == default_agent["id"], t2_execs
assert all(job["agent_id"] != forced_agent["id"] for job in t2_execs), t2_execs

detail = wait_for(
    switch_id,
    lambda item: item["project"]["stage"] == "summarizing" and item["project"]["report"],
    "切换 Agent 后项目完成",
)
call("POST", f"/api/projects/{switch_id}/report", {"action": "accept"})

# 5. 暂停中的项目只记录裁决，不派发作业；恢复后生效。
paused_id = new_project("PausedArbitration", "[switch] 暂停时记录裁决")
detail = wait_for(
    paused_id,
    lambda item: any(
        task["key"] == "T1" and task["status"] == "blocked" for task in item["tasks"]
    ),
    "暂停场景任务阻塞",
)
paused_t1 = task_by_key(detail, "T1")
marked = call(
    "POST",
    f"/api/projects/{paused_id}/tasks/{paused_t1['id']}/milestone",
    {"milestone": True},
)
assert marked["milestone"] is True, marked
cleared = call(
    "POST",
    f"/api/projects/{paused_id}/tasks/{paused_t1['id']}/milestone",
    {"milestone": False},
)
assert cleared["milestone"] is False, cleared
call("POST", f"/api/projects/{paused_id}/pause", {})
detail = wait_for(
    paused_id,
    lambda item: item["project"]["status"] == "paused",
    "项目暂停完成",
)
jobs_in_paused = len(detail["jobs"])
recorded = call(
    "POST",
    f"/api/projects/{paused_id}/tasks/{paused_t1['id']}/arbitration",
    {"action": "retry"},
)
assert recorded["project"]["status"] == "paused", recorded["project"]
assert task_by_key(recorded, "T1")["status"] == "queued", recorded
time.sleep(1.2)
still_paused = call("GET", f"/api/projects/{paused_id}")
assert still_paused["project"]["status"] == "paused", still_paused["project"]
assert len(still_paused["jobs"]) == jobs_in_paused, "暂停期间不应派发新作业"
call("POST", f"/api/projects/{paused_id}/resume", {})
detail = wait_for(
    paused_id,
    lambda item: len(item["jobs"]) > jobs_in_paused,
    "恢复后执行已记录的裁决",
)

# 6. 归档项目拒绝人工验收与裁决。
call("POST", f"/api/projects/{paused_id}/archive", {})
archived_t1 = task_by_key(detail, "T1")
code, _ = expect_error(
    "POST",
    f"/api/projects/{paused_id}/tasks/{archived_t1['id']}/arbitration",
    {"action": "retry"},
)
assert code == 400, code
code, _ = expect_error(
    "POST",
    f"/api/projects/{milestone_id}/tasks/{t1['id']}/milestone",
    {"milestone": True},
)
assert code == 400, code

print("里程碑人工验收与人工裁决测试通过")
print(f"  里程碑项目分支: {branch}")
print(f"  切换 Agent 项目分支: {switch_branch}")
print(f"  指定 Agent: {forced_agent['name']}")
PY

echo "==> 通过：$(basename "$0")"
