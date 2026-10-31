#!/usr/bin/env python3
"""DOMO Codex 与 WorkBuddy 会话复用测试。

覆盖范围：
  1. Codex 首次运行没有写 result.json 时，继续轮次使用
     `codex exec resume <thread_id>`，而不是重新开一个会话。
  2. WorkBuddy 首次运行没有写 result.json 时，继续轮次使用
     `--resume <session_id>`，而不是重新开一个会话。
  3. 继续提示词只包含“继续”和执行者的派生子任务提示。

使用脚本化 CLI 与内置 mock Agent，不访问外网；临时数据全部放在系统临时
目录。退出码 0 表示全部通过。
"""

import json
import sys
from pathlib import Path

from harness import Server, init_repo, temp_root, wait_for, write_python_launcher


FAKE_AGENT = """\
import json
import os
import pathlib
import sys

args = sys.argv[1:]
kind = "opencode" if args and args[0] == "run" else (
    "codex" if args and args[0] == "exec" else "workbuddy"
)

def option(name):
    if name not in args:
        return None
    return args[args.index(name) + 1]

if kind == "opencode":
    session = option("--session") or ""
    model = option("--model") or ""
    resume = False
    prompt = args[-1]
elif kind == "codex":
    resume = len(args) > 1 and args[1] == "resume"
    session = "thread-1"
    model = option("--model") or ""
    prompt = sys.stdin.read()
else:
    resume = "--resume" in args
    session = option("--resume") or "session-1"
    model = option("--model") or ""
    prompt = sys.stdin.read()

state_dir = pathlib.Path(os.environ["FAKE_STATE"])
state_dir.mkdir(parents=True, exist_ok=True)
log_path = pathlib.Path(os.environ["FAKE_LOG"])
with log_path.open("a", encoding="utf-8") as handle:
    handle.write(json.dumps({
        "kind": kind,
        "resume": resume,
        "session": session,
        "model": model,
        "prompt": prompt,
        "args": args,
    }, ensure_ascii=False) + "\\n")

marker = state_dir / session
if not marker.exists():
    marker.write_text("1", encoding="utf-8")
    if kind == "codex":
        print(json.dumps({"type": "thread.started", "thread_id": session}), flush=True)
    elif kind == "workbuddy":
        print(json.dumps({
            "type": "system",
            "subtype": "init",
            "session_id": session,
        }), flush=True)
    sys.exit(0)

role = os.environ.get("DOMO_JOB_ROLE", "")
job_dir = pathlib.Path(os.environ["DOMO_JOB_DIR"])
if role == "executor":
    artifact = pathlib.Path(os.getcwd()) / "domo_mock" / "T1.txt"
    artifact.parent.mkdir(parents=True, exist_ok=True)
    artifact.write_text("T1", encoding="utf-8")
    result = {"kind": "done", "summary": "继续后完成"}
elif role == "verifier":
    result = {"pass": True, "reason": "产物齐全", "issues": []}
elif role == "analysis":
    result = {
        "kind": "plan",
        "summary": "脚本化方案",
        "assumptions": [],
        "acceptance": ["产物存在"],
        "tasks": [{
            "key": "T1",
            "title": "写入文件",
            "description": "写入 domo_mock/T1.txt。",
            "acceptance": ["domo_mock/T1.txt 存在"],
            "deps": [],
        }],
        "skip_confirmation": True,
    }
elif role == "summary":
    result = {"kind": "report", "report": "# 报告\\n\\n完成。"}
else:
    result = {"kind": "done", "summary": "完成"}
(job_dir / "result.json").write_text(
    json.dumps(result, ensure_ascii=False), encoding="utf-8"
)
"""


def read_log(path):
    path = Path(path)
    if not path.exists():
        return []
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def create_agent(call, name, roles, kind="mock", model=None):
    payload = {"name": name, "kind": kind, "roles": roles, "max_concurrency": 2}
    if model:
        payload["model"] = model
    return call("POST", "/api/agents", payload)["id"]


def run_case(temp, repo, env, executor_kind, label):
    with Server(temp, env=env) as server:
        def call(method, path, data=None):
            return server.call(method, path, data)

        def wait_project(project_id, predicate, name, timeout=90):
            def check():
                detail = call("GET", f"/api/projects/{project_id}")
                return detail if predicate(detail) else None

            return wait_for(check, name, timeout)

        for agent in call("GET", "/api/agents"):
            call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
        create_agent(call, "Mock Planner", ["planner", "verifier"])
        create_agent(call, f"{label} Executor", ["executor"], executor_kind, "fake")

        project = call(
            "POST",
            "/api/projects",
            {"name": label, "repo_path": str(repo), "base_branch": "main"},
        )["project"]
        call(
            "POST",
            f"/api/projects/{project['id']}/analysis/messages",
            {"message": "[single] 验证会话复用", "confirm_mode": "auto"},
        )
        detail = wait_project(
            project["id"],
            lambda item: bool(item["tasks"])
            and all(task["stage"] == "completed" for task in item["tasks"]),
            f"{label} 任务完成",
        )
        return detail, read_log(env["FAKE_LOG"])


def main():
    with temp_root("domo-session-reuse-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        fake = temp / "fake_agent.py"
        fake.write_text(FAKE_AGENT, encoding="utf-8")
        launcher = write_python_launcher(fake)

        base_env = {
            "DOMO_MOCK_DELAY_MS": "10",
            "DOMO_MOCK_EXECUTOR_DELAY_MS": "10",
        }

        codex_log = temp / "codex.jsonl"
        detail, entries = run_case(
            temp,
            repo,
            {
                **base_env,
                "DOMO_CODEX_BIN": str(launcher),
                "FAKE_LOG": str(codex_log),
                "FAKE_STATE": str(temp / "codex-state"),
            },
            "codex",
            "Codex",
        )
        executor_jobs = [job for job in detail["jobs"] if job["role"] == "executor"]
        assert executor_jobs and all(job["status"] == "succeeded" for job in executor_jobs), executor_jobs
        assert len(entries) == 2, entries
        assert entries[0]["kind"] == "codex" and not entries[0]["resume"], entries[0]
        assert entries[1]["resume"], entries[1]
        assert "thread-1" in entries[1]["args"], entries[1]
        assert "继续" in entries[1]["prompt"], entries[1]
        assert "派生子任务" in entries[1]["prompt"], entries[1]

        workbuddy_log = temp / "workbuddy.jsonl"
        detail, entries = run_case(
            temp,
            repo,
            {
                **base_env,
                "DOMO_WORKBUDDY_BIN": str(launcher),
                "FAKE_LOG": str(workbuddy_log),
                "FAKE_STATE": str(temp / "workbuddy-state"),
            },
            "workbuddy",
            "WorkBuddy",
        )
        executor_jobs = [job for job in detail["jobs"] if job["role"] == "executor"]
        assert executor_jobs and all(job["status"] == "succeeded" for job in executor_jobs), executor_jobs
        assert len(entries) == 2, entries
        assert entries[0]["kind"] == "workbuddy" and not entries[0]["resume"], entries[0]
        assert entries[1]["resume"], entries[1]
        assert entries[1]["session"] == "session-1", entries[1]
        assert "继续" in entries[1]["prompt"], entries[1]
        assert "派生子任务" in entries[1]["prompt"], entries[1]

        print("Codex 与 WorkBuddy 会话复用测试通过")


if __name__ == "__main__":
    sys.exit(main())
