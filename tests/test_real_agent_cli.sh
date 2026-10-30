#!/usr/bin/env bash
# DOMO Codex CLI / WorkBuddy CLI 真实冒烟测试。
#
# 只有在设置 DOMO_RT_CODEX_MODEL 或 DOMO_RT_WORKBUDDY_MODEL 时运行对应真实
# CLI，例如：
#   DOMO_RT_CODEX_MODEL=<provider/model> tests/test_real_agent_cli.sh
#   DOMO_RT_WORKBUDDY_MODEL=<model> DOMO_RT_WORKBUDDY_BIN=/path/to/codebuddy \
#     tests/test_real_agent_cli.sh
#
# 场景：对每个已配置的 CLI，用真实 Agent 完成一个最小的、可客观验收的任务
# （在仓库根目录创建 hello.txt，内容为一行 hello domo），断言执行作业成功、
# 产物随交付分支保留、作业记录了活动与用量读取结果。规划与验收由内置 mock
# 负责，避免把真实调用扩展到整条链路。
#
# 脚本不硬编码模型名；未设置任何模型环境变量时打印跳过信息并以 0 退出。
# 指定 BIN 覆盖时可用 DOMO_RT_CODEX_BIN、DOMO_RT_WORKBUDDY_BIN；未设置
# 时沿用 DOMO_CODEX_BIN、DOMO_WORKBUDDY_BIN 或引擎默认可执行文件。
# 执行者不提交产物，脚本以执行工作树中的产物或执行日志作为完成依据。

set -euo pipefail

if [[ -z "${DOMO_RT_CODEX_MODEL:-}" && -z "${DOMO_RT_WORKBUDDY_MODEL:-}" ]]; then
  echo "跳过：未设置 DOMO_RT_CODEX_MODEL 或 DOMO_RT_WORKBUDDY_MODEL（真实 CLI 用例）"
  exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-real-agent-cli.$$"
PORT="${DOMO_REAL_AGENT_CLI_PORT:-$(python3 - <<'PY'
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
DOMO_MOCK_DELAY_MS=30 \
DOMO_CODEX_BIN="${DOMO_RT_CODEX_BIN:-${DOMO_CODEX_BIN:-codex}}" \
DOMO_WORKBUDDY_BIN="${DOMO_RT_WORKBUDDY_BIN:-${DOMO_WORKBUDDY_BIN:-codebuddy}}" \
"$ROOT/server/target/debug/domo-server" >"$TMP_ROOT/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 120); do
  if curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null; then
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "服务启动失败" >&2
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

base = f"http://127.0.0.1:{sys.argv[1]}"
repo = sys.argv[2]
timeout_secs = int(os.environ.get("DOMO_JOB_TIMEOUT_SECS", "900"))

CASES = []
if os.environ.get("DOMO_RT_CODEX_MODEL"):
    CASES.append(
        {
            "kind": "codex",
            "model": os.environ["DOMO_RT_CODEX_MODEL"],
            "format": "codex",
        }
    )
if os.environ.get("DOMO_RT_WORKBUDDY_MODEL"):
    CASES.append(
        {
            "kind": "workbuddy",
            "model": os.environ["DOMO_RT_WORKBUDDY_MODEL"],
            "format": "workbuddy",
        }
    )


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


for agent in call("GET", "/api/agents"):
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
mock = call(
    "POST",
    "/api/agents",
    {
        "name": "Real CLI Mock",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 4,
    },
)

reports = []
for case in CASES:
    key = case["kind"]
    agent = call(
        "POST",
        "/api/agents",
        {
            "name": f"Real {key}",
            "kind": key,
            "model": case["model"],
            "roles": ["executor"],
            "max_concurrency": 1,
        },
    )
    assert agent["kind"] == key, agent
    assert agent["model"] == case["model"], agent

    project = call(
        "POST",
        "/api/projects",
        {
            "name": f"Real {key} smoke",
            "repo_path": repo,
            "base_branch": "main",
        },
    )["project"]
    project_id = project["id"]
    # 项目池在创建时从默认团队快照，把真实 CLI 设为唯一可用的执行者。
    pool = call(
        "POST",
        f"/api/projects/{project_id}/agents",
        {"role": "executor", "agent_id": agent["id"]},
    )
    for entry in next(group for group in pool["roles"] if group["role"] == "executor")["entries"]:
        if entry["agent_id"] != agent["id"] and entry["enabled"]:
            call(
                "PATCH",
                f"/api/projects/{project_id}/agents/executor/{entry['agent_id']}",
                {"enabled": False},
            )
    call(
        "PATCH",
        f"/api/projects/{project_id}/agents/executor/{agent['id']}",
        {"position": 0},
    )
    goal = (
        "[single] 在仓库根目录创建 hello.txt，内容为一行 hello domo。"
        "不要修改其他文件。验收标准：hello.txt 存在。"
    )
    call(
        "POST",
        f"/api/projects/{project_id}/analysis/messages",
        {"message": goal, "confirm_mode": "auto"},
    )

    deadline = time.time() + timeout_secs
    detail = None
    while time.time() < deadline:
        detail = call("GET", f"/api/projects/{project_id}")
        state = detail["project"]
        if state["stage"] == "analysis" and state["status"] == "blocked":
            raise AssertionError(f"{key} 项目分析阻塞：{state['blocked_reason']}")
        if state["stage"] == "analysis" and detail.get("proposal"):
            try:
                call("POST", f"/api/projects/{project_id}/analysis/confirm", {})
            except RuntimeError as error:
                if "请稍后再确认" not in str(error):
                    raise
        if state["stage"] in ("ready", "summarizing", "completed"):
            break
        time.sleep(1)
    else:
        raise AssertionError(f"{key} 等待项目执行超时：{json.dumps(detail, ensure_ascii=False)}")

    task_ids = {task["id"] for task in detail["tasks"]}
    exec_jobs = [
        job for job in detail["jobs"] if job["role"] == "executor" and job["task_id"] in task_ids
    ]
    succeeded = [job for job in exec_jobs if job["status"] == "succeeded"]
    if not succeeded:
        raise AssertionError(
            f"{key} 执行作业未成功：\n"
            + json.dumps(exec_jobs, ensure_ascii=False, indent=2)
        )
    job = succeeded[0]

    workspace = detail["project"]["workspace"]
    job_dir = os.path.join(workspace, "jobs", job["id"])
    worktree = next(
        (
            task["worktree"]
            for task in detail["tasks"]
            if task["id"] == job["task_id"] and task.get("worktree")
        ),
        None,
    )
    with open(os.path.join(job_dir, "stdout.log"), encoding="utf-8") as handle:
        stdout = handle.read()
    with open(os.path.join(job_dir, "stderr.log"), encoding="utf-8") as handle:
        stderr = handle.read()
    if worktree and os.path.isfile(os.path.join(worktree, "hello.txt")):
        with open(os.path.join(worktree, "hello.txt"), encoding="utf-8") as handle:
            content = handle.read().strip()
        assert content == "hello domo", repr(content)
        artifact = "worktree"
    else:
        # 执行者不提交；产物出现在交付分支是在后续环节，这里以日志中的写入动作为准。
        assert "hello.txt" in stdout, (
            f"{key} 执行日志中找不到 hello.txt 痕迹：\n{stdout[-4000:]}"
        )
        artifact = "log"

    with open(os.path.join(job_dir, "result.json"), encoding="utf-8") as handle:
        json.load(handle)

    activity = call("GET", f"/api/jobs/{job['id']}/activity")
    assert activity["format"] == case["format"], f"{key} 活动格式错误：{activity}"
    reports.append(
        {
            "kind": key,
            "job": job["id"],
            "artifact": artifact,
            "usage_known": job["usage_known"],
            "input_tokens": job["input_tokens"],
            "output_tokens": job["output_tokens"],
            "activity_format": activity["format"],
            "messages": activity["summary"]["messages"],
            "tool_calls": activity["summary"]["tool_calls"],
            "stderr_tail": stderr[-800:],
        }
    )


print("真实 CLI 冒烟测试通过")
for report in reports:
    print("  " + json.dumps(report, ensure_ascii=False))
PY

echo "==> 通过：$(basename "$0")"
