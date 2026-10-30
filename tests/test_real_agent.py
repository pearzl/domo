#!/usr/bin/env python3
"""DOMO 真实 Agent 自迭代验收。

只有在设置 DOMO_RT_MODEL 时运行，例如：
  DOMO_RT_MODEL=<provider/model> python tests/test_real_agent.py

场景：把仓库临时副本交给 DOMO，用真实 opencode Agent 完成一个最小的、
可客观验收的功能；验证规划、执行、验收、合并、报告与人工确认全链路。
不依赖固定的模型名，不访问除模型服务外的外部资源。
"""

import os
import time

from harness import (
    Server,
    ensure_server_built,
    git,
    init_repo,
    temp_root,
)


def main():
    model = os.environ.get("DOMO_RT_MODEL")
    if not model:
        print("跳过：未设置 DOMO_RT_MODEL（真实模型用例）")
        return

    with temp_root("domo-real-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        (repo / "README.md").write_text("# Real agent test\n", encoding="utf-8")
        git(["add", "."], repo)
        git(
            [
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                "init",
            ],
            repo,
        )

        ensure_server_built()

        print(f"==> 启动 DOMO（真实 Agent，模型 {model}）")
        with Server(
            temp,
            env={
                "DOMO_JOB_TIMEOUT_SECS": os.environ.get("DOMO_JOB_TIMEOUT_SECS", "900"),
            },
        ) as server:
            pending_goals = {}

            def call(method, path, data=None):
                if (
                    method == "POST"
                    and path == "/api/projects"
                    and isinstance(data, dict)
                    and "goal" in data
                ):
                    goal = data.get("goal")
                    payload = {key: value for key, value in data.items() if key != "goal"}
                    result = server.call("POST", path, payload)
                    pending_goals[result["project"]["id"]] = goal
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
                        last = detail["messages"][-1] if detail["messages"] else None
                        if last and last["role"] == "planner" and not detail.get("proposal"):
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
                    "name": "Real OpenCode",
                    "kind": "opencode",
                    "model": model,
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )
            # 新建项目的项目池继承默认团队。

            project = call(
                "POST",
                "/api/projects",
                {
                    "name": "Real agent",
                    "goal": (
                        "在仓库根目录创建 hello.txt，内容为一行 hello domo。"
                        "不要修改其他文件。验收标准：hello.txt 存在且内容为 hello domo。"
                    ),
                    "repo_path": str(repo),
                    "base_branch": "main",
                },
            )["project"]
            project_id = project["id"]
            call("POST", f"/api/projects/{project_id}/confirm", {})

            deadline = time.time() + 1200
            detail = None
            while time.time() < deadline:
                detail = call("GET", f"/api/projects/{project_id}")
                state = detail["project"]
                if state["stage"] == "analysis" and state["status"] == "blocked":
                    call(
                        "POST",
                        f"/api/projects/{project_id}/confirm",
                        {"message": "没有额外要求，请直接规划并执行。"},
                    )
                if state["stage"] == "summarizing" and state["report"]:
                    break
                if state["status"] == "blocked" and state["stage"] == "executing":
                    raise AssertionError(
                        f"项目阻塞：{state['blocked_reason']} "
                        f"{[(task['key'], task['status'], task['last_error']) for task in detail['tasks']]}"
                    )
                time.sleep(1)
            else:
                raise AssertionError(f"等待项目完成超时：{detail['project']}")

            call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
            detail = call("GET", f"/api/projects/{project_id}")
            assert detail["project"]["stage"] == "completed", detail["project"]
            assert all(
                task["stage"] == "completed" for task in detail["tasks"]
            ), detail["tasks"]
            assert detail["project"]["report"], "缺少项目报告"

            branch = detail["project"]["branch"]
            content = git(["show", f"{branch}:hello.txt"], repo)
            assert content.strip() == "hello domo", repr(content)
            status = git(["status", "--porcelain"], repo)
            assert not status.strip(), status

            print("真实 Agent 自迭代测试通过")
            print(f"  交付分支: {branch}")
            print(f"  作业数量: {len(detail['jobs'])}")


if __name__ == "__main__":
    main()
