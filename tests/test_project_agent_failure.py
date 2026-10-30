#!/usr/bin/env python3
"""DOMO 项目级作业（规划者）失败降级测试。

覆盖范围：
  1. 项目分析时高优先级规划 Agent 被限流：Agent 进入冷却，项目不阻塞，
     由下一优先级的规划 Agent 自动接手并给出方案；
  2. 总结时高优先级规划 Agent 被限流：同样自动切换，总结作业由下一
     优先级的规划 Agent 完成；
  3. 项目池里只剩一个规划 Agent 且被限流：项目阻塞并记录限流原因，调度器
     不会自行重派；人工重试失败作业会重新派给该 Agent，并在派发成功后
     清除它的冷却。

使用内置 mock Agent 与脚本化 CLI Agent，不访问外网；临时数据全部放在
系统临时目录。退出码 0 表示全部通过。
"""

import sys
import time

from harness import Server, init_repo, temp_root, wait_for


PLANNER_FIXTURE = """\
import json
import os
import sys

job_dir = sys.argv[1]
role = os.environ.get("DOMO_JOB_ROLE", "")
if role == "analysis":
    result = {
        "kind": "plan",
        "summary": "脚本化规划者给出方案",
        "assumptions": [],
        "acceptance": ["产物符合目标描述"],
        "tasks": [
            {
                "key": "T1",
                "title": "写入基础文件",
                "description": "在 domo_mock/T1.txt 写入 T1。",
                "acceptance": ["domo_mock/T1.txt 存在"],
                "deps": [],
            }
        ],
        "skip_confirmation": True,
    }
elif role == "summary":
    result = {"kind": "report", "summary": "脚本化报告：全部任务已完成。"}
else:
    result = {"kind": "failed", "reason": "脚本化规划 Agent 只承担项目分析"}
with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
    json.dump(result, handle, ensure_ascii=False)
"""


def main():
    with temp_root("domo-project-failure-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        planner = temp / "agents" / "planner.py"
        planner.parent.mkdir(parents=True, exist_ok=True)
        planner.write_text(PLANNER_FIXTURE, encoding="utf-8")

        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "60",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "60",
                "DOMO_MOCK_PLANNER_RATE_LIMIT": "1",
                "DOMO_MOCK_PLANNER_RATE_LIMIT_AGENT": "Flaky Planner",
                "DOMO_MOCK_PLANNER_RATE_LIMIT_RETRY": "4",
            },
        ) as server:
            def call(method, path, data=None):
                return server.call(method, path, data)

            def wait_project(project_id, predicate, label, timeout=60):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            def planner_entries(project_id):
                pool = call("GET", f"/api/projects/{project_id}/agents")
                return next(
                    role["entries"] for role in pool["roles"] if role["role"] == "planner"
                )

            def create_project(name, goal):
                return call(
                    "POST",
                    "/api/projects",
                    {
                        "name": name,
                        "goal": goal,
                        "repo_path": str(repo),
                        "base_branch": "main",
                    },
                )["project"]["id"]

            def submit(project_id, goal):
                call(
                    "POST",
                    f"/api/projects/{project_id}/analysis/messages",
                    {"message": goal, "confirm_mode": "auto"},
                )

            def role_jobs(detail, role):
                return [job for job in detail["jobs"] if job["role"] == role]

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})

            flaky = call(
                "POST",
                "/api/agents",
                {
                    "name": "Flaky Planner",
                    "kind": "mock",
                    "model": "mock",
                    "roles": ["planner"],
                    "max_concurrency": 1,
                },
            )["id"]
            backup = call(
                "POST",
                "/api/agents",
                {
                    "name": "Backup Planner",
                    "kind": "cli",
                    "model": "scripted",
                    "roles": ["planner"],
                    "command": [sys.executable, str(planner), "{job_dir}"],
                    "max_concurrency": 1,
                },
            )["id"]
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Mock Worker",
                    "kind": "mock",
                    "model": "mock",
                    "roles": ["executor", "verifier"],
                    "max_concurrency": 2,
                },
            )
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

            # 场景一：项目分析被限流，自动切换到下一优先级的规划 Agent。
            project_id = create_project("Planner Limit", "写入 domo_mock 产物")
            call(
                "PATCH",
                f"/api/projects/{project_id}/agents/planner/{steady}",
                {"enabled": False},
            )
            entries = [
                entry["agent_id"]
                for entry in planner_entries(project_id)
                if entry["enabled"]
            ]
            assert entries == [flaky, backup], entries

            submit(project_id, "写入 domo_mock 产物")
            detail = wait_project(
                project_id,
                lambda detail: any(
                    job["role"] == "analysis"
                    and job["agent_id"] == backup
                    and job["status"] == "succeeded"
                    for job in detail["jobs"]
                ),
                "项目分析切换到下一优先级",
            )
            jobs = role_jobs(detail, "analysis")
            assert [(job["agent_id"], job["status"]) for job in jobs] == [
                (flaky, "failed"),
                (backup, "succeeded"),
            ], jobs
            assert jobs[0]["failure_kind"] == "rate_limit", jobs
            assert "429" in (jobs[0]["error"] or ""), jobs
            assert detail["project"]["status"] != "blocked", detail["project"]
            assert detail["tasks"], detail

            flaky_row = next(
                agent for agent in call("GET", "/api/agents") if agent["id"] == flaky
            )
            assert flaky_row["cooldown_until"], flaky_row
            assert flaky_row["failure_kind"] == "rate_limit", flaky_row

            messages = [(event["kind"], event["message"]) for event in detail["events"]]
            assert (
                "project_retry",
                "项目分析失败（限流），已切换 Agent 重试："
                "HTTP 429 too many requests; retry after 4 seconds",
            ) in messages, messages
            assert any(
                kind == "job_queued" and "Backup Planner" in message
                for kind, message in messages
            ), messages

            wait_project(
                project_id,
                lambda detail: detail["project"]["stage"] == "summarizing"
                and detail["project"].get("report"),
                "项目产出报告",
            )

            # 场景二：项目池里唯一的规划 Agent 被限流，项目阻塞并等待人工处理。
            call("PATCH", f"/api/team/default/planner/{backup}", {"enabled": False})
            project_id = create_project("Only Planner", "写入 domo_mock 产物")
            call(
                "PATCH",
                f"/api/projects/{project_id}/agents/planner/{steady}",
                {"enabled": False},
            )
            submit(project_id, "写入 domo_mock 产物")
            detail = wait_project(
                project_id,
                lambda detail: detail["project"]["status"] == "blocked"
                and "429" in (detail["project"]["blocked_reason"] or "")
                and not any(job["status"] == "running" for job in detail["jobs"]),
                "唯一规划 Agent 被限流后阻塞",
            )
            assert len(role_jobs(detail, "analysis")) == 1, role_jobs(detail, "analysis")
            assert (
                role_jobs(detail, "analysis")[0]["failure_kind"] == "rate_limit"
            ), role_jobs(detail, "analysis")
            flaky_row = next(
                agent for agent in call("GET", "/api/agents") if agent["id"] == flaky
            )
            assert flaky_row["cooldown_until"], flaky_row

            time.sleep(1.5)
            detail = call("GET", f"/api/projects/{project_id}")
            assert detail["project"]["status"] == "blocked", detail["project"]
            assert len(role_jobs(detail, "analysis")) == 1, role_jobs(detail, "analysis")

            call(
                "PATCH",
                f"/api/agents/{flaky}",
                {"roles": ["planner", "executor", "verifier"]},
            )
            call("PATCH", f"/api/agents/{flaky}", {"enabled": True})
            failed_job = role_jobs(detail, "analysis")[0]
            call("POST", f"/api/jobs/{failed_job['id']}/retry", {})
            detail = wait_project(
                project_id,
                lambda detail: detail["project"]["status"] == "blocked"
                and "429" in (detail["project"]["blocked_reason"] or "")
                and len(role_jobs(detail, "analysis")) >= 2
                and not any(job["status"] == "running" for job in detail["jobs"]),
                "人工重试后重新派发并再次限流",
            )
            resent = role_jobs(detail, "analysis")[1]
            assert resent["agent_id"] == flaky, resent
            assert resent["failure_kind"] == "rate_limit", resent

            print("全部场景通过")


if __name__ == "__main__":
    main()
