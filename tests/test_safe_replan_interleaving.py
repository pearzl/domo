#!/usr/bin/env python3
"""人工里程碑、运行时失败和报告动作与重规划的交错测试。"""

import concurrent.futures
import json
import urllib.error
import urllib.request

from harness import Server, ensure_server_built, init_repo, temp_root, wait_for


def raw_call(base, method, path, data=None):
    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(
        base + path,
        data=body,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raw = error.read()
        return error.code, json.loads(raw) if raw else None


def main():
    with temp_root("domo-safe-replan-interleaving-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()
        env = {
            "DOMO_MOCK_DELAY_MS": "60",
            "DOMO_MOCK_MILESTONE_MERGE_DELAY_MS": "500",
        }
        with Server(temp, env=env) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Interleaving Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )

            def wait_project(pid, predicate, label, timeout=120):
                def check():
                    detail = call("GET", f"/api/projects/{pid}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            def new_project(name, goal="[single] 交错测试"):
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

            def report_ready(pid):
                return wait_project(
                    pid,
                    lambda item: item["project"]["stage"] == "summarizing"
                    and bool(item["project"]["report"]),
                    "报告就绪",
                )

            def wait_draft(pid):
                return wait_project(
                    pid,
                    lambda item: item.get("proposal") is not None
                    and not any(
                        job["role"] == "analysis" and job["status"] == "running"
                        for job in item["jobs"]
                    ),
                    "重规划草稿",
                )

            # 人工验收的 Git 合并与数据库完成全程串行，replan 只能看到完成后的保留链。
            milestone_id = new_project("MilestoneInterleave")
            detail = wait_project(
                milestone_id,
                lambda item: item["project"]["stage"] == "executing" and item["tasks"],
                "里程碑任务",
            )
            task = detail["tasks"][0]
            call(
                "POST",
                f"/api/projects/{milestone_id}/tasks/{task['id']}/milestone",
                {"milestone": True},
            )
            detail = wait_project(
                milestone_id,
                lambda item: item["tasks"][0]["human_gate"] == "milestone",
                "等待人工里程碑",
            )
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                accepted = pool.submit(
                    raw_call,
                    server.base,
                    "POST",
                    f"/api/projects/{milestone_id}/tasks/{task['id']}/milestone/accept",
                    {"reason": "交错验收"},
                )
                wait_project(
                    milestone_id,
                    lambda item: item["tasks"][0]["human_gate"] == "milestone_accepting",
                    "里程碑合并中",
                )
                replanned = raw_call(
                    server.base,
                    "POST",
                    f"/api/projects/{milestone_id}/replan",
                    {"reason": "与人工验收交错"},
                )
                assert accepted.result()[0] == 200, accepted.result()
            assert replanned[0] == 200, replanned
            detail = wait_draft(milestone_id)
            assert "T1" in detail["proposal"]["replan_impact"]["retained_task_keys"]
            detail = call("POST", f"/api/projects/{milestone_id}/analysis/confirm", {})
            assert next(task for task in detail["tasks"] if task["key"] == "T1")[
                "status"
            ] != "canceled"

            # fail_runtime 在业务回调结束前保持 running，旧 direct 不能提前穿透屏障。
            server.env["DOMO_MOCK_RUNTIME_FAIL_ROLE"] = "direct"
            server.env["DOMO_MOCK_FAILURE_CALLBACK_DELAY_MS"] = "1200"
            server.restart()
            runtime_id = new_project("RuntimeFailureInterleave", "[direct] 运行时失败交错")
            running = wait_project(
                runtime_id,
                lambda item: any(
                    job["role"] == "direct" and job["status"] == "running"
                    for job in item["jobs"]
                ),
                "direct 运行时失败回调",
            )
            original_goal = running["project"]["goal"]
            status, _ = raw_call(
                server.base,
                "POST",
                f"/api/projects/{runtime_id}/replan",
                {"reason": "与 fail_runtime 交错"},
            )
            assert status == 200, status
            detail = wait_draft(runtime_id)
            direct = next(job for job in detail["jobs"] if job["role"] == "direct")
            assert direct["status"] == "canceled", direct
            assert detail["project"]["goal"] == original_goal
            assert detail["project"]["stage"] == "analysis"
            # 延迟期间强制取消后，旧异常回调和失败事件都不能落到新计划。
            server.env["DOMO_MOCK_RUNTIME_FAIL_ROLE"] = "direct"
            server.env["DOMO_MOCK_FAILURE_CALLBACK_DELAY_MS"] = "1200"
            server.restart()
            canceled_id = new_project("CanceledRuntimeFailure", "[direct] 强制取消异常回调")
            running = wait_project(
                canceled_id,
                lambda item: any(
                    job["role"] == "direct" and job["status"] == "running"
                    for job in item["jobs"]
                ),
                "待强制取消的 direct",
            )
            old_job = next(job for job in running["jobs"] if job["role"] == "direct")
            call("POST", f"/api/projects/{canceled_id}/pause", {"force": True})
            call(
                "POST",
                f"/api/projects/{canceled_id}/replan",
                {"reason": "强制取消后的新计划"},
            )
            call("POST", f"/api/projects/{canceled_id}/resume", {})
            detail = wait_draft(canceled_id)
            old_job_events = [
                event
                for event in detail["events"]
                if event.get("job_id") == old_job["id"]
                and event["kind"] in ("job_failed", "direct_failed")
            ]
            assert not old_job_events, old_job_events
            assert next(job for job in detail["jobs"] if job["id"] == old_job["id"])[
                "status"
            ] == "canceled"
            server.env.pop("DOMO_MOCK_RUNTIME_FAIL_ROLE")
            server.env.pop("DOMO_MOCK_FAILURE_CALLBACK_DELAY_MS")

            # accept/reject 与 replan 交错时，旧报告动作不能跨代际完成或改写新项目。
            for action in ("accept", "reject"):
                pid = new_project(f"Report-{action}")
                report_ready(pid)
                with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                    outcomes = list(
                        pool.map(
                            lambda fn: fn(None),
                            (
                                lambda _: raw_call(
                                    server.base,
                                    "POST",
                                    f"/api/projects/{pid}/report",
                                    {"action": action, "feedback": "旧报告处置"},
                                ),
                                lambda _: raw_call(
                                    server.base,
                                    "POST",
                                    f"/api/projects/{pid}/replan",
                                    {"reason": "与旧报告处置交错"},
                                ),
                            ),
                        )
                    )
                detail = call("GET", f"/api/projects/{pid}")
                if outcomes[1][0] == 200:
                    if action == "reject" and outcomes[0][0] == 200:
                        pass
                    else:
                        assert outcomes[0][0] in (400, 409), outcomes
                    assert detail["project"]["stage"] != "completed", outcomes
                else:
                    assert outcomes[1][0] == 400, outcomes
                    assert outcomes[0][0] == 200, outcomes

            print("人工里程碑、fail_runtime 与报告交错测试通过")


if __name__ == "__main__":
    main()
