#!/usr/bin/env python3
"""DOMO 执行者任务派生测试。

覆盖范围：
  1. 首个执行作业返回 derive 后，子任务写入同一项目 DAG；
  2. 子任务自动成为父任务前置，兄弟任务 deps 为空，父任务保持等待且没有失败；
  3. 子任务跳过验收，执行成功后直接合入父任务分支；
  4. 全部子任务完成后父任务再次执行并通过验收，最终合入项目交付分支；
  5. 父任务 prompt 包含派生子任务及其结果，子任务 prompt 标记派生并禁止再次派生；
  6. 非法派生使作业失败并进入既有重试流程，不写入子任务；
  7. 项目报告可生成并完成人工验收。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
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
    with temp_root("domo-derive-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "60",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "350",
                "DOMO_MOCK_DERIVE_ONCE": "1",
                "DOMO_MOCK_DERIVE_GOAL": "[derive]",
                "DOMO_MOCK_DERIVE_KEY": "T1",
                "DOMO_MOCK_INVALID_DERIVE_ONCE": "1",
                "DOMO_MOCK_INVALID_DERIVE_GOAL": "[invalid-derive]",
                "DOMO_MOCK_INVALID_DERIVE_KEY": "T1",
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

            def wait_project(project_id, predicate, label, timeout=120):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            def new_project(name, goal):
                project = call(
                    "POST",
                    "/api/projects",
                    {
                        "name": name,
                        "goal": goal,
                        "repo_path": str(repo),
                        "base_branch": "main",
                    },
                )["project"]
                call("POST", f"/api/projects/{project['id']}/confirm", {})
                return project["id"]

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Derive Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )
            # 新建项目的项目池继承默认团队。

            # 1. 合法派生：子任务进入 DAG，父任务等待。
            project_id = new_project("Derive", "[derive] 将复杂任务派生为两个并行子任务")
            detail = wait_project(
                project_id,
                lambda item: any(
                    event["kind"] == "task_derived" for event in item["events"]
                ),
                "派生事件",
                90,
            )
            tasks = {task["key"]: task for task in detail["tasks"]}
            assert {"T1", "T2", "T1.1", "T1.2"} <= set(tasks), tasks
            parent = tasks["T1"]
            children = [task for task in detail["tasks"] if task["parent_id"] == parent["id"]]
            assert {child["key"] for child in children} == {"T1.1", "T1.2"}, children
            assert all(child["deps"] == [] for child in children), children
            assert all(child["status"] in ("queued", "running") for child in children), children
            assert set(parent["deps"]) == {"T1.1", "T1.2"}, parent
            assert parent["stage"] == "pending", parent
            assert parent["status"] == "queued", parent
            assert parent["status"] != "blocked", parent
            derive_event = next(
                event for event in detail["events"] if event["kind"] == "task_derived"
            )
            assert (
                "T1.1" in derive_event["message"] and "T1.2" in derive_event["message"]
            ), derive_event

            # 2. 全部子任务完成后父任务再次执行并完成，项目产出报告。
            detail = wait_project(
                project_id,
                lambda item: item["project"]["stage"] == "summarizing"
                and item["project"]["report"]
                and all(
                    task["stage"] == "completed"
                    for task in item["tasks"]
                    if task["status"] != "canceled"
                ),
                "父任务完成并生成报告",
            )
            tasks = {task["key"]: task for task in detail["tasks"]}
            parent = tasks["T1"]
            children = [task for task in detail["tasks"] if task["parent_id"] == parent["id"]]
            assert parent["stage"] == "completed", parent
            assert parent["status"] is None, parent
            assert all(child["stage"] == "completed" for child in children), children
            assert all(child["result_summary"] for child in children), children
            assert detail["project"]["report"], detail["project"]

            jobs = detail["jobs"]
            parent_execs = sorted(
                (
                    job
                    for job in jobs
                    if job["task_id"] == parent["id"] and job["role"] == "executor"
                ),
                key=lambda job: job["created_at"],
            )
            assert len(parent_execs) >= 2, parent_execs
            for child in children:
                child_jobs = [
                    job
                    for job in jobs
                    if job["task_id"] == child["id"] and job["role"] == "executor"
                ]
                assert child_jobs, child
                assert all(job["status"] == "succeeded" for job in child_jobs), child_jobs
                verifier_jobs = [
                    job
                    for job in jobs
                    if job["task_id"] == child["id"] and job["role"] == "verifier"
                ]
                assert not verifier_jobs, verifier_jobs

            parent_prompt = call("GET", f"/api/jobs/{parent_execs[-1]['id']}")["prompt"]
            assert "## 派生子任务" in parent_prompt, parent_prompt
            assert "T1.1" in parent_prompt and "T1.2" in parent_prompt, parent_prompt
            assert "已写入 domo_mock/T1.1.txt" in parent_prompt, parent_prompt
            child_prompt = call(
                "GET",
                f"/api/jobs/{next(job for job in jobs if job['task_id'] == children[0]['id'] and job['role'] == 'executor')['id']}",
            )["prompt"]
            assert "## 派生任务约束" in child_prompt, child_prompt
            assert "跳过验收阶段" in child_prompt, child_prompt
            assert "禁止再次派生新的子任务" in child_prompt, child_prompt
            assert children[0]["key"] in child_prompt, child_prompt

            # 3. 项目交付分支同时包含父任务和子任务产物。
            branch = detail["project"]["branch"]
            for name in ("T1", "T1.1", "T1.2", "T2"):
                content = git(["show", f"{branch}:domo_mock/{name}.txt"], repo)
                assert name in content, (name, content)
            parent_file = git(["show", f"{branch}:domo_mock/T1.txt"], repo)
            assert "dep T1.1 was merged" in parent_file, parent_file
            assert "dep T1.2 was merged" in parent_file, parent_file

            call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
            completed = call("GET", f"/api/projects/{project_id}")
            assert completed["project"]["stage"] == "completed", completed["project"]
            assert completed["project"]["status"] is None, completed["project"]

            # 4. 非法派生：作业失败并进入既有重试流程，不写入子任务。
            invalid_id = new_project("InvalidDerive", "[invalid-derive] 非法派生应进入重试")
            detail = wait_project(
                invalid_id,
                lambda item: any(
                    job["role"] == "executor"
                    and job["status"] == "failed"
                    and "派生" in (job["error"] or "")
                    for job in item["jobs"]
                ),
                "非法派生作业失败",
                90,
            )
            failed_job = next(
                job
                for job in detail["jobs"]
                if job["role"] == "executor"
                and job["status"] == "failed"
                and "派生" in (job["error"] or "")
            )
            assert "deps" in failed_job["error"], failed_job
            invalid_t1 = next(task for task in detail["tasks"] if task["key"] == "T1")
            assert invalid_t1["attempts"] >= 1, invalid_t1
            assert invalid_t1["status"] != "blocked", invalid_t1
            assert not any(task["parent_id"] for task in detail["tasks"]), detail["tasks"]
            assert any(event["kind"] == "task_retry" for event in detail["events"]), detail[
                "events"
            ]

            detail = wait_project(
                invalid_id,
                lambda item: item["project"]["stage"] == "summarizing"
                and item["project"]["report"]
                and all(
                    task["stage"] == "completed"
                    for task in item["tasks"]
                    if task["status"] != "canceled"
                ),
                "非法派生重试后完成",
            )
            assert not any(task["parent_id"] for task in detail["tasks"]), detail["tasks"]
            assert {task["key"] for task in detail["tasks"]} == {"T1", "T2"}, detail["tasks"]

            print("派生流程测试通过")
            print(f"  项目分支: {branch}")
            print(f"  父任务执行作业: {len(parent_execs)}")
            print(f"  子任务: {', '.join(child['key'] for child in children)}")


if __name__ == "__main__":
    main()
