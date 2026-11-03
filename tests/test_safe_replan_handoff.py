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

            def wait_draft(project_id):
                return wait_project(
                    project_id,
                    lambda detail: detail["project"]["stage"] == "analysis"
                    and detail.get("proposal") is not None
                    and not any(
                        job["role"] == "analysis" and job["status"] == "running"
                        for job in detail["jobs"]
                    ),
                    "静默后的重规划草稿",
                )

            # 旧 direct 回调只保存旧报告，不覆盖重规划阶段；放弃后报告可验收。
            direct_id = new_project("DirectHandoff", "[direct] 旧直接完成交接")
            running_role(direct_id, "direct")
            request_replan(direct_id, "旧 direct 完成后重新规划")
            detail = wait_draft(direct_id)
            assert detail["project"]["report"], detail["project"]
            assert detail["project"]["stage"] == "analysis"
            detail = call("POST", f"/api/projects/{direct_id}/replan/abandon")
            assert detail["project"]["stage"] == "summarizing", detail["project"]
            assert detail["project"]["report"], detail["project"]

            # 等待旧作业静默时也能取消请求，旧 direct 继续完成原流程。
            waiting_id = new_project("WaitingAbandon", "[direct] 等待交接时放弃")
            running_role(waiting_id, "direct")
            request_replan(waiting_id, "等待期间放弃重规划")
            detail = call("POST", f"/api/projects/{waiting_id}/replan/abandon")
            assert detail["project"]["stage"] == "executing", detail["project"]
            detail = wait_project(
                waiting_id,
                lambda item: item["project"]["stage"] == "summarizing"
                and bool(item["project"]["report"]),
                "放弃后旧 direct 报告",
            )

            # 多次主动改目标后放弃，旧 direct 报告与本轮最初目标一起恢复。
            goal_id = new_project("GoalRestoreHandoff", "[direct] 放弃时恢复最初目标")
            original_goal = call("GET", f"/api/projects/{goal_id}")["project"]["goal"]
            running_role(goal_id, "direct")
            call("POST", f"/api/projects/{goal_id}/goal", {"goal": "本轮中间目标"})
            call("POST", f"/api/projects/{goal_id}/goal", {"goal": "本轮最后目标"})
            detail = wait_draft(goal_id)
            assert detail["project"]["report"], detail["project"]
            detail = call("POST", f"/api/projects/{goal_id}/replan/abandon")
            assert detail["project"]["goal"] == original_goal, detail["project"]
            assert detail["project"]["stage"] == "summarizing", detail["project"]
            assert detail["project"]["report"], detail["project"]
            assert any(
                event["kind"] == "replan_aborted" and original_goal in event["message"]
                for event in detail["events"]
            )

            # 旧 summary 回调也不能把 stage 改回 summarizing。
            summary_id = new_project("SummaryHandoff", "[single] 旧总结交接")
            running_role(summary_id, "summary")
            request_replan(summary_id, "旧 summary 完成后重新规划")
            detail = wait_draft(summary_id)
            assert detail["project"]["report"], detail["project"]
            assert detail["project"]["stage"] == "analysis"
            detail = call("POST", f"/api/projects/{summary_id}/replan/abandon")
            assert detail["project"]["stage"] == "summarizing", detail["project"]

            # 旧项目验收在静默屏障内完成，不触发第二次重规划或阶段回写。
            acceptance_id = new_project("AcceptanceHandoff", "[single] 旧项目验收交接")
            running_role(acceptance_id, "project_acceptance")
            request_replan(acceptance_id, "旧项目验收完成后重新规划")
            detail = wait_draft(acceptance_id)
            assert detail["project"]["stage"] == "analysis"
            assert detail["project"]["replan_reason"] == "旧项目验收完成后重新规划"
            detail = call("POST", f"/api/projects/{acceptance_id}/replan/abandon")
            assert detail["project"]["stage"] == "summarizing", detail["project"]

            # verifier 的 Git 合并和作业结算先于分析草稿，保留范围与交付分支一致。
            verifier_id = new_project("VerifierHandoff", "[single] 旧验收合并交接")
            running_role(verifier_id, "verifier")
            before = call("GET", f"/api/projects/{verifier_id}")
            analysis_before = len(
                [job for job in before["jobs"] if job["role"] == "analysis"]
            )
            request_replan(verifier_id, "旧 verifier 完成后重新规划")
            detail = wait_draft(verifier_id)
            verifier = next(
                job for job in detail["jobs"] if job["role"] == "verifier"
            )
            assert verifier["status"] == "succeeded", verifier
            analysis_jobs = [job for job in detail["jobs"] if job["role"] == "analysis"]
            assert len(analysis_jobs) == analysis_before + 1, analysis_jobs
            old = next(task for task in detail["tasks"] if task["key"] == "T1")
            assert old["stage"] == "completed" and old["status"] != "canceled", old
            assert "T1" in detail["proposal"]["replan_impact"]["retained_task_keys"]
            branch = detail["project"]["branch"]
            assert "T1" in git(["show", f"{branch}:domo_mock/T1.txt"], repo)
            detail = call("POST", f"/api/projects/{verifier_id}/analysis/confirm", {})
            old = next(task for task in detail["tasks"] if task["id"] == old["id"])
            assert old["status"] != "canceled", old

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
            detail = wait_draft(derive_id)
            impact = detail["proposal"]["replan_impact"]
            assert "T1" in impact["canceled_task_keys"], impact
            assert any(child["key"] in impact["canceled_task_keys"] for child in children), impact
            assert all(child["key"] not in impact["retained_task_keys"] for child in children)
            analysis_job = sorted(
                (job for job in detail["jobs"] if job["role"] == "analysis"),
                key=lambda job: job["created_at"],
            )[-1]
            prompt = call("GET", f"/api/jobs/{analysis_job['id']}")["prompt"]
            retained_prompt = prompt.split("完整保留的交付任务链", 1)[1].split(
                "不保留任务", 1
            )[0]
            assert all(child["key"] not in retained_prompt for child in children), prompt
            canceled_prompt = prompt.split("不保留任务", 1)[1]
            assert any(child["key"] in canceled_prompt for child in children), prompt
            detail = call("POST", f"/api/projects/{derive_id}/analysis/confirm", {})
            family = [
                task
                for task in detail["tasks"]
                if task["id"] == parent["id"] or task["parent_id"] == parent["id"]
            ]
            assert all(task["status"] == "canceled" for task in family), family
            for child in children:
                if child["stage"] == "completed":
                    result = run(
                        ["git", "show", f"{detail['project']['branch']}:domo_mock/{child['key']}.txt"],
                        cwd=repo,
                        check=False,
                    )
                    assert result.returncode != 0, child

            print("旧作业静默、Git 交接与派生任务链测试通过")


if __name__ == "__main__":
    main()
