#!/usr/bin/env python3
"""DOMO 项目池启停对任务池的屏蔽测试。

覆盖范围：
  1. 停用项目池中某个 Agent 的角色条目只影响当前项目；
  2. 项目池停用不影响全局 Agent 或其他项目的项目池；
  3. 已有任务池的本地启停不被项目池改写，任务池展示项目层屏蔽；
  4. 项目池停用时已有任务不派发，重新启用后按任务池原有配置继续执行。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
"""

import time

from harness import Server, ensure_server_built, init_repo, temp_root, wait_for


def main():
    with temp_root("domo-project-agent-switch-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "50",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "120",
            },
        ) as server:

            def call(method, path, data=None):
                return server.call(method, path, data)

            def wait_project(project_id, predicate, label, timeout=90):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            def create_project(name):
                return call(
                    "POST",
                    "/api/projects",
                    {
                        "name": name,
                        "repo_path": str(repo),
                        "base_branch": "main",
                    },
                )["project"]["id"]

            def project_pool(project_id):
                return call("GET", f"/api/projects/{project_id}/agents")["roles"]

            def role_entries(roles, role):
                return next(group for group in roles if group["role"] == role)["entries"]

            # 只保留一个 mock Agent，避免默认团队中的其他条目干扰断言。
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            agent = call(
                "POST",
                "/api/agents",
                {
                    "name": "项目池开关 Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )

            project_id = create_project("SwitchProject")
            other_id = create_project("UntouchedProject")

            # 手动确认模式让项目停在方案阶段，便于在派发前切换项目池启停。
            call(
                "POST",
                f"/api/projects/{project_id}/analysis/messages",
                {"message": "[single] 项目池开关测试", "confirm_mode": "manual"},
            )
            wait_project(
                project_id,
                lambda detail: detail.get("proposal") is not None,
                "项目方案",
            )

            pool = project_pool(project_id)
            for role in ("planner", "executor", "verifier"):
                entries = [
                    entry for entry in role_entries(pool, role) if entry["agent_id"] == agent["id"]
                ]
                assert entries and entries[0]["enabled"] is True, entries

            switched = None
            for role in ("planner", "executor", "verifier"):
                switched = call(
                    "PATCH",
                    f"/api/projects/{project_id}/agents/{role}/{agent['id']}",
                    {"enabled": False},
                )
            switched_roles = switched["roles"]
            for role in ("planner", "executor", "verifier"):
                entries = [
                    entry
                    for entry in role_entries(switched_roles, role)
                    if entry["agent_id"] == agent["id"]
                ]
                assert entries and entries[0]["enabled"] is False, entries

            # 项目池启停不修改全局 Agent，也不影响其他项目。
            global_agent = next(
                item for item in call("GET", "/api/agents") if item["id"] == agent["id"]
            )
            assert global_agent["enabled"] is True, global_agent
            other_pool = project_pool(other_id)
            for role in ("planner", "executor", "verifier"):
                entries = [
                    entry
                    for entry in role_entries(other_pool, role)
                    if entry["agent_id"] == agent["id"]
                ]
                assert entries and entries[0]["enabled"] is True, entries

            # 确认方案后任务从已停用的项目池初始化，任务池本地状态为停用。
            call("POST", f"/api/projects/{project_id}/analysis/confirm", {})
            detail = wait_project(
                project_id,
                lambda value: len(value.get("tasks", [])) > 0,
                "任务创建",
            )
            task = detail["tasks"][0]
            task_pool = call(
                "GET", f"/api/projects/{project_id}/tasks/{task['id']}/agents"
            )
            executor = next(
                entry
                for entry in role_entries(task_pool["roles"], "executor")
                if entry["agent_id"] == agent["id"]
            )
            assert executor["enabled"] is False, executor
            assert executor["project_enabled"] is False, executor

            # 任务池本地重新启用，项目层仍然屏蔽；任务保持等待且原因明确。
            for role in ("executor", "verifier"):
                call(
                    "PATCH",
                    f"/api/projects/{project_id}/tasks/{task['id']}/agents/{role}/{agent['id']}",
                    {"enabled": True},
                )
            task_pool = call(
                "GET", f"/api/projects/{project_id}/tasks/{task['id']}/agents"
            )
            executor = next(
                entry
                for entry in role_entries(task_pool["roles"], "executor")
                if entry["agent_id"] == agent["id"]
            )
            assert executor["enabled"] is True, executor
            assert executor["project_enabled"] is False, executor

            blocked = wait_project(
                project_id,
                lambda value: any(
                    item["id"] == task["id"]
                    and "项目 Agent 池已停用" in (item.get("waiting_reason") or "")
                    for item in value.get("tasks", [])
                ),
                "项目层停用等待原因",
                timeout=30,
            )
            blocked_task = next(item for item in blocked["tasks"] if item["id"] == task["id"])
            assert blocked_task["stage"] != "completed", blocked_task

            # 重新启用项目池对应角色后，任务池本地启停状态保留，任务继续执行。
            for role in ("planner", "executor", "verifier"):
                call(
                    "PATCH",
                    f"/api/projects/{project_id}/agents/{role}/{agent['id']}",
                    {"enabled": True},
                )
            task_pool = call(
                "GET", f"/api/projects/{project_id}/tasks/{task['id']}/agents"
            )
            executor = next(
                entry
                for entry in role_entries(task_pool["roles"], "executor")
                if entry["agent_id"] == agent["id"]
            )
            assert executor["enabled"] is True, executor
            assert executor["project_enabled"] is True, executor

            completed = wait_project(
                project_id,
                lambda value: all(
                    item["stage"] == "completed" for item in value.get("tasks", [])
                ),
                "项目池恢复后任务完成",
                timeout=60,
            )
            completed_task = next(
                item for item in completed["tasks"] if item["id"] == task["id"]
            )
            assert completed_task["stage"] == "completed", completed_task


if __name__ == "__main__":
    main()
