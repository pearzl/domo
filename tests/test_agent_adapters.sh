#!/usr/bin/env bash
# DOMO 内置 Agent 适配器（Codex CLI / WorkBuddy CLI）端到端测试。
#
# 覆盖范围：
#   1. 通过 DOMO_CODEX_BIN、DOMO_WORKBUDDY_BIN 注入假 codex 与假 codebuddy，
#      被测作业由假 CLI 执行，规划与验收由内置 mock 负责；假 CLI 从 stdin 读取
#      提示词，向 DOMO_JOB_DIR/result.json 写入不含 usage 的结果，并在 cwd 创建
#      domo_mock/T1.txt，分别输出 Codex 与 WorkBuddy 的 JSONL 事件；
#   2. Codex 项目与 WorkBuddy 项目的执行作业均为 succeeded，产物存续于交付分支，
#      作业 usage_known=true 且输入/输出 token、费用与假 CLI 输出一致；
#   3. result.json 不携带 usage 字段；
#   4. /api/jobs/{id}/activity 分别返回 format=codex 与 format=workbuddy，
#      并同时包含 message 与 tool 活动；
#   5. 创建 kind=unknown 的 Agent 返回 400，错误信息同时包含 codex 与 workbuddy
#      （此时尚未启动假 CLI，验证拒绝发生在进程启动之前）。
#
# 使用假 CLI 与内置 mock Agent，不访问外网、不调用真实 codex/codebuddy；
# 临时数据全部放在 ${TMPDIR:-/tmp}，退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-agent-adapters.$$"
PORT="${DOMO_AGENT_ADAPTERS_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)}"
SERVER_PID=""
# 假 CLI 只设置 DOMO_CODEX_BIN/DOMO_WORKBUDDY_BIN 指向本目录，真实 CLI 不可达。
FAKE_BIN="$TMP_ROOT/bin"
CALL_LOG="$TMP_ROOT/calls.log"

cleanup() {
  local status=$?
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ $status -ne 0 ]]; then
    if [[ -f "$TMP_ROOT/server.log" ]]; then
      echo "--- 服务日志 ---" >&2
      tail -200 "$TMP_ROOT/server.log" >&2
    fi
    if [[ -f "$CALL_LOG" ]]; then
      echo "--- 假 CLI 调用记录 ---" >&2
      cat "$CALL_LOG" >&2
    fi
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

mkdir -p "$TMP_ROOT/repo" "$FAKE_BIN"
git -C "$TMP_ROOT/repo" init -q -b main
git -C "$TMP_ROOT/repo" -c user.name=test -c user.email=test@example.com \
  commit --allow-empty -q -m "init"

# 假 CLI 共享行为：追加调用记录、读取 stdin 提示词、写结果、创建产物。
# 结果只包含协议字段，usage 由 stdout 的 JSONL 事件提供。
read -r -d '' FAKE_COMMON <<'PY' || true
import json
import os
import sys
import time

kind = KIND
args = sys.argv[1:]
prompt = sys.stdin.read()
with open(LOG_PATH, "a", encoding="utf-8") as log:
    log.write(
        json.dumps({"kind": kind, "args": args, "prompt_chars": len(prompt)}) + "\n"
    )
job_dir = os.environ["DOMO_JOB_DIR"]
assert os.environ["DOMO_JOB_ROLE"] == "executor", os.environ["DOMO_JOB_ROLE"]
assert "result.json" in prompt, "提示词应说明结果文件路径"
started = time.time()
os.makedirs("domo_mock", exist_ok=True)
with open("domo_mock/T1.txt", "w", encoding="utf-8") as handle:
    handle.write("hello from %s\n" % kind)
result = {"summary": "%s executor finished" % kind, "acceptance": ["domo_mock/T1.txt 存在"]}
with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
    json.dump(result, handle, ensure_ascii=False)
duration_ms = int((time.time() - started) * 1000)
PY

cat >"$FAKE_BIN/codex" <<PY
#!/usr/bin/env python3
KIND = "codex"
LOG_PATH = "${CALL_LOG}"
$FAKE_COMMON

LEVEL = "agts-codex"

def emit(event):
    sys.stdout.write(json.dumps(event, ensure_ascii=False) + "\n")
    sys.stdout.flush()

emit({"type": "thread.started", "thread_id": "thread-agts"})
emit({"type": "turn.started"})
emit({"type": "item.completed", "item": {"id": "item_0", "type": "reasoning", "text": "准备创建产物"}})
emit({"type": "item.started", "item": {"id": "item_1", "type": "command_execution", "command": "mkdir -p domo_mock", "aggregated_output": "", "exit_code": None, "status": "in_progress"}})
emit({"type": "item.completed", "item": {"id": "item_1", "type": "command_execution", "command": "mkdir -p domo_mock", "aggregated_output": "", "exit_code": 0, "status": "completed"}})
emit({"type": "item.completed", "item": {"id": "item_2", "type": "agent_message", "text": "已写入 domo_mock/T1.txt 与 result.json"}})
emit({"type": "turn.completed", "usage": {"input_tokens": 1234, "cached_input_tokens": 100, "output_tokens": 567, "reasoning_output_tokens": 89}})
PY

cat >"$FAKE_BIN/codebuddy" <<PY
#!/usr/bin/env python3
KIND = "workbuddy"
LOG_PATH = "${CALL_LOG}"
$FAKE_COMMON

SESSION = "session-agts"

def emit(event):
    sys.stdout.write(json.dumps(event, ensure_ascii=False) + "\n")
    sys.stdout.flush()

emit({"type": "system", "subtype": "init", "session_id": SESSION, "tools": ["Bash", "Write"]})
emit({"type": "assistant", "session_id": SESSION, "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "tool-1", "name": "Bash", "input": {"command": "mkdir -p domo_mock"}}]}})
emit({"type": "user", "session_id": SESSION, "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tool-1", "content": [{"type": "text", "text": "Exit Code: 0"}], "is_error": False}]}})
emit({"type": "assistant", "session_id": SESSION, "message": {"role": "assistant", "content": [{"type": "text", "text": "已写入 domo_mock/T1.txt 与 result.json"}]}})
emit({"type": "result", "subtype": "success", "is_error": False, "duration_ms": duration_ms, "total_cost_usd": 0.0123, "usage": {"input_tokens": 2048, "cache_read_input_tokens": 200, "cache_creation_input_tokens": 50, "output_tokens": 512}})
PY
chmod +x "$FAKE_BIN/codex" "$FAKE_BIN/codebuddy"

echo "==> 构建后端"
cargo build --manifest-path "$ROOT/server/Cargo.toml" >/dev/null

echo "==> 启动 DOMO (port $PORT)"
DOMO_DATA_ROOT="$TMP_ROOT/data" \
DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
DOMO_LOG_ROOT="$TMP_ROOT/log" \
DOMO_SERVER_PORT="$PORT" \
DOMO_CODEX_BIN="$FAKE_BIN/codex" \
DOMO_WORKBUDDY_BIN="$FAKE_BIN/codebuddy" \
DOMO_MOCK_DELAY_MS=30 \
DOMO_MOCK_EXECUTOR_DELAY_MS=30 \
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

python3 - "$PORT" "$TMP_ROOT/repo" "$CALL_LOG" <<'PY'
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
repo = sys.argv[2]
call_log = sys.argv[3]

CASES = {
    "codex": {
        "kind": "codex",
        "label": "Codex CLI",
        "format": "codex",
        "input_tokens": 1234,
        "output_tokens": 567,
        "cache_read_tokens": 100,
        "cache_write_tokens": 0,
        "reasoning_tokens": 89,
        "cost": None,
        "binary": os.path.join(os.path.dirname(call_log), "bin", "codex"),
    },
    "workbuddy": {
        "kind": "workbuddy",
        "label": "WorkBuddy CLI",
        "format": "workbuddy",
        "input_tokens": 2048,
        "output_tokens": 512,
        "cache_read_tokens": 200,
        "cache_write_tokens": 50,
        "reasoning_tokens": None,
        "cost": 0.0123,
        "binary": os.path.join(os.path.dirname(call_log), "bin", "codebuddy"),
    },
}


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


def expect_status(method, path, data, status):
    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(
        base + path,
        data=body,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


def calls():
    if not os.path.exists(call_log):
        return []
    with open(call_log, encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


# 创建未知类型必须先于任何假 CLI 调用被拒绝。
before = len(calls())
status, body = expect_status(
    "POST",
    "/api/agents",
    {"name": "Unknown", "kind": "unknown", "roles": ["executor"]},
    400,
)
assert status == 400, f"unknown 类型应返回 400，实际 {status}: {body}"
message = json.dumps(body, ensure_ascii=False)
for marker in ("codex", "workbuddy"):
    assert marker in message, f"400 错误信息缺少 {marker}: {message}"
assert len(calls()) == before, "未知类型不应启动任何 CLI 进程"

# 停用内置默认团队中已有的 Agent；规划与验收由新建 mock 负责，
# 执行者只放被测 Agent，避免 mock 抢到执行作业。
for agent in call("GET", "/api/agents"):
    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
mock = call(
    "POST",
    "/api/agents",
    {
        "name": "Adapter Mock",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 4,
    },
)
for role in ("planner", "verifier"):
    call("POST", "/api/team/default", {"role": role, "agent_id": mock["id"]})

reports = {}
for key, case in CASES.items():
    agent = call(
        "POST",
        "/api/agents",
        {
            "name": case["label"],
            "kind": case["kind"],
            "roles": ["executor"],
            "max_concurrency": 1,
        },
    )
    assert agent["kind"] == case["kind"], agent

    project = call(
        "POST",
        "/api/projects",
        {
            "name": f"Adapter {key}",
            "repo_path": repo,
            "base_branch": "main",
        },
    )["project"]
    project_id = project["id"]

    # 项目池在项目创建时从默认团队快照；这里把被测 Agent 设为唯一可用的执行者。
    pool = call("POST", f"/api/projects/{project_id}/agents", {"role": "executor", "agent_id": agent["id"]})
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
    pool = call("GET", f"/api/projects/{project_id}/agents")
    executors = [
        entry["agent_id"]
        for group in pool["roles"]
        if group["role"] == "executor"
        for entry in group["entries"]
        if entry["enabled"]
    ]
    assert executors == [agent["id"]], f"{key} 项目执行者池配置异常: {pool}"

    goal = (
        f"[single] 在仓库根目录创建 domo_mock/T1.txt，内容为一行 hello from {key}。"
        "不要修改其他文件。验收标准：domo_mock/T1.txt 存在。"
    )
    call(
        "POST",
        f"/api/projects/{project_id}/analysis/messages",
        {"message": goal, "confirm_mode": "auto"},
    )

    def project_detail():
        return call("GET", f"/api/projects/{project_id}")

    def wait_stage(done, timeout, description):
        deadline = time.time() + timeout
        current = None
        while time.time() < deadline:
            try:
                current = project_detail()
            except RuntimeError as error:
                # 规划者正在更新方案时确认会被拒绝，下一轮重试。
                if "请稍后再确认" not in str(error):
                    raise
                time.sleep(0.1)
                continue
            state = current["project"]
            if state["status"] == "blocked" and state["stage"] == "analysis":
                raise AssertionError(
                    f"{key} 项目分析阻塞：{state['blocked_reason']} {current['messages']}"
                )
            if done(state):
                return current
            if current.get("proposal") and state["stage"] == "analysis":
                # confirm_mode=auto 只在收到消息时生效，方案已生成时显式确认。
                call("POST", f"/api/projects/{project_id}/analysis/confirm", {})
            time.sleep(0.1)
        raise AssertionError(
            f"等待超时：{description}\n"
            + json.dumps(current, ensure_ascii=False, indent=2)
        )

    wait_stage(lambda state: state["stage"] != "analysis", 180, f"{key} 项目分析完成")

    detail = wait_stage(
        lambda state: state["stage"] in ("ready", "summarizing", "completed"),
        300,
        f"{key} 项目执行完成",
    )

    tasks = detail["tasks"]
    assert tasks, f"{key} 项目没有任务"
    task_ids = {task["id"] for task in tasks}
    exec_jobs = [
        job for job in detail["jobs"] if job["role"] == "executor" and job["task_id"] in task_ids
    ]
    assert exec_jobs, f"{key} 缺少执行作业"
    # 验收作业的 worktree 已回收，被测 Agent 会因产物缺失而重试；以成功的那次为准。
    succeeded = [job for job in exec_jobs if job["status"] == "succeeded"]
    assert succeeded, f"{key} 执行作业均未成功: {exec_jobs}"
    job = succeeded[0]
    for other in exec_jobs:
        assert other["agent_id"] == agent["id"], f"{key} 执行作业使用了非被测 Agent: {other}"

    # 作业详情给出权威的用量记录。
    job_detail = call("GET", f"/api/jobs/{job['id']}")
    assert job_detail["status"] == "succeeded", job_detail
    assert job_detail["usage_known"] is True, f"{key} usage_known 不为 true: {job_detail}"
    assert job_detail["input_tokens"] == case["input_tokens"], job_detail
    assert job_detail["output_tokens"] == case["output_tokens"], job_detail
    assert job_detail["cache_read_tokens"] == case["cache_read_tokens"], job_detail
    if case["cost"] is None:
        assert job_detail.get("cost") is None, f"{key} 不应虚构费用: {job_detail}"
    else:
        assert job_detail.get("cost") is not None, job_detail
        assert abs(job_detail["cost"] - case["cost"]) < 1e-9, job_detail

    # result.json 来自假 CLI，只含协议字段。
    workspace = detail["project"]["workspace"]
    result_path = os.path.join(workspace, "jobs", job["id"], "result.json")
    with open(result_path, encoding="utf-8") as handle:
        result = json.load(handle)
    assert "usage" not in result, f"{key} result.json 不应包含 usage: {result}"

    # 活动流与 stdout.log 均按被测 CLI 的 JSONL 解析。
    activity = call("GET", f"/api/jobs/{job['id']}/activity")
    assert activity["format"] == case["format"], f"{key} activity format 错误: {activity}"
    kinds = {item["kind"] for item in activity["items"]}
    assert "message" in kinds, f"{key} 活动缺少 message: {activity}"
    assert "tool" in kinds, f"{key} 活动缺少 tool: {activity}"
    assert activity["summary"]["messages"] >= 1, activity
    assert activity["summary"]["tool_calls"] >= 1, activity
    with open(os.path.join(workspace, "jobs", job["id"], "stdout.log"), encoding="utf-8") as handle:
        stdout = handle.read()
    assert '"turn.completed"' in stdout or '"type": "result"' in stdout, f"{key} stdout 缺少用量事件"

    reports[key] = {
        "project": project_id,
        "job": job["id"],
        "activity": {"format": activity["format"], "kinds": sorted(kinds)},
    }

# 产物随任务分支交付：执行作业的 worktree 会在验收后移除，交付分支保留产物。
for key in CASES:
    project = call("GET", f"/api/projects/{reports[key]['project']}")["project"]
    branch = project["branch"]
    listing = subprocess.check_output(
        ["git", "-C", repo, "ls-tree", "-r", "--name-only", branch], text=True
    )
    assert "domo_mock/T1.txt" in listing.split(), f"{key} 交付分支缺少产物: {listing}"
    content = subprocess.check_output(
        ["git", "-C", repo, "show", f"{branch}:domo_mock/T1.txt"], text=True
    )
    assert content.strip() == f"hello from {key}", repr(content)

# 假 CLI 必须被真实调用，且参数形态符合适配器约定。
recorded = calls()
for key, case in CASES.items():
    invocations = [item for item in recorded if item["kind"] == key]
    assert len(invocations) == 1, f"{key} 假 CLI 调用次数异常: {invocations}"
    args = invocations[0]["args"]
    assert invocations[0]["prompt_chars"] > 0, f"{key} 未从 stdin 读取提示词"
    if key == "codex":
        assert args[:2] == ["exec", "--json"], args
        assert args[-1] == "-", args
    else:
        assert "-p" in args and "stream-json" in args, args

print("内置 Agent 适配器端到端测试通过")
for key, report in reports.items():
    print(f"  {key}: 项目 {report['project']} 作业 {report['job']} 活动 {report['activity']}")
PY

echo "==> 通过：$(basename "$0")"
