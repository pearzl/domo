#!/usr/bin/env python3
"""DOMO 严格优先级 Agent 调度测试。

覆盖范围：
  1. 高优先级 Agent 可用但并发已满时，新任务保持 queued，等待原因包含该
     Agent 名称与并发占用，且不会产生低优先级 Agent 的作业；
  2. 禁用高优先级 Agent 后低优先级 Agent 接手；
  3. 项目池中全部 Agent 不可用时，阻塞信息逐个列出 Agent 及原因；
  4. 失败处理决定换 Agent 后，一次性 avoid 状态让下一次派发跳过失败
     Agent，派发成功后清除，后续任务恢复使用高优先级 Agent。

使用内置 mock Agent 与脚本化 CLI Agent，不访问外网；临时数据全部放在系统
临时目录。退出码 0 表示全部通过。
"""

import json
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


HIGH_SCRIPT = '''\
import json
import os
import sys
import time

job_dir, cwd, root = sys.argv[1], sys.argv[2], sys.argv[3]
with open(os.path.join(job_dir, "context.json"), encoding="utf-8") as handle:
    context = json.load(handle)
key = (context.get("task") or {}).get("key") or ""
if os.environ.get("DOMO_JOB_ROLE", "") != "executor":
    with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
        json.dump(
            {"kind": "failed", "reason": "高优先级 CLI Agent 只承担执行角色"},
            handle,
            ensure_ascii=False,
        )
    sys.exit(0)
# 占用并发槽位，让同角色后续任务只能排队等待。
time.sleep(4)
os.makedirs(os.path.join(cwd, "domo_mock"), exist_ok=True)
with open(os.path.join(cwd, "domo_mock", f"{key}.txt"), "w", encoding="utf-8") as handle:
    handle.write(key + "\\n")
with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
    json.dump(
        {"kind": "done", "summary": f"高优先级 CLI 完成 {key}"},
        handle,
        ensure_ascii=False,
    )
'''

FLAKY_SCRIPT = '''\
import json
import os
import sys
import time

job_dir, cwd, root = sys.argv[1], sys.argv[2], sys.argv[3]
with open(os.path.join(job_dir, "context.json"), encoding="utf-8") as handle:
    context = json.load(handle)
key = (context.get("task") or {}).get("key") or ""
if os.environ.get("DOMO_JOB_ROLE", "") != "executor":
    with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
        json.dump(
            {"kind": "failed", "reason": "换人 Agent 只承担执行角色"},
            handle,
            ensure_ascii=False,
        )
    sys.exit(0)
marker = os.path.join(root, "flaky-failed-once")
if key == "T1" and not os.path.exists(marker):
    with open(marker, "w", encoding="utf-8") as handle:
        handle.write("1")
    with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
        json.dump(
            {"kind": "failed", "reason": "永久错误：Agent 配置无效"},
            handle,
            ensure_ascii=False,
        )
    sys.exit(0)
time.sleep(0.2)
os.makedirs(os.path.join(cwd, "domo_mock"), exist_ok=True)
with open(os.path.join(cwd, "domo_mock", f"{key}.txt"), "w", encoding="utf-8") as handle:
    handle.write(key + "\\n")
with open(os.path.join(job_dir, "result.json"), "w", encoding="utf-8") as handle:
    json.dump(
        {"kind": "done", "summary": f"换人 Agent 完成 {key}"},
        handle,
        ensure_ascii=False,
    )
'''


def main():
    with temp_root("domo-priority-") as temp:
        repo = temp / "repo"
        agents_dir = temp / "agents"
        agents_dir.mkdir(parents=True, exist_ok=True)
        init_repo(repo)

        high_script = agents_dir / "high.py"
        high_script.write_text(HIGH_SCRIPT, encoding="utf-8")
        flaky_script = agents_dir / "flaky.py"
        flaky_script.write_text(FLAKY_SCRIPT, encoding="utf-8")

        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "60",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "60",
                "PRIORITY_TEST_ROOT": str(temp),
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
                return call(
                    "POST",
                    "/api/projects",
                    {"name": name, "goal": goal, "repo_path": str(repo), "base_branch": "main"},
                )["project"]["id"]

            def confirm(project_id):
                call("POST", f"/api/projects/{project_id}/confirm", {})

            def task_by_key(detail, key):
                return next(task for task in detail["tasks"] if task["key"] == key)

            def role_jobs(detail, role):
                return [job for job in detail["jobs"] if job["role"] == role]

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
                    "name": "Priority High",
                    "kind": "cli",
                    "roles": ["executor"],
                    "max_concurrency": 1,
                    "command": [sys.executable, str(high_script), "{job_dir}", "{cwd}", str(temp)],
                },
            )
            flaky = call(
                "POST",
                "/api/agents",
                {
                    "name": "Priority Flaky",
                    "kind": "cli",
                    "roles": ["executor"],
                    "max_concurrency": 1,
                    "command": [sys.executable, str(flaky_script), "{job_dir}", "{cwd}", str(temp)],
                },
            )
            low = call(
                "POST",
                "/api/agents",
                {
                    "name": "Priority Low",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 1,
                },
            )

            # 1. 高优先级可用但并发已满：任务排队等待，低优先级不接单。
            busy_id = create_project("PriorityBusy", "并发占满时严格等待高优先级 [parallel]")
            call(
                "PATCH",
                f"/api/projects/{busy_id}/agents/executor/{flaky['id']}",
                {"enabled": False},
            )
            confirm(busy_id)
            wait_project(
                busy_id,
                lambda detail: any(
                    job["agent_id"] == high["id"]
                    and job["role"] == "executor"
                    and job["status"] == "running"
                    for job in detail["jobs"]
                ),
                "高优先级执行作业",
            )
            waiting = wait_project(
                busy_id,
                lambda detail: any(
                    task["status"] == "queued"
                    and task["waiting_reason"]
                    and "Priority High" in task["waiting_reason"]
                    and "并发 1/1" in task["waiting_reason"]
                    for task in detail["tasks"]
                ),
                "高优先级并发占满时的等待原因",
                30,
            )
            waited = next(
                task
                for task in waiting["tasks"]
                if task["waiting_reason"]
                and "Priority High" in task["waiting_reason"]
                and "并发 1/1" in task["waiting_reason"]
            )
            assert waited["status"] == "queued", waited
            assert not [
                job
                for job in role_jobs(waiting, "executor")
                if job["agent_id"] == low["id"]
            ], "高优先级忙碌期间不应产生低优先级执行作业"
            assert "Priority High" in (waiting["project"]["blocked_reason"] or ""), waiting["project"]
            assert "并发 1/1" in (waiting["project"]["blocked_reason"] or ""), waiting["project"]

            # 2. 禁用高优先级后，低优先级接手。
            call("PATCH", f"/api/agents/{high['id']}", {"enabled": False})
            handover = wait_project(
                busy_id,
                lambda detail: any(
                    job["agent_id"] == low["id"] and job["role"] == "executor"
                    for job in detail["jobs"]
                ),
                "低优先级接手执行",
                30,
            )
            assert any(
                job["agent_id"] == low["id"] and job["role"] == "executor"
                for job in handover["jobs"]
            ), handover["jobs"]
            busy = wait_completed(busy_id, "并发项目完成")
            assert any(
                job["agent_id"] == low["id"] and job["role"] == "executor"
                for job in busy["jobs"]
            ), busy["jobs"]

            # 3. 任务 Agent 池全部不可用时，阻塞原因逐个列出 Agent。
            blocked_id = create_project("PriorityBlocked", "全部执行 Agent 不可用")
            for agent in (high, flaky, low):
                call(
                    "PATCH",
                    f"/api/projects/{blocked_id}/agents/executor/{agent['id']}",
                    {"enabled": False},
                )
            confirm(blocked_id)
            blocked = wait_project(
                blocked_id,
                lambda detail: detail["project"]["blocked_reason"]
                and all(
                    name in detail["project"]["blocked_reason"]
                    for name in ("Priority High", "Priority Flaky", "Priority Low")
                )
                and "任务 Agent 池中已禁用" in detail["project"]["blocked_reason"],
                "全部 Agent 不可用的阻塞原因",
                30,
            )
            reason = blocked["project"]["blocked_reason"]
            for name in ("Priority High", "Priority Flaky", "Priority Low"):
                assert f"「{name}」（任务 Agent 池中已禁用）" in reason, reason
            assert task_by_key(blocked, "T1")["status"] == "queued", task_by_key(blocked, "T1")

            # 4. 失败换人后一次性跳过失败 Agent，派发成功后恢复高优先级选择。
            switch_id = create_project("PrioritySwitch", "失败换人后恢复高优先级")
            call(
                "PATCH",
                f"/api/projects/{switch_id}/agents/executor/{high['id']}",
                {"enabled": False},
            )
            confirm(switch_id)
            switched = wait_project(
                switch_id,
                lambda detail: any(
                    job["agent_id"] == low["id"]
                    and job["role"] == "executor"
                    and job["task_id"] == task_by_key(detail, "T1")["id"]
                    for job in detail["jobs"]
                ),
                "失败后低优先级接手 T1",
                60,
            )
            failed = next(
                job
                for job in switched["jobs"]
                if job["agent_id"] == flaky["id"] and job["role"] == "executor"
            )
            assert failed["status"] in ("failed", "succeeded"), failed
            assert any(
                job["agent_id"] == low["id"]
                and job["role"] == "executor"
                and job["task_id"] == task_by_key(switched, "T1")["id"]
                for job in switched["jobs"]
            ), switched["jobs"]
            restored = wait_project(
                switch_id,
                lambda detail: any(
                    job["agent_id"] == flaky["id"]
                    and job["role"] == "executor"
                    and job["task_id"] == task_by_key(detail, "T2")["id"]
                    for job in detail["jobs"]
                ),
                "一次性 avoid 清除后恢复高优先级",
                60,
            )
            assert any(
                job["agent_id"] == flaky["id"]
                and job["role"] == "executor"
                and job["task_id"] == task_by_key(restored, "T2")["id"]
                for job in restored["jobs"]
            ), restored["jobs"]
            wait_completed(switch_id, "换人项目完成")

            print("严格优先级调度断言全部通过")


if __name__ == "__main__":
    main()
