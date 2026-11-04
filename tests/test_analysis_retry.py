#!/usr/bin/env python3
"""DOMO 重规划后的项目分析重试测试。

覆盖范围：
  1. 重规划引发的项目分析失败后，调度器在原 Agent 上继续重试，不回到等待人工输入；
  2. 供应商重试耗尽后保留自动恢复等待，不产生人工待办；可放弃重规划恢复旧报告。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
"""

from harness import Server, init_repo, temp_root, wait_for


def main():
    with temp_root("domo-analysis-retry-") as temp:
        repo = temp / "repo"
        init_repo(repo)

        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "60",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "60",
                "DOMO_TRANSIENT_RETRY_LIMIT": "2",
                "DOMO_MOCK_PLANNER_TRANSIENT": "1",
                "DOMO_MOCK_PLANNER_TRANSIENT_AGENT": "Flaky Replan Planner",
            },
        ) as server:
            call = server.call

            def wait_project(project_id, predicate, label, timeout=120):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            def analysis_jobs(detail):
                return [job for job in detail["jobs"] if job["role"] == "analysis"]

            def project_summary():
                return next(
                    item for item in call("GET", "/api/projects")
                    if item["id"] == project_id
                )

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            steady = call(
                "POST",
                "/api/agents",
                {
                    "name": "Steady Planner",
                    "kind": "mock",
                    "model": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )["id"]
            flaky = call(
                "POST",
                "/api/agents",
                {
                    "name": "Flaky Replan Planner",
                    "kind": "mock",
                    "model": "mock",
                    "roles": ["planner"],
                    "max_concurrency": 1,
                },
            )["id"]

            goal = "写入 domo_mock 产物"
            project_id = call(
                "POST",
                "/api/projects",
                {
                    "name": "Analysis Retry",
                    "goal": goal,
                    "confirm_mode": "auto",
                    "repo_path": str(repo),
                    "base_branch": "main",
                },
            )["project"]["id"]
            detail = wait_project(
                project_id,
                lambda item: item["project"]["stage"] == "summarizing"
                and item["project"].get("report"),
                "首次项目分析与总结完成",
            )
            # 首次分析由默认团队中优先级更高的规划 Agent 完成。
            first_analysis = analysis_jobs(detail)
            assert first_analysis and first_analysis[-1]["agent_id"] == steady, first_analysis
            assert detail["messages"][-1]["role"] == "user", detail["messages"]

            # 报告验收后重新规划，只留下会持续失败的规划 Agent。
            call(
                "PATCH",
                f"/api/projects/{project_id}/agents/planner/{steady}",
                {"enabled": False},
            )
            call(
                "POST",
                f"/api/projects/{project_id}/report",
                {"action": "replan", "feedback": "重规划后验证项目分析自动重试"},
            )
            detail = wait_project(
                project_id,
                lambda item: item["project"]["status"] == "running"
                and "冷却" in (item["project"]["blocked_reason"] or "")
                and not any(
                    job["role"] == "analysis" and job["status"] == "running"
                    for job in item["jobs"]
                ),
                "重规划后的项目分析等待冷却",
            )
            assert detail["project"]["report"], "分析失败不应清空旧报告"
            assert detail["tasks"] and all(
                task["status"] != "canceled" for task in detail["tasks"]
            ), "分析失败不应取消旧计划任务"
            jobs = analysis_jobs(detail)
            assert len(jobs) == len(first_analysis) + 2, jobs
            retried = jobs[len(first_analysis):]
            assert all(job["agent_id"] == flaky for job in retried), retried
            assert all(
                job["status"] == "failed" and job["failure_kind"] == "transient"
                for job in retried
            ), retried
            assert project_summary()["attention_items"] == []
            assert not project_summary()["attention_items"]

            detail = call("POST", f"/api/projects/{project_id}/replan/abandon")
            assert detail["project"]["stage"] == "summarizing", detail["project"]
            assert detail["project"]["report"], detail["project"]
            assert all(task["status"] != "canceled" for task in detail["tasks"])

            print("项目分析重试与失败后放弃重规划场景通过")


if __name__ == "__main__":
    main()
