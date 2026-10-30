#!/usr/bin/env python3
"""DOMO 卡死检测测试。

场景：
  1. 单 Agent 池（并发上限 2）并行执行 T1（完全静默）、T2（持续输出）与 T3。
     T1 在阈值内无输出被判定卡死：作业失败原因明确、并发槽位立即释放
     （重试作业在 T2 仍在运行时就拿到槽位），T2/T3 不受误伤并正常完成；
     T1 连续卡死后按既有重试预算阻塞，等待人工裁决。
  2. 池内还有下一优先级 Agent 时，卡死按瞬态失败退避，最终换用下一优先级
     Agent 重试并完成，过程中留下 job_stalled / task_retry 事件与失败原因。
  3. 持续输出但超过整体超时的作业仍按超时失败，与卡死路径互不干扰。

使用脚本化 CLI Agent 模拟静默/心跳，不访问外网；服务进程、Agent 进程与
临时目录全部由脚本清理。
"""

import json
import os
import sys
import time
from datetime import datetime

from harness import (
    Server,
    call as api,
    ensure_server_built,
    init_repo,
    process_alive,
    temp_root,
    wait_for,
)


STALL_SCRIPT = '''\
import json
import os
import sys
import time

job_dir, cwd, marker_root = sys.argv[1], sys.argv[2], sys.argv[3]
with open(os.path.join(job_dir, "context.json"), encoding="utf-8") as handle:
    context = json.load(handle)
key = (context.get("task") or {}).get("key") or ""
os.makedirs(os.path.join(cwd, "domo_mock"), exist_ok=True)
if key == "T1":
    # 静默挂起：不产生任何 stdout/stderr，等待 DOMO 卡死检测终止整个进程组。
    with open(os.path.join(marker_root, "t1.pid"), "w", encoding="utf-8") as handle:
        handle.write(str(os.getpid()))
    time.sleep(60)
    sys.exit(0)
# 持续输出代表仍在推进，不能被误判为卡死。
for _ in range(12):
    print(f"heartbeat {key}", flush=True)
    time.sleep(0.2)
with open(os.path.join(cwd, "domo_mock", f"{key}.txt"), "w", encoding="utf-8") as handle:
    handle.write(key + "\\n")
with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
    json.dump({"kind": "done", "summary": f"心跳完成 {key}"}, handle, ensure_ascii=False)
'''

RECOVERY_SCRIPT = '''\
import json
import os
import sys

job_dir, cwd, marker_root = sys.argv[1], sys.argv[2], sys.argv[3]
with open(os.path.join(job_dir, "context.json"), encoding="utf-8") as handle:
    context = json.load(handle)
key = (context.get("task") or {}).get("key") or ""
os.makedirs(os.path.join(cwd, "domo_mock"), exist_ok=True)
with open(os.path.join(cwd, "domo_mock", f"{key}.txt"), "w", encoding="utf-8") as handle:
    handle.write(key + "\\n")
with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
    json.dump({"kind": "done", "summary": f"恢复 Agent 完成 {key}"}, handle, ensure_ascii=False)
print(f"recovered {key}", flush=True)
'''

HEARTBEAT_SCRIPT = '''\
import time

# 持续输出但永不结束：只能由整体作业超时终止，不应触发卡死判定。
while True:
    print("still working", flush=True)
    time.sleep(0.2)
'''


def main():
    with temp_root("domo-stall-") as temp:
        repo = temp / "repo"
        agents_dir = temp / "agents"
        agents_dir.mkdir(parents=True, exist_ok=True)
        init_repo(repo)

        stall_script = agents_dir / "stall_agent.py"
        stall_script.write_text(STALL_SCRIPT, encoding="utf-8")
        recovery_script = agents_dir / "recovery_agent.py"
        recovery_script.write_text(RECOVERY_SCRIPT, encoding="utf-8")
        heartbeat_script = agents_dir / "heartbeat_agent.py"
        heartbeat_script.write_text(HEARTBEAT_SCRIPT, encoding="utf-8")

        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_STALL_SECS": "2",
                "DOMO_JOB_TIMEOUT_SECS": "6",
                "DOMO_MOCK_DELAY_MS": "40",
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

            def create_agent(name, roles, max_concurrency, command=None):
                payload = {
                    "name": name,
                    "kind": "cli" if command else "mock",
                    "roles": roles,
                    "max_concurrency": max_concurrency,
                }
                if command:
                    payload["command"] = command
                return call("POST", "/api/agents", payload)

            def create_project(name, goal):
                return call(
                    "POST",
                    "/api/projects",
                    {"name": name, "goal": goal, "repo_path": str(repo), "base_branch": "main"},
                )["project"]["id"]

            def configure_pool(project_id, assignments):
                """关闭继承来的默认池，只启用在 assignments 中指定的 Agent。"""
                by_role = {}
                for role, agent_id in assignments:
                    by_role.setdefault(role, []).append(agent_id)
                pool = call("GET", f"/api/projects/{project_id}/agents")
                for group in pool["roles"]:
                    for entry in group["entries"]:
                        if entry["enabled"]:
                            call(
                                "PATCH",
                                f"/api/projects/{project_id}/agents/{group['role']}/{entry['agent_id']}",
                                {"enabled": False},
                            )
                    for position, agent_id in enumerate(by_role.get(group["role"], [])):
                        call(
                            "PATCH",
                            f"/api/projects/{project_id}/agents/{group['role']}/{agent_id}",
                            {"enabled": True, "position": position},
                        )

            def wait_project(project_id, predicate, label, timeout=120, observe=None):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    if observe is not None:
                        observe(detail)
                    return detail if predicate(detail) else None

                try:
                    return wait_for(check, label, timeout)
                except AssertionError:
                    last = call("GET", f"/api/projects/{project_id}")
                    raise AssertionError(
                        f"等待 {label} 超时；最后状态：{last['project'] if last else None} "
                        f"{[(t['key'], t['stage'], t['status'], t['last_error']) for t in (last or {}).get('tasks', [])]}"
                    ) from None

            def task_by_key(detail, key):
                return next(task for task in detail["tasks"] if task["key"] == key)

            def jobs_of(detail, task_id=None, role=None):
                jobs = detail["jobs"]
                if task_id is not None:
                    jobs = [job for job in jobs if job["task_id"] == task_id]
                if role is not None:
                    jobs = [job for job in jobs if job["role"] == role]
                return jobs

            def started_job(job):
                return datetime.fromisoformat(job["started_at"].replace("Z", "+00:00"))

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})

            planner = create_agent("Stall Planner", ["planner"], 2)
            verifier = create_agent("Stall Verifier", ["verifier"], 2)
            stall_agent = create_agent(
                "Stall Executor",
                ["executor"],
                2,
                [sys.executable, str(stall_script), "{job_dir}", "{cwd}", str(temp)],
            )
            recovery_agent = create_agent(
                "Recovery Executor",
                ["executor"],
                2,
                [sys.executable, str(recovery_script), "{job_dir}", "{cwd}", str(temp)],
            )
            timeout_agent = create_agent(
                "Heartbeat Executor",
                ["executor"],
                1,
                [sys.executable, str(heartbeat_script), "{job_dir}", "{cwd}", str(temp)],
            )
            stall_agent_2 = create_agent(
                "Stall Executor 2",
                ["executor"],
                2,
                [sys.executable, str(stall_script), "{job_dir}", "{cwd}", str(temp)],
            )

            # ------------------------------------------------- 卡死隔离与槽位释放
            isolation_project = create_project("Isolation", "[parallel] 卡死检测与同 Agent 隔离")
            configure_pool(
                isolation_project,
                [
                    ("planner", planner["id"]),
                    ("executor", stall_agent["id"]),
                    ("verifier", verifier["id"]),
                ],
            )
            call("POST", f"/api/projects/{isolation_project}/confirm", {})

            observation = {"max_running": 0, "pairs": set()}

            def observe_isolation(detail):
                running = [
                    job
                    for job in detail["jobs"]
                    if job["status"] == "running" and job["agent_id"] == stall_agent["id"]
                ]
                observation["max_running"] = max(observation["max_running"], len(running))
                keys = {task["id"]: task["key"] for task in detail["tasks"]}
                observation["pairs"].add(
                    tuple(sorted(keys.get(job["task_id"], "?") for job in running))
                )

            def isolation_done(item):
                tasks = {task["key"]: task for task in item["tasks"]}
                return (
                    tasks.get("T1", {}).get("status") == "blocked"
                    and tasks.get("T2", {}).get("stage") == "completed"
                    and tasks.get("T3", {}).get("stage") == "completed"
                )

            detail = wait_project(
                isolation_project,
                isolation_done,
                "卡死任务阻塞且同 Agent 任务完成",
                observe=observe_isolation,
            )

            assert observation["max_running"] == 2, (
                f"同一 Agent 并发应恰好达到 2 且不超过上限，实际 {observation['max_running']}"
            )
            assert ("T1", "T2") in observation["pairs"], f"未观察到 T1/T2 并行：{observation['pairs']}"

            t1 = task_by_key(detail, "T1")
            t2 = task_by_key(detail, "T2")
            t3 = task_by_key(detail, "T3")
            assert t1["attempts"] >= 3, t1
            assert t1["status"] == "blocked", t1
            assert t1["last_failure_kind"] == "transient", t1
            assert "卡死" in (t1["last_error"] or ""), t1

            stall_exec_jobs = sorted(
                [
                    job
                    for job in jobs_of(detail, role="executor")
                    if job["agent_id"] == stall_agent["id"]
                ],
                key=lambda job: job["started_at"],
            )
            failed_jobs = [job for job in stall_exec_jobs if job["status"] == "failed"]
            assert len(failed_jobs) == 3, f"T1 应按重试预算卡死 3 次：{failed_jobs}"
            for stalled_job in failed_jobs:
                assert stalled_job["task_id"] == t1["id"], f"只有 T1 应失败：{stalled_job}"
                assert "卡死" in (stalled_job["error"] or ""), stalled_job
                assert stalled_job["failure_kind"] == "transient", stalled_job
                # Windows 的 taskkill 会返回退出码，Unix 上由信号终止时为空。
                if os.name != "nt":
                    assert stalled_job["exit_code"] is None, f"卡死进程应被信号终止：{stalled_job}"

            t1_jobs = jobs_of(detail, task_id=t1["id"], role="executor")
            t1_jobs.sort(key=lambda job: job["started_at"])
            retry_job = t1_jobs[1]
            t2_job = jobs_of(detail, task_id=t2["id"], role="executor")[0]
            assert started_job(retry_job) < datetime.fromisoformat(
                t2_job["finished_at"]
            ), "卡死作业失败后，槽位没有立即释放"
            # 只使用单一 Agent 的池子没有下一优先级可用，第三次卡死后进入人工裁决。
            assert any(
                event["kind"] == "job_stalled" and "卡死" in event["message"]
                for event in detail["events"]
            ), "缺少 job_stalled 审计事件"
            assert any(
                event["kind"] == "task_blocked" and "卡死" in event["message"]
                for event in detail["events"]
            ), "卡死导致的阻塞缺少审计事件"

            # T2/T3 与 T1 同在唯一 Agent 上，不能被误杀。
            for key, task in (("T2", t2), ("T3", t3)):
                jobs = jobs_of(detail, task_id=task["id"], role="executor")
                assert jobs and all(
                    job["agent_id"] == stall_agent["id"] and job["status"] == "succeeded"
                    for job in jobs
                ), f"{key} 应在同一 Agent 上正常完成：{jobs}"

            agents = {agent["id"]: agent for agent in call("GET", "/api/agents")}
            assert agents[stall_agent["id"]]["transient_failures"] >= 3, agents[stall_agent["id"]]

            t1_pid_path = os.path.join(str(temp), "t1.pid")
            with open(t1_pid_path, encoding="utf-8") as handle:
                t1_pid = int(handle.read().strip())
            assert not process_alive(t1_pid), f"卡死进程 {t1_pid} 未被清理"

            call("POST", f"/api/projects/{isolation_project}/archive", {})
            print("   隔离场景通过：T1 卡死阻塞，槽位立即释放，T2/T3 未受影响")

            # ------------------------------------------------- 换下一优先级 Agent
            switch_project = create_project("Switch", "[single] 卡死后换下一优先级 Agent")
            configure_pool(
                switch_project,
                [
                    ("planner", planner["id"]),
                    ("executor", stall_agent_2["id"]),
                    ("executor", recovery_agent["id"]),
                    ("verifier", verifier["id"]),
                ],
            )
            call("POST", f"/api/projects/{switch_project}/confirm", {})

            def switch_done(detail):
                tasks = {item["key"]: item for item in detail["tasks"]}
                return (
                    tasks.get("T1", {}).get("stage") == "completed"
                    and detail["project"]["stage"] == "summarizing"
                    and bool(detail["project"]["report"])
                )

            detail = wait_project(switch_project, switch_done, "换 Agent 后完成")
            task = task_by_key(detail, "T1")
            assert task["attempts"] >= 3, task
            switch_jobs = sorted(
                jobs_of(detail, task_id=task["id"], role="executor"),
                key=lambda job: job["started_at"],
            )
            stalled = [job for job in switch_jobs if job["agent_id"] == stall_agent_2["id"]]
            recovered = [
                job
                for job in switch_jobs
                if job["agent_id"] == recovery_agent["id"] and job["status"] == "succeeded"
            ]
            assert len(stalled) == 3, f"应在原 Agent 上退避重试 3 次：{switch_jobs}"
            assert all(job["status"] == "failed" and "卡死" in (job["error"] or "") for job in stalled)
            assert recovered, f"未换用下一优先级 Agent：{switch_jobs}"
            assert any(
                event["kind"] == "task_retry" and "卡死" in event["message"]
                for event in detail["events"]
            ), "缺少携带卡死原因的 task_retry 事件"
            call("POST", f"/api/projects/{switch_project}/report", {"action": "accept"})
            print("   退避场景通过：连续卡死后换用下一优先级 Agent 完成，事件可审计")

            # ------------------------------------------------- 超时共存
            timeout_project = create_project("Timeout", "[single] 超时与卡死共存")
            configure_pool(
                timeout_project,
                [
                    ("planner", planner["id"]),
                    ("executor", timeout_agent["id"]),
                    ("verifier", verifier["id"]),
                ],
            )
            call("POST", f"/api/projects/{timeout_project}/confirm", {})

            def timed_out(detail):
                return any(
                    job["status"] == "failed" and "超时" in (job["error"] or "")
                    for job in jobs_of(detail, role="executor")
                )

            detail = wait_project(timeout_project, timed_out, "超时失败", timeout=60)
            timeout_jobs = [
                job
                for job in jobs_of(detail, role="executor")
                if job["status"] == "failed" and "超时" in (job["error"] or "")
            ]
            assert timeout_jobs[0]["failure_kind"] == "transient", timeout_jobs[0]
            assert not any(
                event["kind"] == "job_stalled" for event in detail["events"]
            ), "持续输出的作业不应被判定卡死"
            call("POST", f"/api/projects/{timeout_project}/archive", {})
            print("   超时共存通过：持续输出的作业按超时失败，未被卡死检测误杀")

            print("全部卡死检测断言通过")


if __name__ == "__main__":
    main()
