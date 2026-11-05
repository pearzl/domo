#!/usr/bin/env python3
"""DOMO 新建 Agent 同步现有 Agent 池测试。

覆盖范围：
  1. 创建 Agent 时勾选同步，会按角色追加到未归档项目的项目池，以及其未结束
     任务的执行者/验收者池末尾；原有条目、顺序和优先级保持不变。
  2. 未勾选时现有项目池与任务池不受影响。
  3. 只声明规划者角色的 Agent 不进入任务池。
  4. 创建时禁用 Agent，同步后的池条目保持禁用。
  5. 已归档项目不参与同步。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
"""

import sqlite3

from harness import Server, ensure_server_built, init_repo, temp_root


def main():
    with temp_root("domo-agent-pool-sync-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(temp, env={"DOMO_MOCK_DELAY_MS": "20"}) as server:
            def call(method, path, data=None):
                return server.call(method, path, data)

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

            def add_task(project_id, key):
                return call(
                    "POST",
                    f"/api/projects/{project_id}/tasks",
                    {
                        "key": key,
                        "title": f"{key} 标题",
                        "description": f"{key} 描述",
                        "acceptance": ["产物存在"],
                    },
                )

            def create_agent(name, roles, **extra):
                payload = {
                    "name": name,
                    "kind": "mock",
                    "roles": roles,
                    "max_concurrency": 1,
                }
                payload.update(extra)
                return call("POST", "/api/agents", payload)

            def project_pool(project_id):
                pool = call("GET", f"/api/projects/{project_id}/agents")
                return {group["role"]: group["entries"] for group in pool["roles"]}

            def task_pool(project_id, task_id):
                pool = call(
                    "GET", f"/api/projects/{project_id}/tasks/{task_id}/agents"
                )
                return {group["role"]: group["entries"] for group in pool["roles"]}

            def ids(entries):
                return [entry["agent_id"] for entry in entries]

            def priorities(entries):
                return [entry["priority"] for entry in entries]

            def enabled(entries):
                return [entry["enabled"] for entry in entries]

            active_project = create_project("SyncActive")
            task_a = add_task(active_project, "T1")
            task_b = add_task(active_project, "T2")
            archived_project = create_project("SyncArchived")
            call("POST", f"/api/projects/{archived_project}/archive?force=true")

            project_before = project_pool(active_project)
            task_before = {
                task_a["id"]: task_pool(active_project, task_a["id"]),
                task_b["id"]: task_pool(active_project, task_b["id"]),
            }
            archived_before = project_pool(archived_project)

            sync_agent = create_agent(
                "Sync All",
                ["planner", "executor", "verifier"],
                add_to_existing_pools=True,
            )
            assert sync_agent["roles"] == ["planner", "executor", "verifier"], sync_agent

            project_after = project_pool(active_project)
            for role in ("planner", "executor", "verifier"):
                before = project_before[role]
                after = project_after[role]
                assert ids(after) == ids(before) + [sync_agent["id"]], (role, after)
                assert priorities(after) == list(range(len(after))), (role, after)
                assert enabled(after) == enabled(before) + [True], (role, after)

            for task in (task_a, task_b):
                pool_before = task_before[task["id"]]
                pool_after = task_pool(active_project, task["id"])
                for role in ("executor", "verifier"):
                    before = pool_before[role]
                    after = pool_after[role]
                    assert ids(after) == ids(before) + [sync_agent["id"]], (
                        task["key"],
                        role,
                        after,
                    )
                    assert priorities(after) == list(range(len(after))), (
                        task["key"],
                        role,
                        after,
                    )
                assert "planner" not in pool_after, pool_after

            assert project_pool(archived_project) == archived_before, "归档项目不应同步"

            # 未勾选时保持现有池不变。
            plain_agent = create_agent(
                "No Sync",
                ["planner", "executor", "verifier"],
            )
            assert sync_agent["id"] in ids(project_pool(active_project)["planner"])
            assert plain_agent["id"] not in ids(project_pool(active_project)["planner"])
            assert plain_agent["id"] not in ids(
                task_pool(active_project, task_a["id"])["executor"]
            )

            # 只声明规划者时只更新项目池，任务池不增加条目。
            task_pool_before_planner = task_pool(active_project, task_a["id"])
            planner_agent = create_agent(
                "Planner Only",
                ["planner"],
                add_to_existing_pools=True,
            )
            planner_pool = project_pool(active_project)["planner"]
            assert ids(planner_pool)[-1] == planner_agent["id"], planner_pool
            task_pool_after_planner = task_pool(active_project, task_a["id"])
            assert {
                role: ids(entries)
                for role, entries in task_pool_after_planner.items()
            } == {
                role: ids(entries)
                for role, entries in task_pool_before_planner.items()
            }, task_pool_after_planner

            # 禁用 Agent 同步后，项目池与任务池条目都保持禁用。
            disabled_agent = create_agent(
                "Disabled Sync",
                ["executor"],
                enabled=False,
                add_to_existing_pools=True,
            )
            disabled_project_entry = next(
                entry
                for entry in project_pool(active_project)["executor"]
                if entry["agent_id"] == disabled_agent["id"]
            )
            assert disabled_project_entry["enabled"] is False, disabled_project_entry
            assert disabled_project_entry["agent_enabled"] is False, disabled_project_entry
            disabled_task_entry = next(
                entry
                for entry in task_pool(active_project, task_a["id"])["executor"]
                if entry["agent_id"] == disabled_agent["id"]
            )
            assert disabled_task_entry["enabled"] is False, disabled_task_entry

            with sqlite3.connect(temp / "data" / "domo.db") as db:
                db.execute("DELETE FROM task_agents WHERE task_id = ?", (task_b["id"],))
            assert all(not entries for entries in task_pool(active_project, task_b["id"]).values())
            empty_pool_agent = create_agent(
                "Empty Pool Sync",
                ["executor", "verifier"],
                add_to_existing_pools=True,
            )
            for role, entries in task_pool(active_project, task_b["id"]).items():
                assert ids(entries) == [empty_pool_agent["id"]], (role, entries)
                assert priorities(entries) == [0], (role, entries)

            print("新建 Agent 同步现有项目/任务 Agent 池通过")


if __name__ == "__main__":
    main()
