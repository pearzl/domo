#!/usr/bin/env python3
"""重规划等待旧作业静默、Git 交接与派生任务链测试。"""

from harness import Server, ensure_server_built, git, init_repo, run, temp_root, wait_for


def main():
    with temp_root("domo-safe-replan-handoff-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()
        env = {
            "DOMO_MOCK_DELAY_MS": "450",
            "DOMO_MOCK_DERIVE_ONCE": "1",
            "DOMO_MOCK_DERIVE_GOAL": "[derive-parent]",
            "DOMO_MOCK_DERIVE_KEY": "T1",
        }
        with Server(temp, env=env) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Replan Handoff Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )

            def wait_project(project_id, predicate, label, timeout=120):
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
                return project["id"]

            def running_role(project_id, role):
                return wait_project(
                    project_id,
                    lambda detail: any(
                        job["role"] == role and job["status"] == "running"
                        for job in detail["jobs"]
                    ),
                    f"{role} 在途",
                )

            def request_replan(project_id, reason):
                return call(
                    "POST",
                    f"/api/projects/{project_id}/replan",
                    {"reason": reason},
                )

            def wait_applied(project_id, label="重规划自动应用"):
                return wait_project(
                    project_id,
                    lambda detail: detail["project"]["stage"] == "executing"
                    and any(
                        event["kind"] == "replan_applied"
                        for event in detail["events"]
                    )
                    and not any(
                        job["role"] == "analysis" and job["status"] == "running"
                        for job in detail["jobs"]
                    ),
                    label,
                )

            # verifier 的 Git 合并和作业结算先于自动应用，保留范围与交付分支一致。
            verifier_id = new_project("VerifierHandoff", "[single] 旧验收合并交接")
            running_role(verifier_id, "verifier")
            before = call("GET", f"/api/projects/{verifier_id}")
            analysis_before = len(
                [job for job in before["jobs"] if job["role"] == "analysis"]
            )
            request_replan(verifier_id, "旧 verifier 完成后重新规划")
            detail = wait_applied(verifier_id, "verifier 交接后自动应用")
            verifier = next(
                job for job in detail["jobs"] if job["role"] == "verifier"
            )
            assert verifier["status"] == "succeeded", verifier
            analysis_jobs = [job for job in detail["jobs"] if job["role"] == "analysis"]
            assert len(analysis_jobs) == analysis_before + 1, analysis_jobs
            old = next(task for task in detail["tasks"] if task["key"] == "T1")
            assert old["stage"] == "completed" and old["status"] != "canceled", old
            branch = detail["project"]["branch"]
            assert "T1" in git(["show", f"{branch}:domo_mock/T1.txt"], repo)
            assert any(
                event["kind"] == "plan_superseded" for event in detail["events"]
            )

            # 任一未完成父任务会使整个派生链退出，已完成子任务不会伪称已交付。
            derive_id = new_project(
                "DerivedFamily", "[derive-parent] 派生父子链整体交接"
            )
            detail = wait_project(
                derive_id,
                lambda item: any(
                    task["parent_id"] and task["stage"] == "completed"
                    for task in item["tasks"]
                ),
                "派生子任务完成",
            )
            call("POST", f"/api/projects/{derive_id}/pause", {})
            detail = wait_project(
                derive_id,
                lambda item: item["project"]["status"] == "paused",
                "派生链暂停",
            )
            parent = next(task for task in detail["tasks"] if task["key"] == "T1")
            children = [
                task for task in detail["tasks"] if task["parent_id"] == parent["id"]
            ]
            assert any(child["stage"] == "completed" for child in children), children
            request_replan(derive_id, "未完成父任务需要重规划")
            call("POST", f"/api/projects/{derive_id}/resume", {})
            detail = wait_applied(derive_id, "派生链重规划自动应用")
            family = [
                task
                for task in detail["tasks"]
                if task["id"] == parent["id"] or task["parent_id"] == parent["id"]
            ]
            assert all(task["status"] == "canceled" if task["stage"] != "completed" else task["status"] is None for task in family), family
            analysis_job = sorted(
                (job for job in detail["jobs"] if job["role"] == "analysis"),
                key=lambda job: job["created_at"],
            )[-1]
            prompt = call("GET", f"/api/jobs/{analysis_job['id']}")["prompt"]
            retained_prompt = prompt.split("完整保留的交付任务链", 1)[1].split(
                "不保留任务", 1
            )[0]
            assert all(child["key"] in retained_prompt for child in children if child["stage"] == "completed"), prompt
            canceled_prompt = prompt.split("不保留任务", 1)[1]
            assert any(child["key"] in canceled_prompt for child in children), prompt
            for child in children:
                if child["stage"] == "completed":
                    result = run(
                        ["git", "show", f"{detail['project']['branch']}:domo_mock/{child['key']}.txt"],
                        cwd=repo,
                        check=False,
                    )
                    assert result.returncode == 0, child

            print("旧作业静默、Git 交接与派生任务链测试通过")


if __name__ == "__main__":
    main()
