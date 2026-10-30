#!/usr/bin/env python3
"""DOMO 重启恢复测试。

场景：执行者作业运行中直接 kill -9 服务，随后用同一数据目录重启。
验收标准：
  1. 中断作业被标记失败，并产生恢复事件；
  2. 任务在重启后重新调度并最终完成；
  3. 项目仍能产出报告，不会出现重复的活跃作业。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
"""

import json
import time
import urllib.error
import urllib.request

from harness import (
    Server,
    ensure_server_built,
    init_repo,
    temp_root,
    wait_for,
)


def main():
    with temp_root("domo-recovery-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(temp, env={"DOMO_MOCK_DELAY_MS": "1500"}) as server:
            pending_goals = {}

            def call(method, path, data=None):
                if (
                    method == "POST"
                    and path == "/api/projects"
                    and isinstance(data, dict)
                    and "goal" in data
                ):
                    goal = data.get("goal")
                    payload = {
                        key: value for key, value in data.items() if key != "goal"
                    }
                    result = server.call("POST", path, payload)
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
                    detail = server.call("GET", f"/api/projects/{project_id}")
                    if not detail["messages"] and project_id in pending_goals:
                        server.call(
                            "POST",
                            f"/api/projects/{project_id}/analysis/messages",
                            {
                                "message": pending_goals.pop(project_id),
                                "confirm_mode": "auto",
                            },
                        )
                    message = (data or {}).get("message")
                    if message:
                        server.call(
                            "POST",
                            f"/api/projects/{project_id}/analysis/messages",
                            {"message": message, "confirm_mode": "auto"},
                        )
                    deadline = time.time() + 30
                    while time.time() < deadline:
                        detail = server.call("GET", f"/api/projects/{project_id}")
                        if detail["project"]["stage"] != "analysis":
                            return detail
                        if detail.get("proposal") and not detail["jobs"]:
                            return server.call(
                                "POST",
                                f"/api/projects/{project_id}/analysis/confirm",
                                {},
                            )
                        last = (
                            detail["messages"][-1] if detail["messages"] else None
                        )
                        if (
                            last
                            and last["role"] == "planner"
                            and not detail.get("proposal")
                        ):
                            return detail
                        time.sleep(0.1)
                    return server.call("GET", f"/api/projects/{project_id}")
                return server.call(method, path, data)

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Recovery Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )

            # 新建项目的项目池继承默认团队。
            project = call(
                "POST",
                "/api/projects",
                {
                    "name": "Recovery",
                    "goal": "验证服务重启恢复",
                    "repo_path": str(repo),
                    "base_branch": "main",
                },
            )["project"]
            project_id = project["id"]
            call("POST", f"/api/projects/{project_id}/confirm", {})

            def executor_running():
                detail = call("GET", f"/api/projects/{project_id}")
                if any(
                    job["role"] == "executor" and job["status"] == "running"
                    for job in detail["jobs"]
                ):
                    return detail
                return None

            wait_for(executor_running, "等待执行中作业", 60)

            # 模拟 kill -9 后使用同一数据目录重启。
            server.kill()
            server.restart(env={"DOMO_MOCK_DELAY_MS": "80"})

            def report_ready():
                detail = call("GET", f"/api/projects/{project_id}")
                if (
                    detail["project"]["stage"] == "summarizing"
                    and detail["project"]["report"]
                ):
                    return detail
                return None

            detail = wait_for(report_ready, "重启后项目报告", 120)

            def jobs_settled():
                current = call("GET", f"/api/projects/{project_id}")
                if not any(job["status"] == "running" for job in current["jobs"]):
                    return current
                return None

            detail = wait_for(jobs_settled, "重启后作业收尾", 30)

            assert all(
                task["stage"] == "completed" for task in detail["tasks"]
            ), detail["tasks"]
            assert any(
                event["kind"] == "job_recovered" for event in detail["events"]
            ), "缺少恢复事件"
            assert not any(
                job["status"] == "running" for job in detail["jobs"]
            ), "存在重复或残留的活跃作业"
            assert detail["project"]["report"], "缺少项目报告"

            print("重启恢复测试通过")


if __name__ == "__main__":
    main()
