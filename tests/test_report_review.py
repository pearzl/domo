#!/usr/bin/env python3
"""DOMO 项目总结报告多轮评议冒烟测试。

覆盖范围：
  1. 人工对报告提交意见（评议记录持久化为 messages，事件与作业提示词可回溯）；
  2. 报告被拒绝后规划者依据评议意见重写报告，多轮拒绝/重写可重复；
  3. 处置「要求重新规划」：项目回到项目分析阶段并生成分析作业；
  4. 处置「增加新任务」：复用人工加任务端点，项目回到执行阶段、新任务按 DAG 调度、原报告作废；
  5. 处置结果（report_rejected / replan_requested / task_created / project_reopened / project_completed）在事件流可查询。

约定：使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录；
脚本退出码 0 表示全部通过，结束时清理自己启动的进程与临时文件。
"""

import time

from harness import (
    Server,
    call as api,
    ensure_server_built,
    init_repo,
    temp_root,
    wait_for,
)


def main():
    with temp_root("domo-report-review-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(temp, env={"DOMO_MOCK_DELAY_MS": "80"}) as server:
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

            def wait_project(project_id, predicate, label, timeout=90):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            def project_report_ready(project_id, label):
                return wait_project(
                    project_id,
                    lambda d: d["project"]["stage"] == "summarizing"
                    and d["project"]["status"] == "blocked"
                    and bool(d["project"]["report"]),
                    label,
                )

            def wait_job_prompt(job_id, timeout=30):
                def check():
                    detail = call("GET", f"/api/jobs/{job_id}")
                    return detail if detail["prompt"] else None

                return wait_for(check, "作业 prompt", timeout)

            def all_events(project_id):
                return call("GET", f"/api/events?project_id={project_id}")

            agents = call("GET", "/api/agents")
            for agent in agents:
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            mock = call(
                "POST",
                "/api/agents",
                {
                    "name": "Report Review Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "priority": 0,
                    "max_concurrency": 3,
                },
            )
            assert mock["enabled"], mock
            # 默认团队随时快照，新项目从此继承。

            def make_project(name, goal):
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

            # ---- 阶段 A：多轮评议 + 重写 -----------------------------------------
            pid_a = make_project("ReviewA", "[single] 生成单任务 mock 文件")
            detail = project_report_ready(pid_a, "首轮报告")
            assert detail["project"]["report"].strip(), detail["project"]

            # 第 1 轮：先只记录意见（讨论，不触发处置）
            call(
                "POST",
                f"/api/projects/{pid_a}/report",
                {
                    "action": "comment",
                    "feedback": "请补充 ReqB 的实现细节说明",
                },
            )
            detail = call("GET", f"/api/projects/{pid_a}")
            reviews = [m for m in detail["messages"] if m["role"] == "review"]
            assert len(reviews) == 1 and "ReqB" in reviews[0]["content"], detail["messages"]
            assert any(
                e["kind"] == "report_opinion" and "ReqB" in e["message"]
                for e in detail["events"]
            ), detail["events"]

            # 第 1 次拒绝：记录意见并触发重写
            call(
                "POST",
                f"/api/projects/{pid_a}/report",
                {
                    "action": "reject",
                    "feedback": "缺少 ReqA 的实现细节，报告不具体",
                },
            )
            detail = project_report_ready(pid_a, "第 2 轮报告")
            # 新版报告生成后，上一轮意见随 report_feedback 清空，历史意见留在评议记录里。
            assert detail["project"]["report_feedback"] is None, detail["project"]
            assert detail["project"]["report"].count("已回应第") >= 1, detail["project"]["report"]
            assert "缺少 ReqA 的实现细节" in detail["project"]["report"], detail["project"]["report"]
            assert detail["project"]["status"] == "blocked", detail["project"]

            summary_jobs = [j for j in detail["jobs"] if j["role"] == "summary"]
            assert len(summary_jobs) >= 2, summary_jobs
            rewrite_jobs = sorted(summary_jobs, key=lambda j: j["created_at"])
            rewrite = call("GET", f"/api/jobs/{rewrite_jobs[-1]['id']}")
            assert "评议记录" in rewrite["prompt"], rewrite["prompt"]
            assert "缺少 ReqA 的实现细节" in rewrite["prompt"], rewrite["prompt"]
            assert "ReqB" in rewrite["prompt"], rewrite["prompt"]

            # 第 2 次拒绝：多轮可重复
            call(
                "POST",
                f"/api/projects/{pid_a}/report",
                {
                    "action": "reject",
                    "feedback": "再补充验证情况说明",
                },
            )
            detail = project_report_ready(pid_a, "第 3 轮报告")
            reviews = [m for m in detail["messages"] if m["role"] == "review"]
            assert len(reviews) == 3, detail["messages"]
            summary_jobs = [j for j in detail["jobs"] if j["role"] == "summary"]
            assert len(summary_jobs) >= 3, summary_jobs

            events = all_events(pid_a)
            kinds = [e["kind"] for e in events]
            assert kinds.count("report_opinion") >= 3, kinds
            assert kinds.count("report_rejected") >= 2, kinds
            assert kinds.count("report_ready") >= 3, kinds

            # 人工再次确认
            call("POST", f"/api/projects/{pid_a}/report", {"action": "accept"})
            detail = call("GET", f"/api/projects/{pid_a}")
            assert detail["project"]["stage"] == "completed", detail["project"]
            assert detail["project"]["status"] is None, detail["project"]

            # ---- 阶段 B：处置「要求重新规划」-------------------------------------
            pid_b = make_project("ReviewB", "[single] 生成单任务 mock 文件")
            project_report_ready(pid_b, "阶段 B 报告")
            before = call("GET", f"/api/projects/{pid_b}")
            analysis_before = len([j for j in before["jobs"] if j["role"] == "analysis"])
            call(
                "POST",
                f"/api/projects/{pid_b}/report",
                {
                    "action": "replan",
                    "feedback": "范围太小，需要纳入对 ReqC 的支持，请重新规划",
                },
            )
            detail = wait_project(
                pid_b,
                lambda d: d["project"]["stage"] in ("analysis", "executing")
                and len([j for j in d["jobs"] if j["role"] == "analysis"])
                > analysis_before,
                "回到项目分析阶段",
                45,
            )
            assert detail["project"]["report"] is None, detail["project"]
            events = all_events(pid_b)
            kinds = [e["kind"] for e in events]
            assert "replan_requested" in kinds and "plan_superseded" in kinds, kinds
            replan_event = next(e for e in events if e["kind"] == "replan_requested")
            assert "人工" in replan_event["message"] and "ReqC" in replan_event["message"], (
                replan_event
            )
            goal_jobs = [j for j in detail["jobs"] if j["role"] == "analysis"]
            assert goal_jobs, detail["jobs"]
            plan_job = wait_job_prompt(
                sorted(goal_jobs, key=lambda j: j["created_at"])[-1]["id"]
            )
            assert "ReqC" in plan_job["prompt"], plan_job["prompt"]

            # 重规划按现有路径继续走完：新任务执行并再次总结
            detail = wait_project(
                pid_b,
                lambda d: d["project"]["stage"] == "summarizing"
                and bool(d["project"]["report"]),
                "重规划后的报告",
            )
            assert detail["project"]["replan_reason"].endswith("请重新规划"), detail["project"]
            call("POST", f"/api/projects/{pid_b}/report", {"action": "accept"})
            assert call("GET", f"/api/projects/{pid_b}")["project"]["stage"] == "completed"

            # ---- 阶段 C：处置「增加新任务」（复用人工加任务端点）------------------
            pid_c = make_project("ReviewC", "[single] 生成单任务 mock 文件")
            project_report_ready(pid_c, "阶段 C 报告")
            old_report = call("GET", f"/api/projects/{pid_c}")["project"]["report"]
            assert old_report, old_report

            call(
                "POST",
                f"/api/projects/{pid_c}/tasks",
                {
                    "key": "T2",
                    "title": "补充产物核查文件",
                    "description": "在 domo_mock/T2.txt 写入核查记录，并确认 domo_mock/T1.txt 存在。",
                    "acceptance": ["domo_mock/T2.txt 存在且包含 T2"],
                    "deps": ["T1"],
                },
            )
            detail = call("GET", f"/api/projects/{pid_c}")
            assert detail["project"]["stage"] == "executing", detail["project"]
            assert detail["project"]["report"] is None, detail["project"]
            tasks = {task["key"]: task for task in detail["tasks"]}
            assert tasks["T2"]["deps"] == ["T1"], tasks["T2"]
            # 新任务创建时为 pending；调度器可能已开始执行，读取详情时允许两种状态。
            assert tasks["T2"]["stage"] in ("pending", "executing"), tasks["T2"]

            events = all_events(pid_c)
            kinds = [e["kind"] for e in events]
            assert "task_created" in kinds and "project_reopened" in kinds, kinds
            reopened = next(e for e in events if e["kind"] == "project_reopened")
            assert "作废" in reopened["message"], reopened

            detail = wait_project(
                pid_c,
                lambda d: d["project"]["stage"] == "summarizing"
                and bool(d["project"]["report"]),
                "加任务后的新报告",
            )
            task_t2 = next(task for task in detail["tasks"] if task["key"] == "T2")
            assert task_t2["stage"], task_t2
            call("POST", f"/api/projects/{pid_c}/report", {"action": "accept"})
            assert call("GET", f"/api/projects/{pid_c}")["project"]["stage"] == "completed"

            # ---- 反合法输入 ------------------------------------------------------
            errors = []
            try:
                call(
                    "POST",
                    f"/api/projects/{pid_c}/report",
                    {"action": "reject", "feedback": "  "},
                )
            except AssertionError as error:
                errors.append(str(error))
            assert any("填写意见" in text or "等待验收" in text for text in errors), errors

            print("报告多轮评议冒烟测试通过")
            print(f"  评议记录: {len(reviews)} 条")


if __name__ == "__main__":
    main()
