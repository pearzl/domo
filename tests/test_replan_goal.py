#!/usr/bin/env python3
"""已确认目标不可由重规划修改，用户主动改目标启动新代际。"""

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
    with temp_root("domo-replan-goal-") as temp:
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
                    "name": "Replan Goal Mock",
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

            def new_project(name, goal):
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
                detail = wait_project(
                    project["id"],
                    lambda item: item["project"]["stage"] == "executing"
                    and len(item["tasks"]) == 2,
                    f"{name} 首次确认",
                )
                assert detail["project"]["goal"] == goal, detail["project"]
                return project["id"]

            def request_replan(project_id):
                return call(
                    "POST",
                    f"/api/projects/{project_id}/replan",
                    {"reason": "验证目标约束"},
                )

            def wait_draft(project_id):
                return wait_project(
                    project_id,
                    lambda item: item.get("proposal") is not None
                    and not any(
                        job["role"] == "analysis" and job["status"] == "running"
                        for job in item["jobs"]
                    ),
                    "目标一致的草稿",
                )

            def assert_rejected(project_id, confirmed_goal):
                detail = wait_project(
                    project_id,
                    lambda item: "重规划不能修改已确认目标"
                    in (item["project"]["blocked_reason"] or ""),
                    "偏离目标的草稿被拒绝",
                )
                assert detail["project"]["goal"] == confirmed_goal, detail["project"]
                assert detail.get("proposal") is None
                assert all(task["status"] != "canceled" for task in detail["tasks"])
                status, _ = raw_call(
                    server.base,
                    "POST",
                    f"/api/projects/{project_id}/analysis/confirm",
                    {},
                )
                assert status == 400, status
                assert any(event["kind"] == "plan_invalid" for event in detail["events"])

            # 自动确认开启且 mock 请求跳过时，plan 偏离目标仍无效。
            plan_goal = "[replan-goal-mismatch] 已确认目标"
            plan_id = new_project("PlanGoalMismatch", plan_goal)
            request_replan(plan_id)
            assert_rejected(plan_id, plan_goal)

            # direct 也经过同一硬约束，旧计划不因草稿而受损。
            direct_goal = "[replan-direct-mismatch] direct 已确认目标"
            direct_id = new_project("DirectGoalMismatch", direct_goal)
            request_replan(direct_id)
            assert_rejected(direct_id, direct_goal)

            # 用户主动提交目标会提升代际；并发旧确认不能覆盖新目标。
            original_goal = "用户首次确认的原始目标"
            edit_id = new_project("ExplicitGoalEdit", original_goal)
            request_replan(edit_id)
            old_draft = wait_draft(edit_id)
            assert old_draft["proposal"]["goal"] == original_goal
            new_goal = "用户主动提交的新目标"
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                outcomes = list(
                    pool.map(
                        lambda fn: fn(None),
                        (
                            lambda _: raw_call(
                                server.base,
                                "POST",
                                f"/api/projects/{edit_id}/goal",
                                {"goal": new_goal},
                            ),
                            lambda _: raw_call(
                                server.base,
                                "POST",
                                f"/api/projects/{edit_id}/analysis/confirm",
                                {},
                            ),
                        ),
                    )
                )
            assert outcomes[0][0] == 200, outcomes
            assert outcomes[1][0] in (200, 400, 409), outcomes
            detail = wait_project(
                edit_id,
                lambda item: item.get("proposal") is not None
                and item["proposal"]["goal"] == new_goal
                and not any(
                    job["role"] == "analysis" and job["status"] == "running"
                    for job in item["jobs"]
                ),
                "围绕用户新目标生成的草稿",
            )
            assert detail["project"]["goal"] == new_goal, detail["project"]
            assert any(event["kind"] == "goal_updated" for event in detail["events"])
            status, _ = raw_call(
                server.base,
                "POST",
                f"/api/projects/{edit_id}/analysis/confirm",
                {},
            )
            assert status == 200, status
            detail = call("GET", f"/api/projects/{edit_id}")
            assert detail["project"]["goal"] == new_goal, detail["project"]

            print("重规划目标不变量与主动修改目标测试通过")


if __name__ == "__main__":
    main()
