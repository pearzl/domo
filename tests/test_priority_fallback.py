#!/usr/bin/env python3
"""Agent 并发满额让位调度集成测试。

覆盖范围：
  1. 开关默认关闭时，高优先级 Agent 并发已满保持等待，后续 Agent 不接手；
  2. 开启让位后，高优先级 Agent 满额且后续 Agent 空闲时，[parallel] 项目的
     执行作业并行落在两个 Agent，派发事件说明让位来源；
  3. 两个 Agent 均开启让位且并发已满时，第三个任务派发给后续 Agent；
  4. 前序 Agent 开启让位并满额、后续 Agent 关闭让位并满额时，任务等待该
     后续 Agent，更后续的 Agent 不接手，等待原因包含名称与并发占用；
  5. Agent API 的默认值、PATCH 往返与默认团队、项目池条目字段一致。

使用内置 mock Agent 与脚本化 CLI Agent，不访问外网；临时数据全部放在系统
临时目录。退出码 0 表示全部通过。
"""

import sys
import time

from harness import (
    Server,
    call as api,
    ensure_server_built,
    init_repo,
    temp_root,
    wait_for,
)


# 执行者轮询释放标记，占住并发槽位直到测试放行，不依赖固定等待时长。
HOLD_SCRIPT = '''\
import json
import os
import sys
import time

job_dir, cwd, root, token = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
with open(os.path.join(job_dir, "context.json"), encoding="utf-8") as handle:
    context = json.load(handle)
key = (context.get("task") or {}).get("key") or ""
if os.environ.get("DOMO_JOB_ROLE", "") != "executor":
    with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
        json.dump(
            {"kind": "failed", "reason": "占位 Agent 只承担执行角色"},
            handle,
            ensure_ascii=False,
        )
    sys.exit(0)
release = os.path.join(root, "release-" + token)
deadline = time.time() + 180
while not os.path.exists(release):
    if time.time() >= deadline:
        with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
            json.dump(
                {"kind": "failed", "reason": "占位 Agent 等待释放超时"},
                handle,
                ensure_ascii=False,
            )
        sys.exit(0)
    time.sleep(0.05)
os.makedirs(os.path.join(cwd, "domo_mock"), exist_ok=True)
with open(os.path.join(cwd, "domo_mock", key + ".txt"), "w", encoding="utf-8") as handle:
    handle.write(key + "\\n")
with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
    json.dump(
        {"kind": "done", "summary": "占位 Agent 完成 " + key},
        handle,
        ensure_ascii=False,
    )
'''


def flag_of(entry):
    value = entry.get("allow_priority_fallback")
    assert isinstance(value, bool), f"allow_priority_fallback 不是布尔值: {entry!r}"
    return value


def main():
    with temp_root("domo-fallback-schedule-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        hold_script = temp / "hold_agent.py"
        hold_script.write_text(HOLD_SCRIPT, encoding="utf-8")

        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "30",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "30",
            },
        ) as server:
            pending_goals = {}
            project_ids = []

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

            def wait_project(project_id, predicate, label, timeout=120):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                try:
                    return wait_for(check, label, timeout)
                except AssertionError:
                    last = call("GET", f"/api/projects/{project_id}")
                    raise AssertionError(
                        f"等待 {label} 超时；最后状态：{last['project']} "
                        f"{[(t['key'], t['stage'], t['status'], t['waiting_reason']) for t in last['tasks']]}"
                    ) from None

            def create_project(name, goal):
                project_id = call(
                    "POST",
                    "/api/projects",
                    {"name": name, "goal": goal, "repo_path": str(repo), "base_branch": "main"},
                )["project"]["id"]
                project_ids.append(project_id)
                return project_id

            def confirm(project_id):
                call("POST", f"/api/projects/{project_id}/confirm", {})

            def task_by_key(detail, key):
                return next(task for task in detail["tasks"] if task["key"] == key)

            def executor_jobs(detail, agent_id=None, task_id=None):
                jobs = [job for job in detail["jobs"] if job["role"] == "executor"]
                if agent_id is not None:
                    jobs = [job for job in jobs if job["agent_id"] == agent_id]
                if task_id is not None:
                    jobs = [job for job in jobs if job["task_id"] == task_id]
                return jobs

            def agent_running(detail, agent_id):
                return any(
                    job["role"] == "executor"
                    and job["agent_id"] == agent_id
                    and job["status"] == "running"
                    for job in detail["jobs"]
                )

            def pool_entry(pool, role, agent_id):
                group = next(item for item in pool["roles"] if item["role"] == role)
                return next(entry for entry in group["entries"] if entry["agent_id"] == agent_id)

            def restrict_role(project_id, role, keep_ids):
                pool = call("GET", f"/api/projects/{project_id}/agents")
                group = next(item for item in pool["roles"] if item["role"] == role)
                for entry in group["entries"]:
                    if entry["agent_id"] not in keep_ids and entry["enabled"]:
                        call(
                            "PATCH",
                            f"/api/projects/{project_id}/agents/{role}/{entry['agent_id']}",
                            {"enabled": False},
                        )

            def release(token):
                (temp / f"release-{token}").write_text("1", encoding="utf-8")

            def waiting_task(detail, agent_name):
                return next(
                    (
                        task
                        for task in detail["tasks"]
                        if task["status"] == "queued"
                        and agent_name in (task["waiting_reason"] or "")
                        and "并发 1/1" in (task["waiting_reason"] or "")
                    ),
                    None,
                )

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})

            def create_hold_agent(name, token, allow=None):
                payload = {
                    "name": name,
                    "kind": "cli",
                    "roles": ["executor"],
                    "max_concurrency": 1,
                    "command": [
                        sys.executable,
                        str(hold_script),
                        "{job_dir}",
                        "{cwd}",
                        str(temp),
                        token,
                    ],
                }
                if allow is not None:
                    payload["allow_priority_fallback"] = allow
                return call("POST", "/api/agents", payload)

            a1 = create_hold_agent("Fallback A1", "A1")
            b1 = create_hold_agent("Fallback B1", "B1", allow=False)
            a2 = create_hold_agent("Fallback A2", "A2", allow=True)
            b2 = create_hold_agent("Fallback B2", "B2", allow=False)
            a3 = create_hold_agent("Fallback A3", "A3", allow=True)
            b3 = create_hold_agent("Fallback B3", "B3", allow=True)
            a4 = create_hold_agent("Fallback A4", "A4", allow=True)
            b4 = create_hold_agent("Fallback B4", "B4", allow=False)
            base = call(
                "POST",
                "/api/agents",
                {
                    "name": "Fallback Mock Core",
                    "kind": "mock",
                    "roles": ["planner", "verifier", "executor"],
                    "max_concurrency": 12,
                },
            )

            hold_agents = (a1, b1, a2, b2, a3, b3, a4, b4)
            hold_ids = {agent["id"] for agent in hold_agents}

            def cleanup_hold_agents():
                # 停止继续派发后放行占位作业，并等待执行进程退出，避免服务
                # 停止后残留脚本进程。
                for agent_id in hold_ids:
                    try:
                        call("PATCH", f"/api/agents/{agent_id}", {"enabled": False})
                    except AssertionError:
                        pass
                for token in ("A1", "B1", "A2", "B2", "A3", "B3", "A4", "B4"):
                    release(token)

                def idle():
                    for project_id in project_ids:
                        try:
                            detail = call("GET", f"/api/projects/{project_id}")
                        except AssertionError:
                            return True
                        if any(
                            job["agent_id"] in hold_ids and job["status"] == "running"
                            for job in detail["jobs"]
                        ):
                            return False
                    return True

                try:
                    wait_for(idle, "占位 Agent 作业退出", 30)
                except AssertionError:
                    pass

            try:
                # 5. API 往返：默认 false，PATCH true/false 回读，团队与项目池一致。
                listed = call("GET", "/api/agents")
                assert all(
                    isinstance(agent.get("allow_priority_fallback"), bool)
                    for agent in listed
                ), listed
                base_listed = next(agent for agent in listed if agent["id"] == base["id"])
                assert flag_of(base_listed) is False, base_listed

                enabled = call(
                    "PATCH", f"/api/agents/{base['id']}", {"allow_priority_fallback": True}
                )
                assert flag_of(enabled) is True, enabled
                fetched = next(
                    agent
                    for agent in call("GET", "/api/agents")
                    if agent["id"] == base["id"]
                )
                assert flag_of(fetched) is True, fetched

                api_project = create_project("Fallback API", "开关往返")
                team_entry = pool_entry(
                    call("GET", "/api/team/default"), "executor", base["id"]
                )
                project_entry = pool_entry(
                    call("GET", f"/api/projects/{api_project}/agents"),
                    "executor",
                    base["id"],
                )
                assert flag_of(team_entry) is True, team_entry
                assert flag_of(project_entry) is True, project_entry
                assert (
                    team_entry["allow_priority_fallback"]
                    == project_entry["allow_priority_fallback"]
                )

                disabled = call(
                    "PATCH", f"/api/agents/{base['id']}", {"allow_priority_fallback": False}
                )
                assert flag_of(disabled) is False, disabled
                fetched = next(
                    agent
                    for agent in call("GET", "/api/agents")
                    if agent["id"] == base["id"]
                )
                assert flag_of(fetched) is False, fetched
                team_entry = pool_entry(
                    call("GET", "/api/team/default"), "executor", base["id"]
                )
                project_entry = pool_entry(
                    call("GET", f"/api/projects/{api_project}/agents"),
                    "executor",
                    base["id"],
                )
                assert flag_of(team_entry) is False, team_entry
                assert flag_of(project_entry) is False, project_entry
                assert (
                    team_entry["allow_priority_fallback"]
                    == project_entry["allow_priority_fallback"]
                )

                # 1. 默认关闭：高优先级满额时等待，后续 Agent 不接手。
                a1_listed = next(
                    agent
                    for agent in call("GET", "/api/agents")
                    if agent["id"] == a1["id"]
                )
                assert flag_of(a1_listed) is False, a1_listed
                strict_id = create_project(
                    "Fallback Strict", "[parallel] 默认关闭时严格等待"
                )
                restrict_role(strict_id, "executor", {a1["id"], b1["id"]})
                confirm(strict_id)
                wait_project(
                    strict_id,
                    lambda detail: agent_running(detail, a1["id"]),
                    "A1 占住并发",
                    60,
                )
                strict = wait_project(
                    strict_id,
                    lambda detail: waiting_task(detail, "Fallback A1") is not None,
                    "默认关闭时等待 A1",
                    60,
                )
                waited = waiting_task(strict, "Fallback A1")
                assert waited["status"] == "queued", waited
                assert not executor_jobs(
                    strict, agent_id=b1["id"]
                ), "默认关闭时后续 Agent 不应接手"
                assert not any(
                    event["kind"] == "job_queued" and "让位" in event["message"]
                    for event in strict["events"]
                ), strict["events"]
                assert "Fallback A1" in (
                    strict["project"]["blocked_reason"] or ""
                ), strict["project"]
                release("A1")

                # 2. A2 满额、B2 空闲：A2 让位，两个执行作业并行落在 A2/B2。
                parallel_id = create_project(
                    "Fallback Parallel", "[parallel] 让位后并行派发"
                )
                restrict_role(parallel_id, "executor", {a2["id"], b2["id"]})
                confirm(parallel_id)
                spread = wait_project(
                    parallel_id,
                    lambda detail: agent_running(detail, a2["id"])
                    and agent_running(detail, b2["id"]),
                    "A2/B2 并行执行",
                    60,
                )
                t2 = task_by_key(spread, "T2")
                t2_jobs = executor_jobs(spread, task_id=t2["id"])
                assert len(t2_jobs) == 1, t2_jobs
                assert t2_jobs[0]["agent_id"] == b2["id"], t2_jobs
                t2_events = [
                    event
                    for event in spread["events"]
                    if event["kind"] == "job_queued" and event["task_id"] == t2["id"]
                ]
                assert t2_events, spread["events"]
                assert any(
                    "Fallback A2" in event["message"]
                    and "并发已满" in event["message"]
                    and "让位" in event["message"]
                    for event in t2_events
                ), t2_events
                queued = [
                    task for task in spread["tasks"] if task["status"] == "queued"
                ]
                assert queued, spread["tasks"]
                assert any(
                    "Fallback B2" in (task["waiting_reason"] or "")
                    and "并发 1/1" in (task["waiting_reason"] or "")
                    for task in queued
                ), queued
                release("A2")
                release("B2")

                # 3. A3、B3 均开启让位且均满额：第三个任务派发给 Mock Core。
                third_id = create_project(
                    "Fallback Third", "[parallel] 双让位后第三个 Agent 接手"
                )
                restrict_role(third_id, "executor", {a3["id"], b3["id"], base["id"]})
                confirm(third_id)
                third = wait_project(
                    third_id,
                    lambda detail: len(detail["tasks"]) >= 3
                    and agent_running(detail, a3["id"])
                    and agent_running(detail, b3["id"])
                    and any(
                        job["role"] == "executor"
                        and job["task_id"] == task_by_key(detail, "T3")["id"]
                        and job["agent_id"] == base["id"]
                        for job in detail["jobs"]
                    ),
                    "第三个任务派发给 Mock Core",
                    90,
                )
                t3 = task_by_key(third, "T3")
                t3_jobs = executor_jobs(third, task_id=t3["id"])
                assert any(job["agent_id"] == base["id"] for job in t3_jobs), t3_jobs
                t3_events = [
                    event
                    for event in third["events"]
                    if event["kind"] == "job_queued" and event["task_id"] == t3["id"]
                ]
                assert t3_events, third["events"]
                assert any(
                    "Fallback A3" in event["message"]
                    and "Fallback B3" in event["message"]
                    and "让位" in event["message"]
                    for event in t3_events
                ), t3_events
                release("A3")
                release("B3")

                # 4. A4 开启让位且满额、B4 关闭让位且满额：等待 B4，Mock Core 不接手。
                blocker_id = create_project(
                    "Fallback Blocker", "[single] 占住后续 Agent"
                )
                restrict_role(blocker_id, "executor", {b4["id"]})
                confirm(blocker_id)
                wait_project(
                    blocker_id,
                    lambda detail: agent_running(detail, b4["id"]),
                    "B4 被占住",
                    60,
                )

                blocked_id = create_project(
                    "Fallback Blocked", "[parallel] 关闭让位的后续 Agent 阻塞派发"
                )
                restrict_role(
                    blocked_id, "executor", {a4["id"], b4["id"], base["id"]}
                )
                confirm(blocked_id)
                blocked = wait_project(
                    blocked_id,
                    lambda detail: agent_running(detail, a4["id"])
                    and waiting_task(detail, "Fallback B4") is not None,
                    "等待关闭让位的 B4",
                    60,
                )
                waiting = waiting_task(blocked, "Fallback B4")
                assert waiting["status"] == "queued", waiting
                assert "Fallback B4" in waiting["waiting_reason"], waiting
                assert "并发 1/1" in waiting["waiting_reason"], waiting
                assert not executor_jobs(
                    blocked, agent_id=base["id"]
                ), "更后续 Agent 不应接手"
            finally:
                cleanup_hold_agents()

        print("并发满额让位调度断言全部通过")


if __name__ == "__main__":
    main()
