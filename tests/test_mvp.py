#!/usr/bin/env python3
"""DOMO MVP smoke test.

Covers:
  1. the planner creates a DAG with dependencies after the goal is confirmed;
  2. tasks run in isolated worktrees and merge in dependency order;
  3. a verifier rejection returns the task to the executor with the reason;
  4. pause -> paused -> resume -> summary -> manual report acceptance;
  5. archiving stops the project immediately;
  6. job inputs, outputs, and events remain queryable.

Uses the built-in mock agent and never calls a real model. All runtime data is
kept under the system temp directory. Exit code 0 means every check passed.
"""

import time

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
    with temp_root("domo-mvp-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "120",
                "DOMO_MOCK_REJECT_ONCE": "1",
                "DOMO_MOCK_FAIL_ONCE": "1",
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

            def wait_project(project_id, predicate, label, timeout=90):
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
                    "name": "Smoke Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )

            project = call(
                "POST",
                "/api/projects",
                {
                    "name": "Smoke",
                    "goal": "生成两个有依赖的 mock 文件",
                    "repo_path": str(repo),
                    "base_branch": "main",
                },
            )["project"]
            project_id = project["id"]

            call("POST", f"/api/projects/{project_id}/confirm", {})
            call("POST", f"/api/projects/{project_id}/pause", {})

            paused = wait_project(
                project_id,
                lambda detail: detail["project"]["status"] == "paused",
                "项目暂停",
                30,
            )
            assert paused["project"]["stage"] in ("analysis", "executing"), paused["project"]

            call("POST", f"/api/projects/{project_id}/resume", {})
            detail = wait_project(
                project_id,
                lambda detail: detail["project"]["stage"] == "summarizing"
                and detail["project"]["status"] == "blocked",
                "项目报告",
            )

            tasks = {task["key"]: task for task in detail["tasks"]}
            assert set(tasks) == {"T1", "T2"}, tasks
            assert all(task["stage"] == "completed" for task in detail["tasks"]), detail["tasks"]
            assert detail["project"]["report"], detail["project"]

            events = detail["events"]
            assert any(event["kind"] == "task_rejected" for event in events), "未覆盖验收拒绝路径"
            assert any(event["kind"] == "task_retry" for event in events), "未覆盖执行失败重试路径"
            assert any(event["kind"] == "plan_created" for event in events), "缺少规划事件"
            assert any(event["kind"] == "task_completed" for event in events), "缺少完成事件"

            jobs = detail["jobs"]
            roles = {job["role"] for job in jobs}
            assert {"analysis", "executor", "verifier", "summary"} <= roles, roles
            assert any(job["status"] == "failed" for job in jobs), "未覆盖失败作业审计"
            assert any(
                job["role"] == "verifier" and job["attempt"] > 1 for job in jobs
            ), "未覆盖验收拒绝后的再次验收"
            assert tasks["T1"]["attempts"] >= 1, tasks["T1"]

            completed_job = next((job for job in jobs if job["status"] == "succeeded"), None)
            assert completed_job is not None, jobs
            activity = call("GET", f"/api/jobs/{completed_job['id']}/activity")
            assert activity["format"], activity
            assert activity["bytes"] > 0, activity
            assert activity["items"], activity

            branch = detail["project"]["branch"]
            t1 = git(["show", f"{branch}:domo_mock/T1.txt"], repo)
            t2 = git(["show", f"{branch}:domo_mock/T2.txt"], repo)
            assert "T1" in t1
            assert "dep T1 was merged" in t2

            branch_log = git(["log", "--format=%s", branch], repo).splitlines()
            assert not any("Merge" in line for line in branch_log), branch_log
            assert branch_log[0].startswith("domo(task-T2)"), branch_log
            assert branch_log[1].startswith("domo(task-T1)"), branch_log

            call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
            completed = call("GET", f"/api/projects/{project_id}")
            assert completed["project"]["stage"] == "completed", completed["project"]
            assert completed["project"]["status"] is None

            qa = call(
                "POST",
                "/api/projects",
                {
                    "name": "Clarify",
                    "goal": "[ask] 先澄清再执行",
                    "repo_path": str(repo),
                    "base_branch": "main",
                },
            )["project"]
            call("POST", f"/api/projects/{qa['id']}/confirm", {})
            qa_detail = wait_project(
                qa["id"],
                lambda detail: detail["project"]["stage"] == "analysis"
                and detail["project"]["status"] == "blocked"
                and any(message["role"] == "planner" for message in detail["messages"]),
                "规划者提问",
                30,
            )
            assert any("哪个使用场景" in message["content"] for message in qa_detail["messages"])
            call(
                "POST",
                f"/api/projects/{qa['id']}/confirm",
                {"message": "没有偏好，请直接规划。"},
            )
            wait_project(
                qa["id"],
                lambda detail: len(detail["tasks"]) > 0
                or detail["project"]["stage"] in ("summarizing", "completed"),
                "澄清后生成任务",
                30,
            )
            call("POST", f"/api/projects/{qa['id']}/archive", {})
            archived_detail = call("GET", f"/api/projects/{qa['id']}")
            assert archived_detail["project"]["status"] == "archived", archived_detail["project"]

            print("冒烟测试通过")
            print(f"  项目分支: {branch}")
            print(f"  作业数量: {len(jobs)}")
            print(f"  事件数量: {len(events)}")


if __name__ == "__main__":
    main()
