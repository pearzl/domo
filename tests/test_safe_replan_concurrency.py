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

            # 分析期间不能绕过影响预览添加任务。
            pid = new_project("DoubleConfirm")
            request_replan(pid, "验证原子双确认")
            detail = draft(pid)
            before = len(detail["tasks"])
            status, _ = raw_call(
                server.base,
                "POST",
                f"/api/projects/{pid}/tasks",
                {
                    "key": "T9",
                    "title": "绕过预览的任务",
                    "description": "不应创建",
                    "acceptance": [],
                    "deps": [],
                },
            )
            assert status == 409, status
            assert len(call("GET", f"/api/projects/{pid}")["tasks"]) == before

            # 两个请求读取同一草稿时，只有一个能提交代际切换。
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                results = list(
                    pool.map(
                        lambda _: raw_call(
                            server.base,
                            "POST",
                            f"/api/projects/{pid}/analysis/confirm",
                            {},
                        ),
                        range(2),
                    )
                )
            statuses = sorted(status for status, _ in results)
            assert statuses[0] == 200 and statuses[1] in (400, 409), results
            detail = call("GET", f"/api/projects/{pid}")
            assert len(detail["tasks"]) == 4, detail["tasks"]
            old = [task for task in detail["tasks"] if task["seq"] <= 2]
            new = [task for task in detail["tasks"] if task["seq"] > 2]
            assert all(task["status"] == "canceled" for task in old), old
            assert all(task["status"] != "canceled" for task in new), new
            assert sum(
                event["kind"] == "plan_superseded" for event in detail["events"]
            ) == 1

            # direct 草稿使用同一代际校验，不能重复取消新计划。
            direct_id = new_project("DirectDoubleConfirm", "[replan-direct] direct 双确认")
            request_replan(direct_id, "验证 direct 原子双确认")
            detail = draft(direct_id)
            assert detail["proposal"]["kind"] == "direct", detail["proposal"]
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                results = list(
                    pool.map(
                        lambda _: raw_call(
                            server.base,
                            "POST",
                            f"/api/projects/{direct_id}/analysis/confirm",
                            {},
                        ),
                        range(2),
                    )
                )
            statuses = sorted(status for status, _ in results)
            assert statuses[0] == 200 and statuses[1] in (400, 409), results
            detail = call("GET", f"/api/projects/{direct_id}")
            assert detail["project"]["completion_mode"] == "direct"
            assert len(detail["tasks"]) == 2, detail["tasks"]
            assert all(task["status"] == "canceled" for task in detail["tasks"])

            # 修改意见与确认竞态时同样只能有一个代际所有者。
            race_id = new_project("ConfirmMessageRace")
            request_replan(race_id, "验证确认与修改意见竞态")
            draft(race_id)

            def confirm(_):
                return raw_call(
                    server.base,
                    "POST",
                    f"/api/projects/{race_id}/analysis/confirm",
                    {},
                )

            def modify(_):
                return raw_call(
                    server.base,
                    "POST",
                    f"/api/projects/{race_id}/analysis/messages",
                    {"message": "与确认同时提交的修改意见"},
                )

            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                outcomes = list(pool.map(lambda fn: fn(None), (confirm, modify)))
            statuses = sorted(status for status, _ in outcomes)
            assert statuses[0] == 200 and statuses[1] in (400, 409), outcomes
            detail = call("GET", f"/api/projects/{race_id}")
            if outcomes[0][0] == 200:
                assert detail["project"]["stage"] == "executing", detail["project"]
            else:
                assert outcomes[1][0] == 200
                assert detail["project"]["stage"] == "analysis", detail["project"]
                assert detail.get("proposal") is None
                assert all(task["status"] != "canceled" for task in detail["tasks"])

            # 草稿就绪时可放弃并恢复旧计划，暂停项目保持暂停状态。
            abandon_id = new_project("AbandonDraft")
            request_replan(abandon_id, "验证放弃草稿")
            before = draft(abandon_id)
            old_ids = {task["id"] for task in before["tasks"]}
            detail = call("POST", f"/api/projects/{abandon_id}/replan/abandon")
            assert detail["project"]["stage"] == "executing", detail["project"]
            assert detail["project"]["replan_active"] == 0
            assert {task["id"] for task in detail["tasks"]} == old_ids
            assert all(task["status"] != "canceled" for task in detail["tasks"])
            assert any(event["kind"] == "replan_aborted" for event in detail["events"])

            paused_id = new_project("AbandonPaused")
            call("POST", f"/api/projects/{paused_id}/pause", {"force": True})
            request_replan(paused_id, "暂停时放弃重规划")
            detail = call("POST", f"/api/projects/{paused_id}/replan/abandon")
            assert detail["project"]["stage"] == "executing", detail["project"]
            assert detail["project"]["status"] == "paused", detail["project"]
            assert all(task["status"] != "canceled" for task in detail["tasks"])

            print("重规划并发、加任务约束与放弃恢复测试通过")


if __name__ == "__main__":
    main()
