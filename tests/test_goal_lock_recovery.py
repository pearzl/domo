#!/usr/bin/env python3
"""direct 协议失败保持执行阶段和目标锁，重规划不能暗改目标。"""

from harness import Server, ensure_server_built, init_repo, temp_root, wait_for


def main():
    with temp_root("domo-goal-lock-recovery-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        def prepare(server):
            for agent in server.call("GET", "/api/agents"):
                server.call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            server.call(
                "POST",
                "/api/agents",
                {
                    "name": "Goal Lock Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )

        def run_case(root, mode):
            root.mkdir()
            env = {
                "DOMO_MOCK_DELAY_MS": "60",
                "DOMO_MOCK_DIRECT_FAIL_GOAL": "[direct-fail]",
            }
            with Server(root, env=env) as server:
                prepare(server)
                goal = "[direct] [direct-fail] 已锁定目标"
                project = server.call(
                    "POST",
                    "/api/projects",
                    {"name": f"GoalLock-{mode}", "repo_path": str(repo), "base_branch": "main"},
                )["project"]
                pid = project["id"]
                server.call(
                    "POST",
                    f"/api/projects/{pid}/analysis/messages",
                    {"message": goal, "confirm_mode": "auto"},
                )

                def wait(predicate, label):
                    return wait_for(
                        lambda: (
                            lambda detail: detail if predicate(detail) else None
                        )(server.call("GET", f"/api/projects/{pid}")),
                        label,
                    )

                detail = wait(
                    lambda item: item["project"]["stage"] == "executing"
                    and item["project"]["status"] == "blocked"
                    and item["project"]["confirmed_at"] is not None
                    and any(job["role"] == "direct" for job in item["jobs"]),
                    "direct 协议失败保持执行阶段",
                )
                assert detail["project"]["goal"] == goal, detail["project"]
                server.env.pop("DOMO_MOCK_DIRECT_FAIL_GOAL")
                server.env["DOMO_MOCK_ANALYSIS_GOAL_OVERRIDE"] = "普通分析绕过目标"
                server.restart()
                server.call("POST", f"/api/projects/{pid}/replan", {"reason": "验证目标锁"})
                server.call("PATCH", f"/api/projects/{pid}/analysis/policy", {"mode": mode})
                detail = wait(
                    lambda item: item.get("proposal") is None
                    and any(event["kind"] == "plan_invalid" for event in item["events"]),
                    "目标锁定使重分析草稿无效",
                )
                assert detail["project"]["goal"] == goal, detail["project"]
                detail = server.call(
                    "POST",
                    f"/api/projects/{pid}/goal",
                    {"goal": "用户主动修改的新目标"},
                )
                assert detail["project"]["goal"] == "用户主动修改的新目标"
                assert detail["project"]["replan_active"] == 1

        def run_inflight(root, force_plan):
            root.mkdir()
            env = {
                "DOMO_MOCK_DELAY_MS": "60",
                "DOMO_MOCK_DIRECT_FAIL_GOAL": "[direct-fail]",
            }
            with Server(root, env=env) as server:
                prepare(server)
                goal = "[direct] [direct-fail] 普通分析原始目标"
                project = server.call(
                    "POST",
                    "/api/projects",
                    {"name": "OrdinaryAnalysisGoal", "repo_path": str(repo), "base_branch": "main"},
                )["project"]
                pid = project["id"]
                server.call(
                    "POST",
                    f"/api/projects/{pid}/analysis/messages",
                    {"message": goal, "confirm_mode": "auto"},
                )
                wait_for(
                    lambda: (
                        lambda detail: detail
                        if detail["project"]["stage"] == "executing"
                        and detail["project"]["status"] == "blocked"
                        and detail["project"]["confirmed_at"] is not None
                        and any(job["role"] == "direct" for job in detail["jobs"])
                        else None
                    )(server.call("GET", f"/api/projects/{pid}")),
                    "direct 失败形成目标锁",
                )
                server.env.pop("DOMO_MOCK_DIRECT_FAIL_GOAL")
                server.env["DOMO_MOCK_DELAY_MS"] = "1200"
                if force_plan:
                    server.env["DOMO_MOCK_ANALYSIS_FORCE_PLAN"] = "1"
                server.restart()
                server.call("POST", f"/api/projects/{pid}/replan", {"reason": "验证在途分析"})
                server.call("PATCH", f"/api/projects/{pid}/analysis/policy", {"mode": "manual"})
                wait_for(
                    lambda: (
                        lambda detail: detail
                        if any(
                            job["role"] == "analysis" and job["status"] == "running"
                            for job in detail["jobs"]
                        )
                        else None
                    )(server.call("GET", f"/api/projects/{pid}")),
                    "普通分析在途",
                )
                new_goal = "普通分析在途期间提交的新目标"
                server.call("POST", f"/api/projects/{pid}/goal", {"goal": new_goal})
                detail = wait_for(
                    lambda: (
                        lambda detail: detail
                        if detail.get("proposal") is not None
                        and detail["proposal"]["goal"] == new_goal
                        and not any(
                            job["role"] == "analysis" and job["status"] == "running"
                            for job in detail["jobs"]
                        )
                        else None
                    )(server.call("GET", f"/api/projects/{pid}")),
                    "旧普通分析失效后的新目标草稿",
                    120,
                )
                assert detail["project"]["goal"] == new_goal, detail["project"]
                assert detail["proposal"]["kind"] == "plan"
                assert any(
                    job["role"] == "analysis" and job["status"] == "canceled"
                    for job in detail["jobs"]
                ), detail["jobs"]

        run_case(temp / "auto", "auto")
        run_case(temp / "manual", "manual")
        run_inflight(temp / "inflight-plan", True)
        run_inflight(temp / "inflight-direct", False)
        print("direct 失败、普通分析代际与主动改目标测试通过")


if __name__ == "__main__":
    main()
