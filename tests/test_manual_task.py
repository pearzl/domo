#!/usr/bin/env python3
"""DOMO 人工添加任务冒烟测试。

覆盖范围：
  1. 对总结中（报告已生成）的项目人工添加任务：项目回到执行阶段，原报告作废；
  2. 添加的任务按 DAG 调度执行，全部任务完成后报告重新生成并可验收；
  3. 对规划中（规划 Agent 不可用而等待）的项目添加任务：项目进入执行阶段并完成任务；
  4. 对已完成（未归档）的项目添加任务：项目重新回到执行阶段；
  5. 非法请求被明确拒绝（400 且附原因）：重复 key、依赖不存在的任务；
  6. 审计事件记录任务创建（task_created）与项目阶段回退（project_reopened）。

约定：使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
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
    with temp_root("domo-manual-task-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(temp, env={"DOMO_MOCK_DELAY_MS": "120"}) as server:
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

            def call_raw(method, path, data=None):
                body = json.dumps(data).encode() if data is not None else None
                request = urllib.request.Request(
                    server.base + path,
                    data=body,
                    method=method,
                    headers={"Content-Type": "application/json"},
                )
                try:
                    with urllib.request.urlopen(request, timeout=60) as response:
                        raw = response.read()
                        return response.status, json.loads(raw) if raw else None
                except urllib.error.HTTPError as error:
                    raw = error.read().decode("utf-8", errors="replace")
                    try:
                        return error.code, json.loads(raw)
                    except Exception:
                        return error.code, {"error": raw}

            def expect_reject(method, path, data, needle):
                code, body = call_raw(method, path, data)
                assert code == 400, (
                    f"{method} {path} 期望 400，实际 {code}: {body}"
                )
                message = (body or {}).get("error", "")
                assert needle in message, f"期望原因包含「{needle}」，实际：{message}"

            def wait_project(project_id, predicate, label, timeout=90):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            def add_task(project_id, key, deps=None, milestone=False):
                return call(
                    "POST",
                    f"/api/projects/{project_id}/tasks",
                    {
                        "key": key,
                        "title": f"人工任务 {key}",
                        "description": f"在 domo_mock/{key}.txt 写入人工补充的内容。",
                        "acceptance": [f"domo_mock/{key}.txt 存在且包含 {key}"],
                        "deps": deps or [],
                        "milestone": milestone,
                    },
                )

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Manual Task Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "priority": 0,
                    "max_concurrency": 2,
                },
            )

            # ---- 项目 A：总结中添加任务 -> 回到执行 -> 报告作废 -> 重写 ----
            project = call(
                "POST",
                "/api/projects",
                {
                    "name": "Manual-Summarizing",
                    "goal": "生成两个有依赖的 mock 文件",
                    "repo_path": str(repo),
                    "base_branch": "main",
                },
            )["project"]
            project_id = project["id"]

            call("POST", f"/api/projects/{project_id}/confirm", {})
            detail = wait_project(
                project_id,
                lambda d: d["project"]["stage"] == "summarizing"
                and d["project"]["report"]
                and d["project"]["status"] == "blocked",
                "第一轮报告",
            )
            assert {task["key"] for task in detail["tasks"]} == {"T1", "T2"}

            created = add_task(project_id, "T3")
            assert created["key"] == "T3"
            assert created["stage"] == "pending"
            assert created["status"] == "queued"

            detail = call("GET", f"/api/projects/{project_id}")
            assert detail["project"]["stage"] == "executing", detail["project"]
            assert detail["project"]["report"] is None, detail["project"]
            assert detail["project"]["status"] == "running", detail["project"]
            task_keys = {task["key"]: task for task in detail["tasks"]}
            # 调度器可能在添加后立即拾取 T3；创建响应中的 pending/queued
            # 已在上方断言，这里允许它已进入执行阶段。
            assert task_keys["T3"]["stage"] in ("pending", "executing"), task_keys[
                "T3"
            ]
            assert task_keys["T3"]["status"] in ("queued", "running"), task_keys[
                "T3"
            ]
            assert task_keys["T3"]["deps"] == [], task_keys["T3"]

            detail = wait_project(
                project_id,
                lambda d: d["project"]["stage"] == "summarizing"
                and d["project"]["report"]
                and all(task["stage"] == "completed" for task in d["tasks"]),
                "T3 完成后报告重写",
            )
            assert {task["key"] for task in detail["tasks"]} == {"T1", "T2", "T3"}

            kinds = [event["kind"] for event in detail["events"]]
            assert "task_created" in kinds, kinds
            assert "project_reopened" in kinds, kinds
            created_events = [
                event for event in detail["events"] if event["kind"] == "task_created"
            ]
            assert any(
                "T3" in event["message"] for event in created_events
            ), created_events
            reopened_events = [
                event
                for event in detail["events"]
                if event["kind"] == "project_reopened"
            ]
            assert any(
                "作废" in event["message"] for event in reopened_events
            ), reopened_events

            # 非法请求：重复 key、依赖不存在的任务
            expect_reject(
                "POST",
                f"/api/projects/{project_id}/tasks",
                {"key": "T3", "title": "重复", "description": "重复 key"},
                "已存在",
            )
            expect_reject(
                "POST",
                f"/api/projects/{project_id}/tasks",
                {
                    "key": "T9",
                    "title": "坏依赖",
                    "description": "依赖不存在",
                    "deps": ["T99"],
                },
                "不存在",
            )

            # 报告验收通过后项目完成；对已完成（未归档）项目添加任务会重新打开项目
            call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
            detail = wait_project(
                project_id,
                lambda d: d["project"]["stage"] == "completed",
                "项目完成",
            )
            add_task(project_id, "T4")
            detail = call("GET", f"/api/projects/{project_id}")
            assert detail["project"]["stage"] == "executing", detail["project"]
            assert detail["project"]["report"] is None, detail["project"]
            assert detail["project"]["status"] == "running", detail["project"]
            call("POST", f"/api/projects/{project_id}/archive", {})

            # ---- 项目 B：项目分析阶段添加任务，确认方案后与人工任务一起执行 ----
            planning = call(
                "POST",
                "/api/projects",
                {
                    "name": "Manual-Before-Planning",
                    "repo_path": str(repo),
                    "base_branch": "main",
                },
            )["project"]
            planning_id = planning["id"]
            server.call(
                "POST",
                f"/api/projects/{planning_id}/analysis/messages",
                {"message": "生成一个 mock 文件", "confirm_mode": "manual"},
            )
            wait_project(
                planning_id,
                lambda d: d.get("proposal")
                and d["project"]["stage"] == "analysis"
                and d["project"]["status"] == "blocked",
                "项目分析方案",
                30,
            )

            created = add_task(planning_id, "M1")
            assert created["stage"] == "pending"
            assert created["status"] == "queued"

            server.call(
                "POST", f"/api/projects/{planning_id}/analysis/confirm", {}
            )
            detail = wait_project(
                planning_id,
                lambda d: d["project"]["stage"] == "summarizing"
                and d["project"]["report"]
                and all(task["stage"] == "completed" for task in d["tasks"]),
                "项目分析确认前添加的任务完成",
            )
            assert "M1" in {task["key"] for task in detail["tasks"]}, detail["tasks"]
            kinds = [event["kind"] for event in detail["events"]]
            assert "task_created" in kinds, kinds
            call("POST", f"/api/projects/{planning_id}/archive", {})

            print("冒烟测试通过")


if __name__ == "__main__":
    main()
