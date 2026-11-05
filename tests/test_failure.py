#!/usr/bin/env python3
"""DOMO 失败与超时恢复测试。

场景：
  1. 执行者连续超时，任务在 3 次失败后阻塞，项目进入人工处理状态；
  2. 重启服务并降低执行延迟后，人工重试失败作业；
  3. 项目继续完成并产出报告。

使用内置 mock 规划/验收和可终止的脚本执行者，不访问外网；临时数据隔离。
退出码 0 表示全部通过。
"""

import time
import sys

from harness import (
    Server,
    call as api,
    ensure_server_built,
    git,
    init_repo,
    temp_root,
    wait_for,
)


def main():
    with temp_root("domo-failure-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        delay = temp / "delay"
        delay.write_text("2.5")
        executor = temp / "executor.py"
        executor.write_text('''import json, os, pathlib, sys, time
time.sleep(float(pathlib.Path(sys.argv[1]).read_text()))
job = pathlib.Path(os.environ["DOMO_JOB_DIR"])
context = json.loads((job / "context.json").read_text())
key = context["task"]["key"]
out = pathlib.Path("domo_mock") / (key + ".txt")
out.parent.mkdir(exist_ok=True); out.write_text(key)
(job / "result.json").write_text(json.dumps({"kind":"done", "summary":"delivered", "commit_files":[str(out).replace("\\\\", "/")]}))
''')
        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_JOB_TIMEOUT_SECS": "1",
                "DOMO_MOCK_DELAY_MS": "50",
            },
        ) as server:
            pending_goals = {}

            def call(method, path, data=None):
                if method == "POST" and path == "/api/projects" and isinstance(data, dict) and "goal" in data:
                    goal = data.get("goal")
                    payload = {key: value for key, value in data.items() if key != "goal"}
                    result = api(server.base, "POST", path, payload)
                    project_id = result["project"]["id"]
                    pending_goals[project_id] = goal
                    return result
                if (
                    method == "POST"
                    and path.startswith("/api/projects/")
                    and path.endswith("/confirm")
                    and not path.endswith("/analysis/confirm")
                ):
                    project_id = path.split("/")[-2]
                    detail = api(server.base, "GET", f"/api/projects/{project_id}")
                    if not detail["messages"] and project_id in pending_goals:
                        api(
                            server.base,
                            "POST",
                            f"/api/projects/{project_id}/analysis/messages",
                            {
                                "message": pending_goals.pop(project_id),
                                "confirm_mode": "auto",
                            },
                        )
                    message = (data or {}).get("message")
                    if message:
                        api(
                            server.base,
                            "POST",
                            f"/api/projects/{project_id}/analysis/messages",
                            {"message": message, "confirm_mode": "auto"},
                        )
                    deadline = time.time() + 30
                    while time.time() < deadline:
                        detail = api(server.base, "GET", f"/api/projects/{project_id}")
                        if detail["project"]["stage"] != "analysis":
                            return detail
                        if detail.get("proposal") and not detail["jobs"]:
                            return api(
                                server.base,
                                "POST",
                                f"/api/projects/{project_id}/analysis/confirm",
                                {},
                            )
                        last = detail["messages"][-1] if detail["messages"] else None
                        if last and last["role"] == "planner" and not detail.get("proposal"):
                            return detail
                        time.sleep(0.1)
                    return api(server.base, "GET", f"/api/projects/{project_id}")
                return api(server.base, method, path, data)

            def wait_project(project_id, predicate, label, timeout=60):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Timeout Mock",
                    "kind": "mock",
                    "roles": ["planner", "verifier"],
                    "max_concurrency": 2,
                },
            )
            call("POST", "/api/agents", {"name":"Timed Executor", "kind":"cli", "roles":["executor"],
                "command":[sys.executable, str(executor), str(delay)]})

            # 新建项目的项目池继承默认团队。
            project = call(
                "POST",
                "/api/projects",
                {
                    "name": "Timeout",
                    "goal": "验证超时与人工重试",
                    "repo_path": str(repo),
                    "base_branch": "main",
                },
            )["project"]
            call("PUT", f"/api/projects/{project['id']}/limits", {"task_attempt_limit": 3})
            call("POST", f"/api/projects/{project['id']}/confirm", {})

            detail = wait_project(
                project["id"],
                lambda item: item["project"]["status"] == "blocked",
                "超时阻塞",
            )

            task = detail["tasks"][0]
            assert task["attempts"] == 3, task
            assert "尝试" in (task["last_error"] or ""), task
            assert any(event["kind"] == "task_state_changed" for event in detail["events"])
            failed_jobs = [job for job in detail["jobs"] if job["status"] == "failed"]
            assert failed_jobs, detail["jobs"]
            for job in failed_jobs:
                summary = job.get("failure_summary")
                assert summary and summary.get("message"), job
                assert len(summary["message"]) <= 300, job
                assert job.get("error") and "超时" in job["error"], job

            project_id = project["id"]

            # 重启服务并缩短执行延迟。
            server.restart()
            delay.write_text("0.01")

            detail = call("GET", f"/api/projects/{project_id}")
            blocked_task = next(task for task in detail["tasks"] if task["status"] == "blocked")
            failed_jobs = [
                job
                for job in detail["jobs"]
                if job["task_id"] == blocked_task["id"] and job["status"] == "failed"
            ]
            job = max(failed_jobs, key=lambda item: item["created_at"])
            # 用户显式提高累计尝试上限，并解除该 Agent 的测试冷却以模拟时间恢复。
            import sqlite3
            with sqlite3.connect(server.root / "data" / "domo.db") as db:
                db.execute("UPDATE agents SET cooldown_until = NULL WHERE id = ?", (job["agent_id"],))
            call("PUT", f"/api/projects/{project_id}/limits", {"task_attempt_limit": 4})

            detail = wait_project(
                project_id,
                lambda item: item["project"]["stage"] == "summarizing"
                and item["project"]["report"],
                "重试后完成",
                90,
            )

            assert all(task["stage"] == "completed" for task in detail["tasks"]), detail["tasks"]
            call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
            detail = call("GET", f"/api/projects/{project_id}")
            assert detail["project"]["stage"] == "completed", detail["project"]

            branch = detail["project"]["branch"]
            git(["show", f"{branch}:domo_mock/T1.txt"], repo)
            git(["show", f"{branch}:domo_mock/T2.txt"], repo)

            print("超时与人工重试测试通过")


if __name__ == "__main__":
    main()
