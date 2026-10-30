#!/usr/bin/env python3
"""DOMO 项目级 Agent 池与任务级 Agent 选择测试。

覆盖范围：
  1. 项目创建时从默认团队快照初始化，并包含禁用条目；
  2. 规划、执行、验收分别按角色从项目池中启用且优先级最高的 Agent 选人；
  3. 调整项目池顺序后，新作业按新顺序选人；
  4. 禁用项目池中最高优先级的 Agent 后自动回退到下一优先级；
  5. 任务级指定执行/验收 Agent 优先于项目池，清除后恢复项目池规则；
  6. 项目池配置在服务重启后保持；
  7. 全局 Agent 删除后项目池条目标记为不可用，并给出可读的阻塞原因；
  8. 项目池角色校验拒绝未知角色。
  9. PATCH position 把条目移动到指定位置（首位、中间、末位），角色内优先级
     连续重排为 0..n-1，其他角色不变；越界或角色下不存在该 Agent 返回 400。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
"""

import json
import time
import urllib.error
import urllib.request

from harness import Server, ensure_server_built, init_repo, temp_root, wait_for


def main():
    with temp_root("domo-agent-pool-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "80",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "800",
            },
        ) as server:
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
                        last = detail["messages"][-1] if detail["messages"] else None
                        if last and last["role"] == "planner" and not detail.get("proposal"):
                            return detail
                        time.sleep(0.1)
                    return server.call("GET", f"/api/projects/{project_id}")
                return server.call(method, path, data)

            def expect_error(method, path, data=None):
                body = json.dumps(data).encode() if data is not None else None
                request = urllib.request.Request(
                    server.base + path,
                    data=body,
                    method=method,
                    headers={"Content-Type": "application/json"},
                )
                try:
                    with urllib.request.urlopen(request, timeout=60) as response:
                        raise AssertionError(
                            f"{method} {path} 应被拒绝，实际返回 {response.status}"
                        )
                except urllib.error.HTTPError as error:
                    return error.code

            def wait_project(project_id, predicate, label, timeout=120):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

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

            def confirm(project_id):
                call("POST", f"/api/projects/{project_id}/confirm", {})

            def task_by_key(detail, key):
                return next(task for task in detail["tasks"] if task["key"] == key)

            def pool_role(project_id, role):
                pool = call("GET", f"/api/projects/{project_id}/agents")
                return next(group for group in pool["roles"] if group["role"] == role)

            def role_jobs(detail, role):
                return [job for job in detail["jobs"] if job["role"] == role]

            def wait_role_job(project_id, role, key, timeout=60):
                def found(detail):
                    task = task_by_key(detail, key)
                    return any(
                        job["task_id"] == task["id"] and job["role"] == role
                        for job in detail["jobs"]
                    )

                detail = wait_project(project_id, found, f"{key} 的{role}作业", timeout)
                task = task_by_key(detail, key)
                return next(
                    job
                    for job in detail["jobs"]
                    if job["task_id"] == task["id"] and job["role"] == role
                )

            def wait_completed(project_id, label):
                return wait_project(
                    project_id,
                    lambda detail: detail["project"]["stage"] == "summarizing"
                    and detail["project"]["report"],
                    label,
                )

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})

            high = call(
                "POST",
                "/api/agents",
                {
                    "name": "Pool High",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )
            low = call(
                "POST",
                "/api/agents",
                {
                    "name": "Pool Low",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )
            # 新建项目的项目池继承默认团队。

            # 1. 默认顺序：三个角色都使用各自角色下优先级最高的 Agent。
            basic_id = create_project("PoolBasic", "项目池按角色选择最高优先级 Agent")
            confirm(basic_id)
            basic = wait_completed(basic_id, "基础项目完成")
            goal_jobs = role_jobs(basic, "analysis")
            exec_jobs = role_jobs(basic, "executor")
            verify_jobs = role_jobs(basic, "verifier")
            assert goal_jobs and goal_jobs[0]["agent_id"] == high["id"], goal_jobs
            assert exec_jobs and all(
                job["agent_id"] == high["id"] for job in exec_jobs
            ), exec_jobs
            assert verify_jobs and all(
                job["agent_id"] == high["id"] for job in verify_jobs
            ), verify_jobs

            # 2. 调整执行角色的项目池顺序后，新作业使用新的最高优先级 Agent。
            reordered_id = create_project("PoolReorder", "调整顺序后按新顺序选人")
            planner_before = pool_role(reordered_id, "planner")["entries"]
            executor_before = pool_role(reordered_id, "executor")["entries"]
            assert [
                entry["agent_id"] for entry in executor_before if entry["enabled"]
            ][:2] == [high["id"], low["id"]], executor_before
            assert [
                entry["priority"] for entry in executor_before[:3]
            ] == [0, 1, 2], executor_before
            updated = call(
                "PATCH",
                f"/api/projects/{reordered_id}/agents/executor/{low['id']}",
                {"direction": "up"},
            )
            executor_after = next(
                group for group in updated["roles"] if group["role"] == "executor"
            )["entries"]
            assert [
                entry["agent_id"] for entry in executor_after if entry["enabled"]
            ][:2] == [low["id"], high["id"]], executor_after
            assert [
                entry["priority"] for entry in executor_after[:3]
            ] == [0, 1, 2], executor_after
            planner_after = next(
                group for group in updated["roles"] if group["role"] == "planner"
            )["entries"]
            assert planner_after == planner_before, "调整执行角色不应改变规划角色顺序"

            # 全局配置在项目创建后变化，不应影响已经快照的项目池。
            call("PATCH", f"/api/agents/{high['id']}", {"enabled": False})
            decoupled = pool_role(reordered_id, "planner")["entries"]
            assert next(
                entry for entry in decoupled if entry["agent_id"] == high["id"]
            )["enabled"] is True, decoupled
            # 全局禁用属于调度不可用，恢复后再验证项目池内顺序。
            call("PATCH", f"/api/agents/{high['id']}", {"enabled": True})

            confirm(reordered_id)
            reordered = wait_completed(reordered_id, "调整顺序后项目完成")
            reordered_execs = role_jobs(reordered, "executor")
            assert reordered_execs and all(
                job["agent_id"] == low["id"] for job in reordered_execs
            ), reordered_execs
            assert role_jobs(reordered, "analysis")[0]["agent_id"] == high["id"], reordered[
                "jobs"
            ]
            assert all(
                job["agent_id"] == high["id"]
                for job in role_jobs(reordered, "verifier")
            ), reordered["jobs"]

            # 2b. PATCH position：默认团队与项目池都支持 0 基目标位置，并在事务内重排该角色。
            team_pool = call("GET", "/api/team/default")
            team_executor_before = next(
                group for group in team_pool["roles"] if group["role"] == "executor"
            )["entries"]
            team_executor_ids = [entry["agent_id"] for entry in team_executor_before]
            team_planner_before = next(
                group for group in team_pool["roles"] if group["role"] == "planner"
            )["entries"]
            assert low["id"] in team_executor_ids, team_executor_before
            count = len(team_executor_ids)

            # 末位中的条目移到首位。
            team_moved = call(
                "PATCH",
                f"/api/team/default/executor/{low['id']}",
                {"position": 0},
            )
            team_executor_after = next(
                group for group in team_moved["roles"] if group["role"] == "executor"
            )["entries"]
            team_executor_after_ids = [
                entry["agent_id"] for entry in team_executor_after
            ]
            assert team_executor_after_ids == [low["id"]] + [
                agent_id for agent_id in team_executor_ids if agent_id != low["id"]
            ], team_executor_after
            assert [
                entry["priority"] for entry in team_executor_after
            ] == list(range(count)), team_executor_after
            team_planner_after = next(
                group for group in team_moved["roles"] if group["role"] == "planner"
            )["entries"]
            assert team_planner_after == team_planner_before, team_planner_after
            low_original_index = team_executor_ids.index(low["id"])
            team_restored = call(
                "PATCH",
                f"/api/team/default/executor/{low['id']}",
                {"position": low_original_index},
            )
            team_restored_ids = [
                entry["agent_id"]
                for entry in next(
                    group
                    for group in team_restored["roles"]
                    if group["role"] == "executor"
                )["entries"]
            ]
            assert team_restored_ids == team_executor_ids, team_restored
            assert [
                entry["priority"]
                for entry in next(
                    group
                    for group in team_restored["roles"]
                    if group["role"] == "executor"
                )["entries"]
            ] == list(range(count)), team_restored

            code = expect_error(
                "PATCH",
                f"/api/team/default/executor/{low['id']}",
                {"position": -1},
            )
            assert code == 400, code
            code = expect_error(
                "PATCH",
                f"/api/team/default/executor/{low['id']}",
                {"position": count},
            )
            assert code == 400, code
            code = expect_error(
                "PATCH",
                "/api/team/default/executor/not-in-team-agent",
                {"position": 0},
            )
            assert code == 400, code

            # 项目池：条目移到中间、首位、末位，其他角色顺序保持不变。
            # 由条目移动结果推导目标顺序，不假设池中 Agent 的数量。
            def apply_move(entries, agent_id, position):
                rest = [item for item in entries if item != agent_id]
                return rest[:position] + [agent_id] + rest[position:]

            project_entries = pool_role(reordered_id, "executor")["entries"]
            assert len(project_entries) >= 3, project_entries
            project_ids_before = [entry["agent_id"] for entry in project_entries]
            mover = project_ids_before[2]

            stepped = call(
                "PATCH",
                f"/api/projects/{reordered_id}/agents/executor/{mover}",
                {"position": 1},
            )
            stepped_entries = next(
                group for group in stepped["roles"] if group["role"] == "executor"
            )["entries"]
            assert [
                entry["agent_id"] for entry in stepped_entries
            ] == apply_move(project_ids_before, mover, 1), stepped_entries
            assert [
                entry["priority"] for entry in stepped_entries
            ] == list(range(len(project_entries))), stepped_entries
            assert next(
                group for group in stepped["roles"] if group["role"] == "planner"
            )["entries"] == planner_before, stepped["roles"]

            first = call(
                "PATCH",
                f"/api/projects/{reordered_id}/agents/executor/{mover}",
                {"position": 0},
            )
            first_ids = [
                entry["agent_id"]
                for entry in next(
                    group for group in first["roles"] if group["role"] == "executor"
                )["entries"]
            ]
            assert first_ids == apply_move(
                apply_move(project_ids_before, mover, 1), mover, 0
            ), first["roles"]

            last = call(
                "PATCH",
                f"/api/projects/{reordered_id}/agents/executor/{mover}",
                {"position": 2},
            )
            last_entries = next(
                group for group in last["roles"] if group["role"] == "executor"
            )["entries"]
            executor_after = [entry["agent_id"] for entry in last_entries]
            assert executor_after == apply_move(first_ids, mover, 2), last_entries
            assert [
                entry["priority"] for entry in last_entries
            ] == list(range(len(project_entries))), last_entries
            planner_after = next(
                group for group in last["roles"] if group["role"] == "planner"
            )["entries"]
            assert planner_after == planner_before, planner_after

            code = expect_error(
                "PATCH",
                f"/api/projects/{reordered_id}/agents/executor/{mover}",
                {"position": -1},
            )
            assert code == 400, code
            code = expect_error(
                "PATCH",
                f"/api/projects/{reordered_id}/agents/executor/{mover}",
                {"position": len(project_entries)},
            )
            assert code == 400, code
            code = expect_error(
                "PATCH",
                f"/api/projects/{reordered_id}/agents/executor/not-in-pool-agent",
                {"position": 0},
            )
            assert code == 400, code

            # 3. 禁用最高优先级 Agent 后自动回退到下一优先级。
            fallback_id = create_project("PoolFallback", "禁用最高优先级后自动回退")
            for role in ("planner", "executor", "verifier"):
                call(
                    "PATCH",
                    f"/api/projects/{fallback_id}/agents/{role}/{high['id']}",
                    {"enabled": False},
                )
            fallback_executor = pool_role(fallback_id, "executor")["entries"]
            high_entry = next(
                entry for entry in fallback_executor if entry["agent_id"] == high["id"]
            )
            assert high_entry["enabled"] is False, high_entry
            assert [
                entry["agent_id"]
                for entry in fallback_executor
                if entry["enabled"]
            ][0] == low["id"], fallback_executor
            confirm(fallback_id)
            fallback = wait_completed(fallback_id, "回退后项目完成")
            assert role_jobs(fallback, "analysis")[0]["agent_id"] == low["id"], fallback[
                "jobs"
            ]
            assert all(
                job["agent_id"] == low["id"]
                for job in role_jobs(fallback, "executor")
            ), fallback["jobs"]
            assert all(
                job["agent_id"] == low["id"]
                for job in role_jobs(fallback, "verifier")
            ), fallback["jobs"]

            # 4. 任务级指定执行/验收 Agent，优先于项目池。
            override_id = create_project("TaskOverride", "任务级指定执行与验收 Agent")
            confirm(override_id)
            wait_project(
                override_id,
                lambda detail: any(
                    task["key"] == "T1"
                    and any(
                        job["task_id"] == task["id"]
                        and job["role"] == "executor"
                        and job["status"] in ("running", "succeeded")
                        for job in detail["jobs"]
                    )
                    for task in detail["tasks"]
                ),
                "T1 默认执行作业",
            )
            detail = call("GET", f"/api/projects/{override_id}")
            t1 = task_by_key(detail, "T1")
            t2 = task_by_key(detail, "T2")
            default_t1_exec = next(
                job
                for job in detail["jobs"]
                if job["task_id"] == t1["id"] and job["role"] == "executor"
            )
            assert default_t1_exec["agent_id"] == high["id"], default_t1_exec
            updated_task = call(
                "PATCH",
                f"/api/projects/{override_id}/tasks/{t2['id']}/agents",
                {"executor_agent_id": low["id"], "verifier_agent_id": low["id"]},
            )
            assert updated_task["executor_agent_id"] == low["id"], updated_task
            assert updated_task["verifier_agent_id"] == low["id"], updated_task
            t2_exec = wait_role_job(override_id, "executor", "T2")
            assert t2_exec["agent_id"] == low["id"], t2_exec
            t2_verify = wait_role_job(override_id, "verifier", "T2")
            assert t2_verify["agent_id"] == low["id"], t2_verify
            wait_completed(override_id, "任务覆盖项目完成")

            # 5. 清除任务覆盖后恢复项目池规则。
            clear_id = create_project("TaskOverrideClear", "清除任务覆盖后恢复项目池")
            confirm(clear_id)
            wait_project(
                clear_id,
                lambda detail: any(
                    task["key"] == "T1"
                    and any(
                        job["task_id"] == task["id"]
                        and job["role"] == "executor"
                        and job["status"] in ("running", "succeeded")
                        for job in detail["jobs"]
                    )
                    for task in detail["tasks"]
                ),
                "T1 执行作业",
            )
            detail = call("GET", f"/api/projects/{clear_id}")
            t2 = task_by_key(detail, "T2")
            call(
                "PATCH",
                f"/api/projects/{clear_id}/tasks/{t2['id']}/agents",
                {"executor_agent_id": low["id"], "verifier_agent_id": low["id"]},
            )
            cleared = call(
                "PATCH",
                f"/api/projects/{clear_id}/tasks/{t2['id']}/agents",
                {"executor_agent_id": None, "verifier_agent_id": None},
            )
            assert cleared["executor_agent_id"] is None, cleared
            assert cleared["verifier_agent_id"] is None, cleared
            t2_exec = wait_role_job(clear_id, "executor", "T2")
            assert t2_exec["agent_id"] == high["id"], t2_exec
            wait_completed(clear_id, "清除覆盖后项目完成")

            # 6. 全局 Agent 删除后，项目池条目标记不可用并给出阻塞原因。
            call("PATCH", f"/api/agents/{high['id']}", {"enabled": False})
            call("PATCH", f"/api/agents/{low['id']}", {"enabled": False})
            doomed_a = call(
                "POST",
                "/api/agents",
                {
                    "name": "Doomed A",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 1,
                },
            )
            doomed_b = call(
                "POST",
                "/api/agents",
                {
                    "name": "Doomed B",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 1,
                },
            )
            missing_id = create_project("PoolMissing", "全局 Agent 删除后应明确阻塞")
            missing_pool = pool_role(missing_id, "planner")["entries"]
            assert any(
                entry["agent_id"] == doomed_a["id"] and entry["available"]
                for entry in missing_pool
            ), missing_pool
            call("DELETE", f"/api/agents/{doomed_a['id']}")
            call("DELETE", f"/api/agents/{doomed_b['id']}")
            missing_pool = pool_role(missing_id, "planner")["entries"]
            unavailable = [
                entry
                for entry in missing_pool
                if entry["agent_id"] in (doomed_a["id"], doomed_b["id"])
            ]
            assert len(unavailable) == 2, missing_pool
            assert all(entry["available"] is False for entry in unavailable), unavailable
            assert all(
                entry["unavailable_reason"]
                and "删除" in entry["unavailable_reason"]
                for entry in unavailable
            ), unavailable
            assert {entry["name"] for entry in unavailable} == {
                "Doomed A",
                "Doomed B",
            }, unavailable
            confirm(missing_id)
            missing = wait_project(
                missing_id,
                lambda detail: detail["project"]["blocked_reason"]
                and "Doomed A" in detail["project"]["blocked_reason"]
                and "Doomed B" in detail["project"]["blocked_reason"],
                "全局 Agent 删除后的阻塞原因",
                30,
            )
            reason = missing["project"]["blocked_reason"]
            assert "不可用" in reason and "删除" in reason, reason
            code = expect_error(
                "PATCH",
                f"/api/projects/{reordered_id}/agents/invalid/{high['id']}",
                {"enabled": False},
            )
            assert code == 400, code

            # 重启服务，验证项目池配置持久化。
            server.restart()

            pool = call("GET", f"/api/projects/{reordered_id}/agents")
            roles = {group["role"]: group["entries"] for group in pool["roles"]}
            assert [
                entry["agent_id"] for entry in roles["executor"]
            ] == executor_after, roles["executor"]
            assert [
                entry["agent_id"] for entry in roles["planner"]
            ] == [entry["agent_id"] for entry in planner_after], roles["planner"]
            assert [
                entry["priority"] for entry in roles["executor"]
            ] == list(range(len(roles["executor"]))), roles["executor"]

            team_pool = call("GET", "/api/team/default")
            team_roles = {
                group["role"]: group["entries"] for group in team_pool["roles"]
            }
            assert [
                entry["agent_id"] for entry in team_roles["executor"]
            ] == team_restored_ids, team_roles["executor"]
            assert [
                entry["priority"] for entry in team_roles["executor"]
            ] == list(range(len(team_roles["executor"]))), team_roles["executor"]

            missing = call("GET", f"/api/projects/{missing_id}/agents")
            missing_entries = {
                group["role"]: group["entries"] for group in missing["roles"]
            }
            for role, entries in missing_entries.items():
                for entry in entries:
                    if entry["agent_id"] in (doomed_a["id"], doomed_b["id"]):
                        assert entry["available"] is False, (role, entry)
                        assert "删除" in (
                            entry["unavailable_reason"] or ""
                        ), (role, entry)

            print("项目池配置重启后保持")


if __name__ == "__main__":
    main()
