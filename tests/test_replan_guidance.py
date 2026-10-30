#!/usr/bin/env python3
"""DOMO 人工指导与重规划闭环测试。

覆盖范围：
  1. 人工指导保存后可查询审计事件，并出现在任务下一次执行作业的 prompt 中；
  2. 执行者返回 replan 后作业按成功处理，产生重规划事件，旧计划被作废；
  3. 已完成任务保留，未完成任务退出活动 DAG，新规划可复用旧 key 并最终产出报告；
  4. 暂停中的项目可提交重规划，暂停期间不产生新的规划作业，恢复后按新计划推进；
  5. 归档中的项目拒绝重规划，状态和规划作业数量不变；
  6. 使用同一数据目录重启服务后，指导、重规划和取消任务记录仍可读取，schema 初始化幂等。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
"""

import json
import time
import urllib.error
import urllib.request

from harness import (
    Server,
    ensure_server_built,
    init_repo,
    temp_root,
    wait_for,
)

REPLAN_ENV = {
    "DOMO_MOCK_DELAY_MS": "80",
    "DOMO_MOCK_EXECUTOR_DELAY_MS": "250",
    "DOMO_MOCK_REPLAN_ONCE": "1",
    "DOMO_MOCK_REPLAN_GOAL": "[replan]",
    "DOMO_MOCK_REPLAN_KEY": "T2",
}


def main():
    with temp_root("domo-replan-guidance-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(temp, env=REPLAN_ENV) as server:
            pending_goals = {}

            def call(method, path, data=None):
                if (
                    method == "POST"
                    and path == "/api/projects"
                    and isinstance(data, dict)
                    and "goal" in data
                ):
                    goal = data.get("goal")
                    payload = {
                        key: value for key, value in data.items() if key != "goal"
                    }
                    result = server.call("POST", path, payload)
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
                    detail = server.call("GET", f"/api/projects/{project_id}")
                    if not detail["messages"] and project_id in pending_goals:
                        server.call(
                            "POST",
                            f"/api/projects/{project_id}/analysis/messages",
                            {
                                "message": pending_goals.pop(project_id),
                                "confirm_mode": "auto",
                            },
                        )
                    message = (data or {}).get("message")
                    if message:
                        server.call(
                            "POST",
                            f"/api/projects/{project_id}/analysis/messages",
                            {"message": message, "confirm_mode": "auto"},
                        )
                    deadline = time.time() + 30
                    while time.time() < deadline:
                        detail = server.call("GET", f"/api/projects/{project_id}")
                        if detail["project"]["stage"] != "analysis":
                            return detail
                        if detail.get("proposal") and not detail["jobs"]:
                            return server.call(
                                "POST",
                                f"/api/projects/{project_id}/analysis/confirm",
                                {},
                            )
                        last = (
                            detail["messages"][-1] if detail["messages"] else None
                        )
                        if (
                            last
                            and last["role"] == "planner"
                            and not detail.get("proposal")
                        ):
                            return detail
                        time.sleep(0.1)
                    return server.call("GET", f"/api/projects/{project_id}")
                return server.call(method, path, data)

            def call_error(method, path, data=None):
                body = json.dumps(data).encode() if data is not None else None
                request = urllib.request.Request(
                    server.base + path,
                    data=body,
                    method=method,
                    headers={"Content-Type": "application/json"},
                )
                try:
                    with urllib.request.urlopen(request, timeout=60) as response:
                        response.read()
                        return response.status, None
                except urllib.error.HTTPError as error:
                    return error.code, error.read().decode(
                        "utf-8", errors="replace"
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
                    {
                        "name": name,
                        "goal": goal,
                        "repo_path": str(repo),
                        "base_branch": "main",
                    },
                )["project"]
                call("POST", f"/api/projects/{project['id']}/confirm", {})
                return project["id"]

            def goal_jobs(detail):
                return [
                    job for job in detail["jobs"] if job["role"] == "analysis"
                ]

            def active_tasks(detail):
                return [
                    task for task in detail["tasks"] if task["status"] != "canceled"
                ]

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Replan Guidance Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )
            # 新建项目的项目池继承默认团队。

            # 1. 人工指导写入 prompt 和审计。
            guidance_project = new_project(
                "Guidance", "生成两个有依赖的 mock 文件"
            )
            detail = wait_project(
                guidance_project,
                lambda item: item["project"]["stage"] == "executing"
                and len(item["tasks"]) >= 2,
                "指导项目规划完成",
                60,
            )
            guidance_task = next(
                task for task in detail["tasks"] if task["key"] == "T2"
            )
            guidance_text = "DOMO-GUIDANCE-ALPHA：优先确认前置产物，再写入 T2。"
            saved = call(
                "POST",
                f"/api/projects/{guidance_project}/tasks/{guidance_task['id']}/guidance",
                {"guidance": guidance_text},
            )
            assert saved["guidance"] == guidance_text, saved

            deadline = time.time() + 60
            prompt = None
            while time.time() < deadline:
                detail = call("GET", f"/api/projects/{guidance_project}")
                job = next(
                    (
                        item
                        for item in detail["jobs"]
                        if item["task_id"] == guidance_task["id"]
                        and item["role"] == "executor"
                    ),
                    None,
                )
                if job:
                    job_detail = call("GET", f"/api/jobs/{job['id']}")
                    prompt = job_detail["prompt"]
                    if guidance_text in prompt:
                        break
                time.sleep(0.1)
            else:
                raise AssertionError("下一次执行作业的 prompt 未包含人工指导")
            assert guidance_text in prompt

            guidance_events = call(
                "GET", f"/api/events?project_id={guidance_project}"
            )
            assert any(
                event["kind"] == "guidance"
                and guidance_text in event["message"]
                for event in guidance_events
            ), "审计事件中缺少人工指导原文"

            detail = wait_project(
                guidance_project,
                lambda item: item["project"]["stage"] == "summarizing"
                and item["project"]["report"],
                "指导项目报告",
            )
            call(
                "POST",
                f"/api/projects/{guidance_project}/report",
                {"action": "accept"},
            )
            code, _ = call_error(
                "POST",
                f"/api/projects/{guidance_project}/tasks/{guidance_task['id']}/guidance",
                {"guidance": "已完成任务不应接受新的指导"},
            )
            assert code == 400, code

            # 2. 执行者请求重规划，旧计划作废并生成新计划。
            replan_project = new_project(
                "Replan", "[replan] 验证执行者请求重新规划"
            )
            detail = wait_project(
                replan_project,
                lambda item: item["project"]["stage"] == "executing"
                and len(item["tasks"]) == 2,
                "重规划项目首次规划",
                60,
            )
            old_tasks = {task["id"]: task for task in detail["tasks"]}
            old_ids = set(old_tasks)
            old_t1 = next(
                task for task in detail["tasks"] if task["key"] == "T1"
            )
            old_t2 = next(
                task for task in detail["tasks"] if task["key"] == "T2"
            )

            detail = wait_project(
                replan_project,
                lambda item: any(
                    event["kind"] == "replan_requested"
                    for event in item["events"]
                ),
                "执行者重规划事件",
                90,
            )
            detail = wait_project(
                replan_project,
                lambda item: item["project"]["stage"] == "summarizing"
                and item["project"]["report"],
                "重规划后的项目报告",
            )
            assert detail["project"]["replan_source"] == "executor", detail[
                "project"
            ]
            assert "原计划与仓库现状冲突" in (
                detail["project"]["replan_reason"] or ""
            ), detail["project"]
            assert detail["project"]["status"] == "blocked", detail["project"]

            tasks_by_id = {task["id"]: task for task in detail["tasks"]}
            assert tasks_by_id[old_t1["id"]]["stage"] == "completed", tasks_by_id[
                old_t1["id"]
            ]
            assert tasks_by_id[old_t2["id"]]["status"] == "canceled", tasks_by_id[
                old_t2["id"]
            ]
            assert all(
                task["status"] == "canceled"
                for task in tasks_by_id.values()
                if task["stage"] != "completed"
            )
            new_tasks = [
                task for task in detail["tasks"] if task["id"] not in old_ids
            ]
            assert new_tasks, detail["tasks"]
            assert min(task["seq"] for task in new_tasks) > max(
                task["seq"] for task in old_tasks.values()
            ), detail["tasks"]
            assert all(
                task["stage"] == "completed" and task["status"] is None
                for task in new_tasks
            ), new_tasks
            assert any(
                task["key"] == "T2" for task in new_tasks
            ), "新规划未能复用被取消任务的 key"
            assert all(
                task["stage"] == "completed" for task in active_tasks(detail)
            ), active_tasks(detail)

            kinds = {event["kind"] for event in detail["events"]}
            assert "replan_requested" in kinds, kinds
            assert "plan_superseded" in kinds, kinds
            assert detail["project"]["replan_requested_at"], detail["project"]

            planner_jobs = goal_jobs(detail)
            assert len(planner_jobs) >= 2, planner_jobs
            planner_prompt = call(
                "GET", f"/api/jobs/{planner_jobs[-1]['id']}"
            )["prompt"]
            assert "重规划上下文" in planner_prompt
            assert "原计划与仓库现状冲突" in planner_prompt
            assert (
                old_t1["key"] in planner_prompt
                and "已完成任务" in planner_prompt
            )
            assert (
                old_t2["key"] in planner_prompt
                and "已取消任务" in planner_prompt
            )
            replan_reason = detail["project"]["replan_reason"]
            code, _ = call_error(
                "POST",
                f"/api/projects/{replan_project}/tasks/{old_t2['id']}/guidance",
                {"guidance": "已取消任务不应接受新的指导"},
            )
            assert code == 400, code

            # 3. 暂停中的重规划不产生重复规划作业，恢复后按新计划推进。
            paused_project = new_project("PausedReplan", "暂停中的重规划测试")
            detail = wait_project(
                paused_project,
                lambda item: item["project"]["stage"] == "executing"
                and len(item["tasks"]) == 2,
                "暂停项目首次规划",
                60,
            )
            call("POST", f"/api/projects/{paused_project}/pause", {})
            detail = wait_project(
                paused_project,
                lambda item: item["project"]["status"] == "paused",
                "项目暂停完成",
                60,
            )
            goal_before = len(goal_jobs(detail))
            paused_reason = "暂停时人工补充的重规划原因：需要调整后续任务边界。"
            paused_detail = call(
                "POST",
                f"/api/projects/{paused_project}/replan",
                {"reason": paused_reason},
            )
            assert paused_detail["project"]["stage"] == "analysis", paused_detail[
                "project"
            ]
            assert paused_detail["project"]["status"] == "paused", paused_detail[
                "project"
            ]
            assert paused_detail["project"]["replan_source"] == "human", paused_detail[
                "project"
            ]
            assert any(
                task["status"] == "canceled" for task in paused_detail["tasks"]
            ), paused_detail["tasks"]

            time.sleep(1.5)
            paused_detail = call("GET", f"/api/projects/{paused_project}")
            assert paused_detail["project"]["stage"] == "analysis", paused_detail[
                "project"
            ]
            assert paused_detail["project"]["status"] == "paused", paused_detail[
                "project"
            ]
            assert (
                len(goal_jobs(paused_detail)) == goal_before
            ), "暂停期间不应产生新的项目分析作业"

            call("POST", f"/api/projects/{paused_project}/resume", {})
            detail = wait_project(
                paused_project,
                lambda item: item["project"]["stage"] == "summarizing"
                and item["project"]["report"],
                "暂停恢复后的项目报告",
            )
            assert all(
                task["stage"] == "completed" for task in active_tasks(detail)
            ), active_tasks(detail)
            assert (
                len(goal_jobs(detail)) == goal_before + 1
            ), "恢复后应只产生一次新的项目分析作业"
            assert detail["project"]["replan_reason"] == paused_reason, detail[
                "project"
            ]

            # 4. 归档中的重规划被拒绝，不改变状态、不产生规划作业。
            archived_project = new_project("ArchiveReplan", "归档中的重规划测试")
            detail = wait_project(
                archived_project,
                lambda item: item["project"]["stage"] == "executing"
                and len(item["tasks"]) >= 1,
                "归档项目首次规划",
                60,
            )
            call("POST", f"/api/projects/{archived_project}/archive", {})
            archived_detail = call("GET", f"/api/projects/{archived_project}")
            assert archived_detail["project"]["status"] == "archived", archived_detail[
                "project"
            ]
            goal_before_archive = len(goal_jobs(archived_detail))
            replan_events_before = [
                event
                for event in archived_detail["events"]
                if event["kind"] == "replan_requested"
            ]
            code, body = call_error(
                "POST",
                f"/api/projects/{archived_project}/replan",
                {"reason": "归档后不应生效的重规划"},
            )
            assert code == 400, (code, body)
            time.sleep(1.0)
            archived_detail = call("GET", f"/api/projects/{archived_project}")
            assert archived_detail["project"]["status"] == "archived", archived_detail[
                "project"
            ]
            assert (
                len(goal_jobs(archived_detail)) == goal_before_archive
            ), "归档项目不应产生项目分析作业"
            assert [
                event
                for event in archived_detail["events"]
                if event["kind"] == "replan_requested"
            ] == replan_events_before, "归档项目不应写入重规划事件"
            code, _ = call_error(
                "POST",
                f"/api/projects/{archived_project}/tasks/{archived_detail['tasks'][0]['id']}/guidance",
                {"guidance": "归档项目不应接受新的指导"},
            )
            assert code == 400, code

            print("首次运行：指导、重规划、暂停与归档场景通过")

            # 使用同一数据目录重启服务。
            server.interrupt()
            server.restart()

            guidance_detail = call("GET", f"/api/projects/{guidance_project}")
            guidance_task_id = guidance_task["id"]
            restored_guidance_task = next(
                task
                for task in guidance_detail["tasks"]
                if task["id"] == guidance_task_id
            )
            assert (
                restored_guidance_task["guidance"] == guidance_text
            ), restored_guidance_task
            assert any(
                event["kind"] == "guidance"
                and guidance_text in event["message"]
                for event in guidance_detail["events"]
            ), "重启后审计事件缺少人工指导"

            replan_detail = call("GET", f"/api/projects/{replan_project}")
            assert (
                replan_detail["project"]["replan_reason"] == replan_reason
            ), replan_detail["project"]
            assert replan_detail["project"]["replan_source"] == "executor", replan_detail[
                "project"
            ]
            tasks_by_id = {
                task["id"]: task for task in replan_detail["tasks"]
            }
            assert tasks_by_id[old_t1["id"]]["stage"] == "completed", tasks_by_id[
                old_t1["id"]
            ]
            assert tasks_by_id[old_t2["id"]]["status"] == "canceled", tasks_by_id[
                old_t2["id"]
            ]

            paused_detail = call("GET", f"/api/projects/{paused_project}")
            assert (
                paused_detail["project"]["replan_reason"] == paused_reason
            ), paused_detail["project"]
            assert any(
                task["status"] == "canceled" for task in paused_detail["tasks"]
            ), paused_detail["tasks"]

            archived_detail = call("GET", f"/api/projects/{archived_project}")
            assert archived_detail["project"]["status"] == "archived", archived_detail[
                "project"
            ]

            print("重启后：指导、重规划和取消任务记录仍可读取")


if __name__ == "__main__":
    main()
