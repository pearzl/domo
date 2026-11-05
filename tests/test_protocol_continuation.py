#!/usr/bin/env python3
"""DOMO 结果协议继续与协议失败阻塞测试。

覆盖范围：
  1. Agent 进程退出码为 0 但没有写 result.json 时，DOMO 在同一作业、
     同一 OpenCode session 内追加包含缺失原因、当前契约和结果路径的提示；
     Agent 只有收到这些信息才实际写入结果。
   2. 同一作业最多追加 5 轮继续提示；5 轮后仍没有结果时直接协议失败并
      阻塞任务，不切换到下一优先级 Agent。
   3. 非法结果携带具体校验错误在同一会话内纠错；耗尽预算后直接阻塞。
   4. 合法任务失败正文中的 provider、HTTP 状态码等不影响 Agent 健康；
      人工重试携带上次错误和当前输出契约。

使用脚本化 OpenCode 假 CLI 与内置 mock Agent，不访问外网；临时数据全部
放在系统临时目录。退出码 0 表示全部通过。
"""

import json
import sys
from pathlib import Path

from harness import Server, init_repo, temp_root, wait_for, write_python_launcher


FAKE_OPENCODE = """\
import json
import os
import pathlib
import sys

args = sys.argv[1:]
if not args or args[0] != "run":
    sys.exit(1)

def option(name):
    index = args.index(name)
    return args[index + 1]

session = option("--session")
model = option("--model") if "--model" in args else ""
prompt = sys.stdin.read()
role = os.environ.get("DOMO_JOB_ROLE", "")

state_dir = pathlib.Path(os.environ["FAKE_OPENCODE_STATE"])
state_dir.mkdir(parents=True, exist_ok=True)
log_path = pathlib.Path(os.environ["FAKE_OPENCODE_LOG"])
with log_path.open("a", encoding="utf-8") as handle:
    handle.write(json.dumps({
        "session": session,
        "model": model,
        "role": role,
        "prompt": prompt,
    }, ensure_ascii=False) + "\\n")

marker = state_dir / session
job_dir = pathlib.Path(os.environ["DOMO_JOB_DIR"])
if model == "task-failure":
    result = {"kind": "failed", "reason": "验收未完成：SDK credentials provider 含 session token；HTTP 429 quota network timeout；aws.rs:401-403"}
    (job_dir / "result.json").write_text(json.dumps(result), encoding="utf-8")
    sys.exit(0)
if model in ("invalid", "invalid-never"):
    if not marker.exists() or model == "invalid-never":
        marker.write_text("1", encoding="utf-8")
        (job_dir / "result.json").write_text('{"kind":"derive","tasks":[{"key":"T1.9","title":"bad","description":"bad","acceptance":["bad"],"deps":["T1"]}]}', encoding="utf-8")
        sys.exit(0)
    assert "具体校验错误" in prompt and "本批次以外" in prompt and "T1.9" in prompt, prompt
    artifact = pathlib.Path.cwd() / "domo_mock" / "T1.txt"
    artifact.parent.mkdir(parents=True, exist_ok=True)
    artifact.write_text("T1", encoding="utf-8")
    (job_dir / "result.json").write_text('{"kind":"done","summary":"协议纠错后完成","commit_files":["domo_mock/T1.txt"]}', encoding="utf-8")
    sys.exit(0)
if not marker.exists():
    marker.write_text("1", encoding="utf-8")
    print("任务已完成，结果文件已写入，等待验收。", flush=True)
    sys.exit(0)

if model == "never":
    sys.exit(0)

job_dir = pathlib.Path(os.environ["DOMO_JOB_DIR"])
assert "未能读取结果文件" in prompt, prompt
assert str(job_dir / "result.json") in prompt, prompt
assert "当前有效的任务与输出契约" in prompt, prompt
if role == "executor":
    artifact = pathlib.Path(os.getcwd()) / "domo_mock" / "T1.txt"
    artifact.parent.mkdir(parents=True, exist_ok=True)
    artifact.write_text("T1", encoding="utf-8")
    result = {"kind": "done", "summary": "继续后完成", "commit_files": ["domo_mock/T1.txt"]}
elif role == "verifier":
    context = json.loads((job_dir / "context.json").read_text(encoding="utf-8"))
    result = {"pass": True, "reason": "产物齐全", "issues": [], "evidence": {
        "contract_id": context["contract"]["id"], "artifact_version": context["artifact_version"],
        "artifacts": ["domo_mock/T1.txt"], "risks": [], "human_checks": [],
        "criteria": [{"criterion": item, "passed": True, "checks": [{"method": "检查产物文件", "result": "存在"}]}
                     for item in context["task"]["acceptance"]]}}
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
elif role == "project_acceptance":
    result = {
        "kind": "project_acceptance",
        "summary": "项目验收通过",
        "next_action": "report", "report": "# 报告：完成",
        "criteria": [{
            "id": "A1",
            "source": "proposal_acceptance",
            "criterion": "产物存在",
            "verdict": "met",
            "evidence": ["file: domo_mock/T1.txt"],
            "gap": "",
        }],
        "replan_reason": "",
    }
elif role == "summary":
    result = {"kind": "report", "report": "# 报告\\n\\n完成。"}
else:
    result = {"kind": "done", "summary": "完成", "commit_files": []}
if role == "project_acceptance":
    context = json.loads((job_dir / "context.json").read_text(encoding="utf-8"))
    result["contract_id"] = context["contract"]["id"]
    result["artifact_version"] = context["artifact_version"]
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


def create_agent(call, name, roles, model=None):
    payload = {
        "name": name,
        "kind": "opencode" if model else "mock",
        "roles": roles,
        "max_concurrency": 2,
    }
    if model:
        payload["model"] = model
    return call("POST", "/api/agents", payload)["id"]


def run_case(temp, repo, env, agents, label, predicate=None):
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
        for agent in agents:
            create_agent(call, **agent)

        project = call(
            "POST",
            "/api/projects",
            {"name": label, "repo_path": str(repo), "base_branch": "main"},
        )["project"]
        call(
            "POST",
            f"/api/projects/{project['id']}/analysis/messages",
            {"message": "[single] 验证协议继续", "confirm_mode": "auto"},
        )
        check = predicate or (
            lambda item: bool(item["tasks"])
            and all(task["stage"] == "completed" for task in item["tasks"])
        )
        detail = wait_project(project["id"], check, f"{label} 结束")
        return detail, read_log(env["FAKE_OPENCODE_LOG"])


def main():
    with temp_root("domo-protocol-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        fake = temp / "fake_opencode.py"
        fake.write_text(FAKE_OPENCODE, encoding="utf-8")
        launcher = write_python_launcher(fake)

        base_env = {
            "DOMO_OPENCODE_BIN": str(launcher),
            "DOMO_MOCK_DELAY_MS": "10",
            "DOMO_MOCK_EXECUTOR_DELAY_MS": "10",
        }

        # 1. 同一 session 追加继续提示后成功。
        log1 = temp / "opencode-1.jsonl"
        state1 = temp / "state-1"
        env1 = {
            **base_env,
            "FAKE_OPENCODE_LOG": str(log1),
            "FAKE_OPENCODE_STATE": str(state1),
        }
        detail, entries = run_case(
            temp,
            repo,
            env1,
            [
                {"name": "Mock Planner", "roles": ["planner", "verifier"]},
                {"name": "Continue Executor", "roles": ["executor"], "model": "continue"},
            ],
            "协议继续",
        )
        executor_jobs = [job for job in detail["jobs"] if job["role"] == "executor"]
        assert executor_jobs and all(job["status"] == "succeeded" for job in executor_jobs), executor_jobs
        assert any(event["kind"] == "job_continued" for event in detail["events"]), detail["events"]

        executor_entries = [entry for entry in entries if entry["role"] == "executor"]
        assert len(executor_entries) == 2, executor_entries
        assert executor_entries[0]["session"] == executor_entries[1]["session"], executor_entries
        assert "继续" in executor_entries[1]["prompt"], executor_entries[1]
        assert "派生子任务" in executor_entries[1]["prompt"], executor_entries[1]
        assert "未能读取结果文件" in executor_entries[1]["prompt"], executor_entries[1]

        # 2. 最多 5 轮继续后仍无结果：协议失败并阻塞，不切换 Agent。
        log2 = temp / "opencode-2.jsonl"
        state2 = temp / "state-2"
        env2 = {
            **base_env,
            "FAKE_OPENCODE_LOG": str(log2),
            "FAKE_OPENCODE_STATE": str(state2),
        }
        detail, entries = run_case(
            temp,
            repo,
            env2,
            [
                {"name": "Mock Planner", "roles": ["planner", "verifier"]},
                {"name": "Never Executor", "roles": ["executor"], "model": "never"},
                {"name": "Backup Executor", "roles": ["executor"], "model": "continue"},
            ],
            "协议阻塞",
            lambda item: bool(item["tasks"]) and item["tasks"][0]["status"] == "blocked",
        )
        agents = {agent["id"]: agent["name"] for agent in detail["agents"]}
        executor_jobs = [job for job in detail["jobs"] if job["role"] == "executor"]
        failed = [
            job
            for job in executor_jobs
            if job["status"] == "failed" and agents.get(job["agent_id"]) == "Never Executor"
        ]
        backup_jobs = [
            job
            for job in executor_jobs
            if agents.get(job["agent_id"]) == "Backup Executor"
        ]
        assert len(failed) == 1, failed
        assert failed[0]["failure_kind"] == "protocol", failed[0]
        assert "没有写 result.json" in (failed[0]["error"] or ""), failed[0]
        assert not backup_jobs, backup_jobs
        assert any(
            event["kind"] == "task_blocked" and "协议失败" in (event["message"] or "")
            for event in detail["events"]
        ), detail["events"]

        never_entries = [entry for entry in entries if entry["model"] == "never"]
        backup_entries = [entry for entry in entries if entry["model"] == "continue"]
        assert len(never_entries) == 6, never_entries
        assert not backup_entries, backup_entries
        assert all(entry["session"] == never_entries[0]["session"] for entry in never_entries)
        assert all("继续" in entry["prompt"] for entry in never_entries[1:]), never_entries[1:]

        # 3. 非法依赖与缺少结果使用同一有界纠错机制。
        for model in ("invalid", "invalid-never", "task-failure"):
            case_root = temp / model
            case_root.mkdir()
            env = {
                **base_env,
                "FAKE_OPENCODE_LOG": str(case_root / "calls.jsonl"),
                "FAKE_OPENCODE_STATE": str(case_root / "state"),
            }
            blocked = model != "invalid"
            detail, entries = run_case(
                case_root, repo, env,
                [
                    {"name": "Mock Planner", "roles": ["planner", "verifier"]},
                    {"name": "Test Executor", "roles": ["executor"], "model": model},
                ],
                model,
                (lambda item: bool(item["tasks"]) and item["tasks"][0]["status"] == "blocked") if blocked else None,
            )
            jobs = [job for job in detail["jobs"] if job["role"] == "executor"]
            assert not any(task["parent_id"] for task in detail["tasks"]), detail["tasks"]
            calls = [entry for entry in entries if entry["role"] == "executor"]
            agent = next(agent for agent in detail["agents"] if agent["name"] == "Test Executor")
            assert agent["transient_failures"] == 0 and agent["cooldown_until"] is None, agent
            if model == "invalid":
                assert len(jobs) == 1 and jobs[0]["status"] == "succeeded", jobs
                assert len(calls) == 2 and calls[0]["session"] == calls[1]["session"], calls
            elif model == "invalid-never":
                assert len(jobs) == 1 and jobs[0]["failure_kind"] == "protocol", jobs
                assert len(calls) == 6 and len({entry["session"] for entry in calls}) == 1, calls
            else:
                assert len(jobs) == 3 and all(job["failure_kind"] == "task" for job in jobs), jobs
                assert all(job["failure_summary"]["kind"] == "task" for job in jobs), jobs
                assert not any(event["kind"] == "job_continued" for event in detail["events"]), detail["events"]
                assert "SDK credentials provider" in calls[1]["prompt"], calls[1]

        print("结果协议继续与协议失败阻塞测试通过")


if __name__ == "__main__":
    sys.exit(main())
