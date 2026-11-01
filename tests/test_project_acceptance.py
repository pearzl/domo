#!/usr/bin/env python3
"""项目整体验收冒烟测试。

覆盖范围：
  1. 全部任务完成后先派发项目验收作业，验收通过后才生成项目报告；
  2. 项目验收不通过时按验收条目自动重规划，新任务完成后再次验收；
  3. 项目验收连续未通过达到上限后阻塞，等待人工裁决；
  4. 项目验收与总结作业在作业记录和事件流中分别可查。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
"""

from harness import Server, ensure_server_built, init_repo, temp_root, wait_for


def make_server(temp, env):
    temp.mkdir(parents=True, exist_ok=True)
    return Server(
        temp,
        env={
            "DOMO_MOCK_DELAY_MS": "30",
            "DOMO_MOCK_EXECUTOR_DELAY_MS": "30",
            **env,
        },
    )


def prepare_agents(call):
    for agent in call("GET", "/api/agents"):
        call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
    call(
        "POST",
        "/api/agents",
        {
            "name": "Acceptance Mock",
            "kind": "mock",
            "roles": ["planner", "executor", "verifier"],
            "max_concurrency": 2,
        },
    )


def create_running_project(call, repo, name, goal):
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


def wait_for_report(call, project_id, timeout=120):
    def check():
        detail = call("GET", f"/api/projects/{project_id}")
        if detail["project"]["stage"] == "summarizing" and detail["project"]["report"]:
            return detail
        return None

    return wait_for(check, "项目报告", timeout)


def test_acceptance_failure_replans_and_then_reports(temp, repo):
    with make_server(temp / "acceptance-replan", {"DOMO_MOCK_ACCEPT_FAIL_ONCE": "1"}) as server:
        call = server.call
        prepare_agents(call)
        project_id = create_running_project(
            call, repo, "AcceptanceReplan", "[single] 项目验收失败后重规划"
        )
        detail = wait_for_report(call, project_id)
        project = detail["project"]
        assert project["replan_source"] == "acceptance", project

        roles = [job["role"] for job in detail["jobs"]]
        assert roles.count("project_acceptance") >= 2, roles
        assert roles.count("summary") >= 1, roles

        events = detail["events"]
        kinds = [event["kind"] for event in events]
        assert "project_acceptance_failed" in kinds, kinds
        assert "acceptance_replan" in kinds, kinds
        assert "replan_requested" in kinds, kinds
        assert "project_accepted" in kinds, kinds
        assert "report_ready" in kinds, kinds
        assert kinds.index("project_acceptance_failed") < kinds.index("report_ready"), kinds

        replan_events = [
            event
            for event in events
            if event["kind"] == "replan_requested" and "项目验收" in event["message"]
        ]
        assert replan_events, events

        analysis_jobs = sorted(
            (job for job in detail["jobs"] if job["role"] == "analysis"),
            key=lambda job: job["created_at"],
        )
        replan_prompt = call("GET", f"/api/jobs/{analysis_jobs[-1]['id']}")["prompt"]
        assert "项目验收" in replan_prompt, replan_prompt
        assert "mock 项目验收发现未满足条目" in replan_prompt, replan_prompt
        assert "T1" in replan_prompt, replan_prompt

        completed = {task["key"] for task in detail["tasks"] if task["stage"] == "completed"}
        assert {"T1", "T2", "T3"} <= completed, detail["tasks"]


def test_human_required_items_do_not_replan(temp, repo):
    with make_server(temp / "acceptance-human", {"DOMO_MOCK_ACCEPT_HUMAN_ONLY": "1"}) as server:
        call = server.call
        prepare_agents(call)
        project_id = create_running_project(
            call, repo, "AcceptanceHuman", "[single] 人工待办不触发重规划"
        )
        detail = wait_for_report(call, project_id)
        events = detail["events"]
        kinds = [event["kind"] for event in events]
        assert "project_accepted" in kinds, kinds
        assert "report_ready" in kinds, kinds
        assert "acceptance_replan" not in kinds, kinds

        acceptance_job = next(
            job for job in detail["jobs"] if job["role"] == "project_acceptance"
        )
        job_detail = call("GET", f"/api/jobs/{acceptance_job['id']}")
        assert "human_required" in (job_detail.get("result") or ""), job_detail


def test_acceptance_loop_blocks_for_human(temp, repo):
    with make_server(temp / "acceptance-loop", {"DOMO_MOCK_ACCEPT_ALWAYS_FAIL": "1"}) as server:
        call = server.call
        prepare_agents(call)
        project_id = create_running_project(
            call, repo, "AcceptanceBlocked", "[single] 项目验收持续失败"
        )

        def blocked():
            detail = call("GET", f"/api/projects/{project_id}")
            if (
                detail["project"]["stage"] == "summarizing"
                and detail["project"]["status"] == "blocked"
                and "自动重规划次数已达上限"
                in (detail["project"]["blocked_reason"] or "")
            ):
                return detail
            return None

        detail = wait_for(blocked, "项目验收阻塞", 180)
        assert detail["project"]["report"] is None, detail["project"]
        kinds = [event["kind"] for event in detail["events"]]
        assert "project_acceptance_blocked" in kinds, kinds
        assert "report_ready" not in kinds, kinds


def main():
    with temp_root("domo-project-acceptance-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()
        test_acceptance_failure_replans_and_then_reports(temp, repo)
        test_human_required_items_do_not_replan(temp, repo)
        test_acceptance_loop_blocks_for_human(temp, repo)
    print("project acceptance: ok")


if __name__ == "__main__":
    main()
