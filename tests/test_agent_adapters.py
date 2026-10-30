#!/usr/bin/env python3
"""DOMO 内置 Agent 适配器（Codex CLI / WorkBuddy CLI）端到端测试。

覆盖范围：
  1. 通过 DOMO_CODEX_BIN、DOMO_WORKBUDDY_BIN 注入假 codex 与假 codebuddy，
     被测作业由假 CLI 执行，规划与验收由内置 mock 负责；假 CLI 从 stdin 读取
     提示词，向 DOMO_JOB_DIR/result.json 写入不含 usage 的结果，并在 cwd 创建
     domo_mock/T1.txt，分别输出 Codex 与 WorkBuddy 的 JSONL 事件；
  2. Codex 项目与 WorkBuddy 项目的执行作业均为 succeeded，产物存续于交付分支，
     作业 usage_known=true 且输入/输出 token、费用与假 CLI 输出一致；
  3. result.json 不携带 usage 字段；
  4. /api/jobs/{id}/activity 分别返回 format=codex 与 format=workbuddy，
     并同时包含 message 与 tool 活动；
  5. 创建 kind=unknown 的 Agent 返回 400，错误信息同时包含 codex 与 workbuddy
     （假 CLI 尚未启动，验证拒绝发生在进程启动前）。

使用假 CLI 与内置 mock Agent，不访问外网、不调用真实 codex/codebuddy；
临时数据全部放在系统临时目录，退出码 0 表示全部通过。
"""

import json
import time
import urllib.error
import urllib.request

from harness import (
    Server,
    ensure_server_built,
    git,
    init_repo,
    temp_root,
    write_python_launcher,
)

# 假 CLI 共享行为：追加调用记录、读取 stdin 提示词、写结果、创建产物。
# 结果只包含协议字段，usage 由 stdout 的 JSONL 事件提供。
FAKE_COMMON = '''\
sys.stdin.reconfigure(encoding="utf-8", errors="replace")
sys.stdout.reconfigure(encoding="utf-8")
sys.stderr.reconfigure(encoding="utf-8", errors="replace")

kind = KIND
args = sys.argv[1:]
prompt = sys.stdin.read()
with open(LOG_PATH, "a", encoding="utf-8") as log:
    log.write(
        json.dumps({"kind": kind, "args": args, "prompt_chars": len(prompt)}) + "\\n"
    )
job_dir = os.environ["DOMO_JOB_DIR"]
assert os.environ["DOMO_JOB_ROLE"] == "executor", os.environ["DOMO_JOB_ROLE"]
assert "result.json" in prompt, "提示词应说明结果文件路径"
started = time.time()
os.makedirs("domo_mock", exist_ok=True)
with open("domo_mock/T1.txt", "w", encoding="utf-8") as handle:
    handle.write("hello from %s\\n" % kind)
result = {"summary": "%s executor finished" % kind, "acceptance": ["domo_mock/T1.txt 存在"]}
with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
    json.dump(result, handle, ensure_ascii=False)
duration_ms = int((time.time() - started) * 1000)
'''

FAKE_HEADER = '''\
import json
import os
import sys
import time

KIND = __KIND__
LOG_PATH = __LOG__
'''

CODEX_BODY = '''
LEVEL = "agts-codex"


def emit(event):
    sys.stdout.write(json.dumps(event, ensure_ascii=False) + "\\n")
    sys.stdout.flush()


emit({"type": "thread.started", "thread_id": "thread-agts"})
emit({"type": "turn.started"})
emit({"type": "item.completed", "item": {"id": "item_0", "type": "reasoning", "text": "准备创建产物"}})
emit({"type": "item.started", "item": {"id": "item_1", "type": "command_execution", "command": "mkdir -p domo_mock", "aggregated_output": "", "exit_code": None, "status": "in_progress"}})
emit({"type": "item.completed", "item": {"id": "item_1", "type": "command_execution", "command": "mkdir -p domo_mock", "aggregated_output": "", "exit_code": 0, "status": "completed"}})
emit({"type": "item.completed", "item": {"id": "item_2", "type": "agent_message", "text": "已写入 domo_mock/T1.txt 与 result.json"}})
emit({"type": "turn.completed", "usage": {"input_tokens": 1234, "cached_input_tokens": 100, "output_tokens": 567, "reasoning_output_tokens": 89}})
'''

WORKBUDDY_BODY = '''
SESSION = "session-agts"


def emit(event):
    sys.stdout.write(json.dumps(event, ensure_ascii=False) + "\\n")
    sys.stdout.flush()


emit({"type": "system", "subtype": "init", "session_id": SESSION, "tools": ["Bash", "Write"]})
emit({"type": "assistant", "session_id": SESSION, "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "tool-1", "name": "Bash", "input": {"command": "mkdir -p domo_mock"}}]}})
emit({"type": "user", "session_id": SESSION, "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tool-1", "content": [{"type": "text", "text": "Exit Code: 0"}], "is_error": False}]}})
emit({"type": "assistant", "session_id": SESSION, "message": {"role": "assistant", "content": [{"type": "text", "text": "已写入 domo_mock/T1.txt 与 result.json"}]}})
emit({"type": "result", "subtype": "success", "is_error": False, "duration_ms": duration_ms, "total_cost_usd": 0.0123, "usage": {"input_tokens": 2048, "cache_read_input_tokens": 200, "cache_creation_input_tokens": 50, "output_tokens": 512}})
'''


def write_fake_cli(path, kind, log_path, body):
    text = (
        FAKE_HEADER.replace("__KIND__", repr(kind)).replace("__LOG__", repr(str(log_path)))
        + FAKE_COMMON
        + body
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    return path


def expect_status(server, method, path, data):
    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(
        server.base + path,
        data=body,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


def main():
    with temp_root("domo-agent-adapters-") as temp:
        repo = temp / "repo"
        init_repo(repo)

        # 假 CLI 只设置 DOMO_CODEX_BIN/DOMO_WORKBUDDY_BIN 指向本目录，真实 CLI 不可达。
        bin_dir = temp / "bin"
        call_log = temp / "calls.log"
        codex_script = write_fake_cli(
            bin_dir / "codex.py", "codex", call_log, CODEX_BODY
        )
        codebuddy_script = write_fake_cli(
            bin_dir / "codebuddy.py", "workbuddy", call_log, WORKBUDDY_BODY
        )
        codex_launcher = write_python_launcher(codex_script)
        codebuddy_launcher = write_python_launcher(codebuddy_script)

        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_CODEX_BIN": codex_launcher,
                "DOMO_WORKBUDDY_BIN": codebuddy_launcher,
                "DOMO_MOCK_DELAY_MS": "30",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "30",
            },
        ) as server:

            def call(method, path, data=None):
                return server.call(method, path, data)

            def calls():
                if not call_log.exists():
                    return []
                with open(call_log, encoding="utf-8") as handle:
                    return [json.loads(line) for line in handle if line.strip()]

            cases = {
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
                    "binary": str(codex_launcher),
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
                    "binary": str(codebuddy_launcher),
                },
            }

            # 创建未知类型必须先于任何假 CLI 调用被拒绝。
            before = len(calls())
            status, body = expect_status(
                server,
                "POST",
                "/api/agents",
                {"name": "Unknown", "kind": "unknown", "roles": ["executor"]},
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
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Adapter Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 4,
                },
            )

            reports = {}
            for key, case in cases.items():
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
                        "repo_path": str(repo),
                        "base_branch": "main",
                    },
                )["project"]
                project_id = project["id"]

                # 项目池在项目创建时从默认团队快照；这里把被测 Agent 设为唯一可用的执行者。
                pool = call("GET", f"/api/projects/{project_id}/agents")
                for entry in next(
                    group for group in pool["roles"] if group["role"] == "executor"
                )["entries"]:
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
                        except AssertionError as error:
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
                            call(
                                "POST",
                                f"/api/projects/{project_id}/analysis/confirm",
                                {},
                            )
                        time.sleep(0.1)
                    raise AssertionError(
                        f"等待超时：{description}\n"
                        + json.dumps(current, ensure_ascii=False, indent=2)
                    )

                wait_stage(
                    lambda state: state["stage"] != "analysis",
                    180,
                    f"{key} 项目分析完成",
                )

                detail = wait_stage(
                    lambda state: state["stage"] in ("ready", "summarizing", "completed"),
                    300,
                    f"{key} 项目执行完成",
                )

                tasks = detail["tasks"]
                assert tasks, f"{key} 项目没有任务"
                task_ids = {task["id"] for task in tasks}
                exec_jobs = [
                    job
                    for job in detail["jobs"]
                    if job["role"] == "executor" and job["task_id"] in task_ids
                ]
                assert exec_jobs, f"{key} 缺少执行作业"
                # 验收作业的 worktree 已回收，被测 Agent 会因产物缺失而重试；以成功的那次为准。
                succeeded = [job for job in exec_jobs if job["status"] == "succeeded"]
                assert succeeded, f"{key} 执行作业均未成功: {exec_jobs}"
                job = succeeded[0]
                for other in exec_jobs:
                    assert other["agent_id"] == agent["id"], (
                        f"{key} 执行作业使用了非被测 Agent: {other}"
                    )

                # 作业详情给出权威的用量记录。
                job_detail = call("GET", f"/api/jobs/{job['id']}")
                assert job_detail["status"] == "succeeded", job_detail
                assert job_detail["usage_known"] is True, (
                    f"{key} usage_known 不为 true: {job_detail}"
                )
                assert job_detail["input_tokens"] == case["input_tokens"], job_detail
                assert job_detail["output_tokens"] == case["output_tokens"], job_detail
                assert job_detail["cache_read_tokens"] == case["cache_read_tokens"], (
                    job_detail
                )
                if case["cost"] is None:
                    assert job_detail.get("cost") is None, (
                        f"{key} 不应虚构费用: {job_detail}"
                    )
                else:
                    assert job_detail.get("cost") is not None, job_detail
                    assert abs(job_detail["cost"] - case["cost"]) < 1e-9, job_detail

                # result.json 来自假 CLI，只含协议字段。
                workspace = detail["project"]["workspace"]
                result_path = workspace + "/jobs/" + job["id"] + "/result.json"
                with open(result_path, encoding="utf-8") as handle:
                    result = json.load(handle)
                assert "usage" not in result, f"{key} result.json 不应包含 usage: {result}"

                # 活动流与 stdout.log 均按被测 CLI 的 JSONL 解析。
                activity = call("GET", f"/api/jobs/{job['id']}/activity")
                assert activity["format"] == case["format"], (
                    f"{key} activity format 错误: {activity}"
                )
                kinds = {item["kind"] for item in activity["items"]}
                assert "message" in kinds, f"{key} 活动缺少 message: {activity}"
                assert "tool" in kinds, f"{key} 活动缺少 tool: {activity}"
                assert activity["summary"]["messages"] >= 1, activity
                assert activity["summary"]["tool_calls"] >= 1, activity
                with open(
                    workspace + "/jobs/" + job["id"] + "/stdout.log",
                    encoding="utf-8",
                ) as handle:
                    stdout = handle.read()
                assert '"turn.completed"' in stdout or '"type": "result"' in stdout, (
                    f"{key} stdout 缺少用量事件"
                )

                reports[key] = {
                    "project": project_id,
                    "job": job["id"],
                    "activity": {"format": activity["format"], "kinds": sorted(kinds)},
                }

            # 产物随任务分支交付：执行作业的 worktree 在验收后回收，交付分支保留产物。
            for key in cases:
                project = call("GET", f"/api/projects/{reports[key]['project']}")["project"]
                branch = project["branch"]
                listing = git(["ls-tree", "-r", "--name-only", branch], repo)
                assert "domo_mock/T1.txt" in listing.split(), (
                    f"{key} 交付分支缺少产物: {listing}"
                )
                content = git(["show", f"{branch}:domo_mock/T1.txt"], repo)
                assert content.strip() == f"hello from {key}", repr(content)

            # 假 CLI 必须被真实调用，且参数形态符合适配器约定。
            recorded = calls()
            for key, case in cases.items():
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
                print(
                    f"  {key}: 项目 {report['project']} 作业 {report['job']} "
                    f"活动 {report['activity']}"
                )


if __name__ == "__main__":
    main()
