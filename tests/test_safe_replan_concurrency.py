#!/usr/bin/env python3
"""重规划草稿代际、并发确认与放弃恢复测试。"""

import concurrent.futures
import json
import urllib.error
import urllib.request

from harness import Server, ensure_server_built, init_repo, temp_root, wait_for


def raw_call(base, method, path, data=None):
    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(
        base + path,
        data=body,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raw = error.read()
        return error.code, json.loads(raw) if raw else None


def main():
    with temp_root("domo-safe-replan-concurrency-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()
        with Server(temp, env={"DOMO_MOCK_DELAY_MS": "60"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Safe Replan Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )

            def wait_project(project_id, predicate, label, timeout=90):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            def new_project(name, goal="两个任务的安全重规划测试"):
                project = call(
                    "POST",
                    "/api/projects",
                    {"name": name, "repo_path": str(repo), "base_branch": "main"},
                )["project"]
                call(
                    "POST",
                    f"/api/projects/{project['id']}/analysis/messages",
                    {"message": goal, "confirm_mode": "auto"},
                )
                wait_project(
                    project["id"],
                    lambda detail: detail["project"]["stage"] == "executing"
                    and len(detail["tasks"]) == 2,
                    f"{name} 初始计划",
                )
                return project["id"]

            def draft(project_id):
                return wait_project(
                    project_id,
                    lambda detail: detail["project"]["stage"] == "analysis"
                    and detail.get("proposal") is not None
                    and not any(
                        job["role"] == "analysis" and job["status"] == "running"
                        for job in detail["jobs"]
                    ),
                    "重规划草稿",
                )

            def request_replan(project_id, reason):
                return call(
                    "POST",
                    f"/api/projects/{project_id}/replan",
                    {"reason": reason},
                )

            # 重规划草稿由 DOMO 自动应用；并发请求不能让同一代际重复创建任务。
            pid = new_project("AutoApplyReplan")
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                results = list(
                    pool.map(
                        lambda _: raw_call(
                            server.base,
                            "POST",
                            f"/api/projects/{pid}/replan",
                            {"reason": "验证自动应用代际"},
                        ),
                        range(2),
                    )
                )
            assert any(status == 200 for status, _ in results), results
            detail = wait_project(
                pid,
                lambda item: item["project"]["stage"] == "executing"
                and len(item["tasks"]) == 4
                and any(
                    event["kind"] == "replan_applied"
                    for event in item["events"]
                ),
                "重规划自动应用",
            )
            old = [task for task in detail["tasks"] if task["seq"] <= 2]
            new = [task for task in detail["tasks"] if task["seq"] > 2]
            assert all(task["status"] == "canceled" for task in old), old
            assert all(task["status"] != "canceled" for task in new), new
            assert sum(
                event["kind"] == "plan_superseded" for event in detail["events"]
            ) == 1
            assert sum(
                event["kind"] == "replan_applied" for event in detail["events"]
            ) == 1

            # direct 重规划使用同一代际校验，不能重复取消新计划。
            direct_id = new_project(
                "DirectAutoApply", "[replan-direct] direct 自动应用"
            )
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                list(
                    pool.map(
                        lambda _: raw_call(
                            server.base,
                            "POST",
                            f"/api/projects/{direct_id}/replan",
                            {"reason": "验证 direct 自动应用"},
                        ),
                        range(2),
                    )
                )
            detail = wait_project(
                direct_id,
                lambda item: item["project"]["completion_mode"] == "direct"
                and len(item["tasks"]) == 2
                and all(
                    task["status"] == "canceled" for task in item["tasks"]
                )
                and any(
                    event["kind"] == "replan_applied"
                    for event in item["events"]
                ),
                "direct 重规划自动应用",
            )
            assert detail["project"]["stage"] == "executing", detail["project"]

            # 分析尚未应用时仍可放弃并恢复旧计划，暂停项目保持暂停状态。
            paused_id = new_project("AbandonPaused")
            call("POST", f"/api/projects/{paused_id}/pause", {"force": True})
            wait_project(
                paused_id,
                lambda item: item["project"]["status"] == "paused",
                "暂停项目",
            )
            request_replan(paused_id, "暂停时放弃重规划")
            before = call("GET", f"/api/projects/{paused_id}")
            assert before["project"]["stage"] == "analysis", before["project"]
            assert before["project"]["status"] == "paused", before["project"]
            old_ids = {task["id"] for task in before["tasks"]}
            detail = call("POST", f"/api/projects/{paused_id}/replan/abandon")
            assert detail["project"]["stage"] == "executing", detail["project"]
            assert detail["project"]["status"] == "paused", detail["project"]
            assert {task["id"] for task in detail["tasks"]} == old_ids
            assert all(task["status"] != "canceled" for task in detail["tasks"])
            assert any(event["kind"] == "replan_aborted" for event in detail["events"])

            print("重规划自动应用、并发代际与放弃恢复测试通过")


if __name__ == "__main__":
    main()
