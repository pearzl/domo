#!/usr/bin/env python3
"""DOMO 任务专用 Agent 池测试。

覆盖范围：
  1. 新建任务从项目池对应角色快照全部条目；修改任务池排序或启停只影响该任务，
     项目池与其他任务保持不变；
  2. 项目池变化不改写已有任务池，新任务按当前项目池快照初始化；
  3. 任务池某角色只保留一个启用 Agent 时等价于指定该 Agent；该 Agent 全局
     不可用时作业给出明确的“任务 Agent 池”等待原因，不回退项目池、
     不转给其他 Agent；
  4. executor_agent_id 覆盖对应单 Agent 池，清除覆盖恢复项目池快照，
     调度只读取任务池；
  5. 任务池修改在服务重启后保持。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
"""

import json
import time
import urllib.error
import urllib.request

from harness import Server, ensure_server_built, init_repo, temp_root, wait_for


def main():
    with temp_root("domo-task-agent-pool-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "60",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "2000",
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

            def wait_project(project_id, predicate, label, timeout=150):
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

            def task_pool_role(project_id, task_id, role):
                pool = call("GET", f"/api/projects/{project_id}/tasks/{task_id}/agents")
                return next(group for group in pool["roles"] if group["role"] == role)

            def summarize(group):
                return [
                    [entry["agent_id"], entry["priority"], entry["enabled"]]
                    for entry in group["entries"]
                ]

            def add_task(project_id, key, deps=()):
                return call(
                    "POST",
                    f"/api/projects/{project_id}/tasks",
                    {
                        "key": key,
                        "title": f"{key} 标题",
                        "description": f"{key} 描述",
                        "acceptance": ["产物存在"],
                        "deps": list(deps),
                    },
                )

            def wait_t1_executor_running(project_id, label):
                return wait_project(
                    project_id,
                    lambda d: any(
                        task["key"] == "T1"
                        and any(
                            job["task_id"] == task["id"]
                            and job["role"] == "executor"
                            and job["status"] == "running"
                            for job in d["jobs"]
                        )
                        for task in d["tasks"]
                    ),
                    label,
                )

            def clear_team():
                # 停用存量 Agent；项目池快照只保留测试 Agent 为可用。
                for agent in call("GET", "/api/agents"):
                    call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})

            clear_team()

            high = call(
                "POST",
                "/api/agents",
                {
                    "name": "Task High",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )
            low = call(
                "POST",
                "/api/agents",
                {
                    "name": "Task Low",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 1,
                },
            )

            state = {}
            state["high"] = high["id"]
            state["low"] = low["id"]

            # ---------- 1. 快照初始化与独立性 ----------
            snap_id = create_project("TaskSnap", "新建任务获得项目池角色快照")
            confirm(snap_id)
            detail = wait_t1_executor_running(snap_id, "TaskSnap T1 执行作业")
            t1 = task_by_key(detail, "T1")
            t2 = task_by_key(detail, "T2")

            pool_view = call(
                "GET", f"/api/projects/{snap_id}/tasks/{t2['id']}/agents"
            )
            assert [group["role"] for group in pool_view["roles"]] == [
                "executor",
                "verifier",
            ], pool_view
            initial_exec = summarize(pool_role(snap_id, "executor"))
            assert summarize(
                task_pool_role(snap_id, t2["id"], "executor")
            ) == initial_exec, summarize(task_pool_role(snap_id, t2["id"], "executor"))

            # 未知角色被拒绝
            code = expect_error(
                "PATCH",
                f"/api/projects/{snap_id}/tasks/{t2['id']}/agents/planner/{high['id']}",
                {"enabled": True},
            )
            assert code == 400, code

            # 修改 T2 任务池：排序与启停。
            call(
                "PATCH",
                f"/api/projects/{snap_id}/tasks/{t2['id']}/agents/executor/{low['id']}",
                {"position": 0},
            )
            code = expect_error(
                "PATCH",
                f"/api/projects/{snap_id}/tasks/{t2['id']}/agents/executor/{low['id']}",
                {"position": 5},
            )
            assert code == 400, code
            call(
                "PATCH",
                f"/api/projects/{snap_id}/tasks/{t2['id']}/agents/executor/{low['id']}",
                {"enabled": False},
            )

            # 项目池与其他任务不受影响
            assert summarize(pool_role(snap_id, "executor")) == initial_exec, summarize(
                pool_role(snap_id, "executor")
            )
            assert summarize(
                task_pool_role(snap_id, t1["id"], "executor")
            ) == initial_exec, summarize(task_pool_role(snap_id, t1["id"], "executor"))
            exec_ids = [
                entry["agent_id"]
                for entry in task_pool_role(snap_id, t2["id"], "executor")["entries"]
                if entry["agent_id"] in (high["id"], low["id"])
            ]
            assert exec_ids == [low["id"], high["id"]], exec_ids
            verifier_sum = [
                (entry["agent_id"], entry["priority"])
                for entry in task_pool_role(snap_id, t2["id"], "verifier")["entries"]
                if entry["agent_id"] in (high["id"], low["id"])
            ]
            assert verifier_sum == [(high["id"], 1), (low["id"], 2)], verifier_sum
            state["initial_executor"] = initial_exec
            state["t1_executor"] = summarize(
                task_pool_role(snap_id, t1["id"], "executor")
            )
            state["t2_executor"] = summarize(
                task_pool_role(snap_id, t2["id"], "executor")
            )
            state["t2_verifier"] = summarize(
                task_pool_role(snap_id, t2["id"], "verifier")
            )

            # ---------- 2. 项目池变化不改写既有任务池 ----------
            call(
                "PATCH",
                f"/api/projects/{snap_id}/agents/executor/{low['id']}",
                {"position": 0},
            )
            assert [
                entry["agent_id"]
                for entry in task_pool_role(snap_id, t2["id"], "executor")["entries"]
                if entry["agent_id"] in (high["id"], low["id"])
            ] == [low["id"], high["id"]], task_pool_role(
                snap_id, t2["id"], "executor"
            )
            new_task = add_task(snap_id, "T3", deps=["T2"])
            snapshot_t3 = [
                entry["agent_id"]
                for entry in task_pool_role(
                    snap_id, new_task["id"], "executor"
                )["entries"]
                if entry["agent_id"] in (high["id"], low["id"])
            ]
            assert snapshot_t3 == [low["id"], high["id"]], snapshot_t3
            project_executor = pool_role(snap_id, "executor")
            state["project_executor"] = summarize(project_executor)

            # ---------- 3. 单 Agent 等价指定与阻塞 ----------
            uniq_id = create_project(
                "TaskUnique", "任务池单 Agent 等价指定且不可用时不静默回退"
            )
            confirm(uniq_id)
            detail = wait_t1_executor_running(uniq_id, "TaskUnique T1 执行作业")
            u2 = task_by_key(detail, "T2")
            # 等 u1 的验收作业派发后，任务池已完成过一次调度
            wait_project(
                uniq_id,
                lambda d: any(
                    job["task_id"] == task_by_key(d, "T1")["id"]
                    and job["role"] == "verifier"
                    and job["status"] in ("running", "succeeded")
                    for job in d["jobs"]
                ),
                "TaskUnique T1 验收作业",
            )
            # u2 的任务池执行者与验收者都只剩 high 启用
            call(
                "PATCH",
                f"/api/projects/{uniq_id}/tasks/{u2['id']}/agents/executor/{low['id']}",
                {"enabled": False},
            )
            call(
                "PATCH",
                f"/api/projects/{uniq_id}/tasks/{u2['id']}/agents/verifier/{low['id']}",
                {"enabled": False},
            )
            # 把 high 全局禁用：u2 只能等待，不能静默回退到 low
            call("PATCH", f"/api/agents/{high['id']}", {"enabled": False})
            detail = wait_project(
                uniq_id,
                lambda d: any(
                    task["key"] == "T2"
                    and "任务 Agent 池" in (task["waiting_reason"] or "")
                    for task in d["tasks"]
                ),
                "u2 执行等待原因",
                60,
            )
            # 禁用时刻已完成的历史作业不影响判断：不得有任何 T2 作业落到池外的 low。
            fallback_jobs = [
                job
                for job in detail["jobs"]
                if job["task_id"] == u2["id"] and job["agent_id"] != high["id"]
            ]
            assert fallback_jobs == [], fallback_jobs

            # 恢复 high 后 u2 完全由唯一 Agent 推进
            call("PATCH", f"/api/agents/{high['id']}", {"enabled": True})
            detail = wait_project(
                uniq_id,
                lambda d: d["project"]["stage"] == "summarizing"
                and d["project"]["report"],
                "TaskUnique 完成",
            )
            for job in detail["jobs"]:
                if job["task_id"] == u2["id"] and job["role"] in (
                    "executor",
                    "verifier",
                ):
                    assert job["agent_id"] == high["id"], job

            # ---------- 4. 覆盖字段与清除重置 ----------
            legacy_id = create_project("TaskLegacy", "旧覆盖字段迁移为单 Agent 池")
            confirm(legacy_id)
            detail = wait_t1_executor_running(legacy_id, "TaskLegacy T1 执行作业")
            lt2 = task_by_key(detail, "T2")
            legacy_task = call(
                "PATCH",
                f"/api/projects/{legacy_id}/tasks/{lt2['id']}/agents",
                {"executor_agent_id": low["id"]},
            )
            assert legacy_task["executor_agent_id"] == low["id"], legacy_task
            legacy_pool = task_pool_role(legacy_id, lt2["id"], "executor")
            assert [
                entry["agent_id"] for entry in legacy_pool["entries"]
            ] == [low["id"]], legacy_pool
            cleared = call(
                "PATCH",
                f"/api/projects/{legacy_id}/tasks/{lt2['id']}/agents",
                {"executor_agent_id": None},
            )
            assert cleared["executor_agent_id"] is None, cleared
            legacy_pool = task_pool_role(legacy_id, lt2["id"], "executor")
            assert [
                entry["agent_id"]
                for entry in legacy_pool["entries"]
                if entry["enabled"]
            ] == [high["id"], low["id"]], legacy_pool
            # 调度依据任务池：T2 的作业交给池内最高优先级 high
            detail = wait_project(
                legacy_id,
                lambda d: d["project"]["stage"] == "summarizing"
                and d["project"]["report"],
                "TaskLegacy 完成",
            )
            t2_exec = next(
                job
                for job in detail["jobs"]
                if job["task_id"] == lt2["id"] and job["role"] == "executor"
            )
            assert t2_exec["agent_id"] == high["id"], t2_exec

            state["t1_project"] = snap_id

            # 重启服务验证持久化。
            server.restart()

            snap_id = state["t1_project"]
            detail = call("GET", f"/api/projects/{snap_id}")
            t1 = next(task for task in detail["tasks"] if task["key"] == "T1")
            t2 = next(task for task in detail["tasks"] if task["key"] == "T2")

            assert summarize(
                task_pool_role(snap_id, t2["id"], "executor")
            ) == state["t2_executor"], (
                summarize(task_pool_role(snap_id, t2["id"], "executor")),
                state["t2_executor"],
            )
            assert summarize(
                task_pool_role(snap_id, t1["id"], "executor")
            ) == state["t1_executor"], (
                summarize(task_pool_role(snap_id, t1["id"], "executor")),
                state["t1_executor"],
            )
            assert summarize(
                task_pool_role(snap_id, t2["id"], "verifier")
            ) == state["t2_verifier"], (
                summarize(task_pool_role(snap_id, t2["id"], "verifier")),
                state["t2_verifier"],
            )
            project = call("GET", f"/api/projects/{snap_id}/agents")
            project_executor = next(
                group for group in project["roles"] if group["role"] == "executor"
            )
            assert summarize(project_executor) == state["project_executor"], (
                summarize(project_executor),
                state["project_executor"],
            )
            print("OK")
            print("全部通过")


if __name__ == "__main__":
    main()
