#!/usr/bin/env python3
"""DOMO 自定义 CLI 与 OpenCode 调用契约端到端测试。

覆盖范围：
  1. kind=cli 的命令模板占位符：{prompt} 存在时提示词走位置参数且 stdin 为空；
     模板缺少 {prompt} 时提示词走 stdin；{session_id}、{job_dir}、{cwd}、
     {model}、{model_args} 按适配器规则展开，模型参数在模板未引用时追加。
  2. 续跑轮次沿用首次运行派生的 session，继续提示词只包含继续指令。
  3. kind=opencode 通过 stdin 接收提示词，参数固定为 run --auto --thinking
     --format json --session <session> --title "DOMO <role> <job_id>"，配置模型时
     附带 --model；续跑使用同一 session，用量以该 session 的账本为准。
  4. 各适配器作业的 args、prompt_chars、session 与 usage 数值和既有契约一致。

codex 与 workbuddy 的参数、stdin、会话与用量由 tests/test_agent_adapters.py 与
tests/test_session_reuse.py 覆盖。本脚本只使用 Python 3 标准库与 tests/harness.py，
以假 CLI 代替真实 Agent，不访问外网；临时数据由 temp_root 清理。
"""

import json
import string
import sys
import time
from pathlib import Path

from harness import Server, init_repo, temp_root, write_python_launcher

# 假 CLI 在每次执行时记录实际收到的参数与 stdin；首次运行只登记状态并正常退出，
# 触发 runner 的继续轮次；第二次运行写入产物与结果。
CLI_FAKE = '''\
import json
import os
import pathlib
import sys

log_path = pathlib.Path(os.environ["FAKE_LOG"])
state_dir = pathlib.Path(os.environ["FAKE_STATE"])
job_id = os.environ["DOMO_JOB_ID"]
job_dir = pathlib.Path(os.environ["DOMO_JOB_DIR"])
role = os.environ["DOMO_JOB_ROLE"]
prompt = sys.stdin.read()
entry = {
    "kind": "cli",
    "args": sys.argv[1:],
    "prompt": prompt,
    "prompt_chars": len(prompt),
    "cwd": os.getcwd(),
    "job_dir": str(job_dir),
    "role": role,
}
with log_path.open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(entry, ensure_ascii=False) + "\\n")

state_dir.mkdir(parents=True, exist_ok=True)
marker = state_dir / job_id
if not marker.exists():
    marker.write_text("1", encoding="utf-8")
    sys.exit(0)

artifact = pathlib.Path.cwd() / "domo_mock" / "T1.txt"
artifact.parent.mkdir(parents=True, exist_ok=True)
artifact.write_text("T1", encoding="utf-8")
usage = json.loads(os.environ.get("FAKE_USAGE") or "{}")
result = {"kind": "done", "summary": "cli executor finished"}
if usage:
    result["usage"] = usage
(job_dir / "result.json").write_text(
    json.dumps(result, ensure_ascii=False), encoding="utf-8"
)
'''

# OpenCode 的账本查询通过 CLI 子命令完成：debug paths db 失败后回退到 API，
# 根会话返回固定用量，子会话查询返回空数组。
OPENCODE_FAKE = '''\
import json
import os
import pathlib
import sys

args = sys.argv[1:]
log_path = pathlib.Path(os.environ["FAKE_LOG"])
state_dir = pathlib.Path(os.environ["FAKE_STATE"])
job_dir = pathlib.Path(os.environ["DOMO_JOB_DIR"])
state_dir.mkdir(parents=True, exist_ok=True)


def log(entry):
    with log_path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(entry, ensure_ascii=False) + "\\n")


def option(name):
    return args[args.index(name) + 1] if name in args else ""


if args[:1] == ["run"]:
    prompt = sys.stdin.read()
    session = option("--session")
    log({
        "kind": "opencode",
        "args": args,
        "prompt": prompt,
        "prompt_chars": len(prompt),
        "session": session,
        "title": option("--title"),
        "model": option("--model"),
        "cwd": os.getcwd(),
        "job_dir": str(job_dir),
        "role": os.environ["DOMO_JOB_ROLE"],
    })
    marker = state_dir / os.environ["DOMO_JOB_ID"]
    if not marker.exists():
        marker.write_text("1", encoding="utf-8")
        sys.exit(0)
    artifact = pathlib.Path.cwd() / "domo_mock" / "T1.txt"
    artifact.parent.mkdir(parents=True, exist_ok=True)
    artifact.write_text("T1", encoding="utf-8")
    (job_dir / "result.json").write_text(
        json.dumps({"kind": "done", "summary": "opencode executor finished"}, ensure_ascii=False),
        encoding="utf-8",
    )
    print(json.dumps({
        "type": "text",
        "sessionID": session,
        "part": {"type": "text", "text": "已写入 domo_mock/T1.txt 与 result.json"},
    }, ensure_ascii=False), flush=True)
    print(json.dumps({
        "type": "tool_use",
        "sessionID": session,
        "part": {
            "type": "tool",
            "tool": "write",
            "state": {"status": "completed", "input": {"filePath": "domo_mock/T1.txt"}},
        },
    }, ensure_ascii=False), flush=True)
    sys.exit(0)

if args[:2] == ["debug", "paths"]:
    sys.exit(1)

if args[:2] == ["api", "get"]:
    path = args[2] if len(args) > 2 else ""
    if path.startswith("/api/session/"):
        session = path.rsplit("/", 1)[-1]
        log({"kind": "opencode-usage", "session": session})
        print(json.dumps({
            "data": {
                "id": session,
                "cost": 0.75,
                "tokens": {
                    "input": 1000,
                    "output": 200,
                    "reasoning": 30,
                    "cache": {"read": 40, "write": 10},
                },
            }
        }), flush=True)
        sys.exit(0)
    if path == "/api/session":
        print(json.dumps({"data": []}), flush=True)
        sys.exit(0)

sys.stderr.write("unexpected opencode invocation: %r\\n" % (args,))
sys.exit(1)
'''


def derived_session_id(job_id):
    """Mirrors the CLI adapter's session derivation for a job id."""
    suffix = job_id[4:] if job_id.startswith("job_") else job_id
    hex_digits = "".join(ch for ch in suffix if ch in string.hexdigits)
    if len(hex_digits) >= 32:
        return "{}-{}-{}-{}-{}".format(
            hex_digits[0:8],
            hex_digits[8:12],
            hex_digits[12:16],
            hex_digits[16:20],
            hex_digits[20:32],
        )
    return "ses_domo" + suffix


def opencode_session_id(job_id):
    suffix = job_id[4:] if job_id.startswith("job_") else job_id
    return "ses_domo" + suffix


def read_log(path):
    path = Path(path)
    if not path.exists():
        return []
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def wait_stage(call, project_id, done, timeout, description):
    deadline = time.time() + timeout
    current = None
    while time.time() < deadline:
        try:
            current = call("GET", f"/api/projects/{project_id}")
        except AssertionError as error:
            # 规划者更新方案期间的确认会被拒绝，下一轮重试。
            if "请稍后再确认" not in str(error):
                raise
            time.sleep(0.1)
            continue
        state = current["project"]
        if state["status"] == "blocked" and state["stage"] == "analysis":
            raise AssertionError(
                f"项目分析阻塞：{state['blocked_reason']} {current['messages']}"
            )
        if done(state):
            return current
        if current.get("proposal") and state["stage"] == "analysis":
            call("POST", f"/api/projects/{project_id}/analysis/confirm", {})
        time.sleep(0.1)
    raise AssertionError(
        f"等待超时：{description}\n" + json.dumps(current, ensure_ascii=False, indent=2)
    )


def drive_case(temp, key, agent_payload, env, goal):
    """Runs one project with a mock planner/verifier and the tested executor."""
    case_root = temp / key
    repo = case_root / "repo"
    init_repo(repo)

    with Server(case_root, env={"DOMO_MOCK_DELAY_MS": "20", **env}) as server:

        def call(method, path, data=None):
            return server.call(method, path, data)

        for agent in call("GET", "/api/agents"):
            call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
        call(
            "POST",
            "/api/agents",
            {
                "name": f"{key} Mock",
                "kind": "mock",
                "roles": ["planner", "verifier"],
                "max_concurrency": 4,
            },
        )
        agent = call("POST", "/api/agents", agent_payload)
        assert agent["kind"] == agent_payload["kind"], agent

        project = call(
            "POST",
            "/api/projects",
            {"name": key, "repo_path": str(repo), "base_branch": "main"},
        )["project"]
        project_id = project["id"]

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
        assert executors == [agent["id"]], f"执行者池配置异常: {pool}"

        call(
            "POST",
            f"/api/projects/{project_id}/analysis/messages",
            {"message": goal, "confirm_mode": "auto"},
        )
        wait_stage(
            call,
            project_id,
            lambda state: state["stage"] != "analysis",
            180,
            f"{key} 项目分析完成",
        )
        detail = wait_stage(
            call,
            project_id,
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
        for job in exec_jobs:
            assert job["agent_id"] == agent["id"], f"{key} 使用了非被测 Agent: {job}"
        succeeded = [job for job in exec_jobs if job["status"] == "succeeded"]
        assert len(succeeded) == 1, f"{key} 执行作业状态异常: {exec_jobs}"
        job = succeeded[0]
        return call("GET", f"/api/jobs/{job['id']}")


def assert_usage(job_detail, expected, key):
    assert job_detail["usage_known"] is True, f"{key} usage_known 不为 true: {job_detail}"
    for field in (
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "reasoning_tokens",
    ):
        if field in expected:
            assert job_detail[field] == expected[field], f"{key} {field} 异常: {job_detail}"
    if expected.get("cost") is None:
        assert job_detail.get("cost") is None, f"{key} 不应虚构费用: {job_detail}"
    else:
        assert job_detail.get("cost") is not None, job_detail
        assert abs(job_detail["cost"] - expected["cost"]) < 1e-9, job_detail


def run_cli_positional(temp, script):
    key = "cli-positional"
    log_path = temp / "cli-positional.jsonl"
    model = "cli/fake-model"
    model_args = ["--opt-model", "{model}", "--opt-session", "{session_id}"]
    command = [
        sys.executable,
        str(script),
        "--prompt",
        "{prompt}",
        "--session",
        "{session_id}",
        "--job-dir",
        "{job_dir}",
        "--cwd",
        "{cwd}",
        "--model",
        "{model}",
        "{model_args}",
        "--inline=prefix/{model}/suffix",
    ]
    usage = {
        "input_tokens": 111,
        "output_tokens": 22,
        "cache_read_tokens": 3,
        "cache_write_tokens": 4,
        "reasoning_tokens": 5,
        "cost": 0.5,
    }
    job = drive_case(
        temp,
        key,
        {
            "name": key,
            "kind": "cli",
            "model": model,
            "roles": ["executor"],
            "max_concurrency": 1,
            "command": command,
            "model_args": model_args,
        },
        {
            "FAKE_LOG": str(log_path),
            "FAKE_STATE": str(temp / "cli-positional-state"),
            "FAKE_USAGE": json.dumps(usage),
        },
        "[single] 创建 domo_mock/T1.txt。验收标准：domo_mock/T1.txt 存在。标记 INVOKE-CLI-POSITIONAL。",
    )
    assert_usage(job, usage, key)

    entries = read_log(log_path)
    assert len(entries) == 2, f"{key} 调用次数异常: {entries}"
    session = derived_session_id(job["id"])
    for index, entry in enumerate(entries):
        args = entry["args"]
        assert entry["kind"] == "cli", entry
        assert entry["prompt_chars"] == 0, f"{key} 位置参数时 stdin 应为空: {entry}"
        assert entry["prompt"] == "", entry
        assert args[0] == "--prompt", args
        prompt = args[1]
        assert args[2:] == [
            "--session",
            session,
            "--job-dir",
            entry["job_dir"],
            "--cwd",
            entry["cwd"],
            "--model",
            model,
            "--opt-model",
            model,
            "--opt-session",
            session,
            f"--inline=prefix/{model}/suffix",
        ], f"{key} 占位符展开异常: {args}"
        if index == 0:
            assert "INVOKE-CLI-POSITIONAL" in prompt, prompt
            assert "结果文件" in prompt, prompt
        else:
            assert "继续" in prompt and "派生子任务" in prompt, prompt
            assert "INVOKE-CLI-POSITIONAL" not in prompt, prompt
    return job


def run_cli_stdin(temp, script):
    key = "cli-stdin"
    log_path = temp / "cli-stdin.jsonl"
    model = "cli/stdin-model"
    model_args = ["--opt-model", "{model}", "--opt-session", "{session_id}"]
    command = [
        sys.executable,
        str(script),
        "--job-dir",
        "{job_dir}",
        "--cwd",
        "{cwd}",
        "--session",
        "{session_id}",
        "--tag",
        "stdin",
    ]
    usage = {
        "input_tokens": 210,
        "output_tokens": 43,
        "cache_read_tokens": 6,
        "cache_write_tokens": 7,
        "reasoning_tokens": 8,
    }
    job = drive_case(
        temp,
        key,
        {
            "name": key,
            "kind": "cli",
            "model": model,
            "roles": ["executor"],
            "max_concurrency": 1,
            "command": command,
            "model_args": model_args,
        },
        {
            "FAKE_LOG": str(log_path),
            "FAKE_STATE": str(temp / "cli-stdin-state"),
            "FAKE_USAGE": json.dumps(usage),
        },
        "[single] 创建 domo_mock/T1.txt。验收标准：domo_mock/T1.txt 存在。标记 INVOKE-CLI-STDIN。",
    )
    assert_usage(job, usage, key)

    entries = read_log(log_path)
    assert len(entries) == 2, f"{key} 调用次数异常: {entries}"
    session = derived_session_id(job["id"])
    for index, entry in enumerate(entries):
        args = entry["args"]
        assert entry["prompt_chars"] > 0, f"{key} 未从 stdin 读取提示词: {entry}"
        assert entry["prompt_chars"] == len(entry["prompt"]), entry
        assert args == [
            "--job-dir",
            entry["job_dir"],
            "--cwd",
            entry["cwd"],
            "--session",
            session,
            "--tag",
            "stdin",
            # 模板未引用 {model} 或 {model_args}，配置的模型参数追加在末尾。
            "--opt-model",
            model,
            "--opt-session",
            session,
        ], f"{key} 占位符展开异常: {args}"
        if index == 0:
            assert "INVOKE-CLI-STDIN" in entry["prompt"], entry["prompt"]
        else:
            assert "继续" in entry["prompt"], entry["prompt"]
            assert "派生子任务" in entry["prompt"], entry["prompt"]
            assert "INVOKE-CLI-STDIN" not in entry["prompt"], entry["prompt"]
    return job


def run_opencode(temp, script):
    key = "opencode"
    log_path = temp / "opencode.jsonl"
    model = "opencode/fake-model"
    job = drive_case(
        temp,
        key,
        {
            "name": key,
            "kind": "opencode",
            "model": model,
            "roles": ["executor"],
            "max_concurrency": 1,
        },
        {
            "DOMO_OPENCODE_BIN": str(script),
            "FAKE_LOG": str(log_path),
            "FAKE_STATE": str(temp / "opencode-state"),
        },
        "[single] 创建 domo_mock/T1.txt。验收标准：domo_mock/T1.txt 存在。标记 INVOKE-OPENCODE。",
    )
    expected_usage = {
        # 账本根会话：input 1000 + cache.read 40 + cache.write 10；
        # output 200 + reasoning 30。
        "input_tokens": 1050,
        "output_tokens": 230,
        "cache_read_tokens": 40,
        "cache_write_tokens": 10,
        "reasoning_tokens": 30,
        "cost": 0.75,
    }
    assert_usage(job, expected_usage, key)

    entries = read_log(log_path)
    runs = [entry for entry in entries if entry["kind"] == "opencode"]
    usage_calls = [entry for entry in entries if entry["kind"] == "opencode-usage"]
    assert len(runs) == 2, f"{key} 调用次数异常: {entries}"
    session = opencode_session_id(job["id"])
    for index, entry in enumerate(runs):
        assert entry["args"] == [
            "run",
            "--auto",
            "--thinking",
            "--format",
            "json",
            "--session",
            session,
            "--title",
            f"DOMO executor {job['id']}",
            "--model",
            model,
        ], f"{key} 参数异常: {entry['args']}"
        assert entry["session"] == session, entry
        assert entry["title"] == f"DOMO executor {job['id']}", entry
        assert entry["model"] == model, entry
        assert entry["role"] == "executor", entry
        assert entry["prompt_chars"] > 0, f"{key} 未从 stdin 读取提示词: {entry}"
        assert entry["prompt_chars"] == len(entry["prompt"]), entry
        assert not any(
            "INVOKE-OPENCODE" in arg for arg in entry["args"]
        ), f"{key} 提示词不应出现在命令行: {entry['args']}"
        if index == 0:
            assert "INVOKE-OPENCODE" in entry["prompt"], entry["prompt"]
            assert "结果文件" in entry["prompt"], entry["prompt"]
        else:
            assert "继续" in entry["prompt"], entry["prompt"]
            assert "INVOKE-OPENCODE" not in entry["prompt"], entry["prompt"]
    assert len(usage_calls) == 1, f"{key} 用量查询次数异常: {entries}"
    assert usage_calls[0]["session"] == session, usage_calls
    return job


def main():
    with temp_root("domo-agent-invocation-") as temp:
        bin_dir = temp / "bin"
        bin_dir.mkdir()
        cli_script = bin_dir / "fake_cli.py"
        cli_script.write_text(CLI_FAKE, encoding="utf-8")
        opencode_script = bin_dir / "fake_opencode.py"
        opencode_script.write_text(OPENCODE_FAKE, encoding="utf-8")
        opencode_launcher = write_python_launcher(opencode_script)

        cli_positional = run_cli_positional(temp, cli_script)
        cli_stdin = run_cli_stdin(temp, cli_script)
        opencode_job = run_opencode(temp, opencode_launcher)

        print("自定义 CLI 与 OpenCode 调用契约测试通过")
        print(
            "  cli positional: 作业 {id} session {session}".format(
                id=cli_positional["id"], session=derived_session_id(cli_positional["id"])
            )
        )
        print(
            "  cli stdin:      作业 {id} session {session}".format(
                id=cli_stdin["id"], session=derived_session_id(cli_stdin["id"])
            )
        )
        print(
            "  opencode:       作业 {id} session {session}".format(
                id=opencode_job["id"], session=opencode_session_id(opencode_job["id"])
            )
        )


if __name__ == "__main__":
    sys.exit(main())
