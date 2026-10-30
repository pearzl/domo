#!/usr/bin/env python3
"""DOMO 成本用量记录与 Agent 可用性限额测试。

覆盖范围：
  1. mock 结果携带 usage 时，作业记录输入/输出 token、费用与时长；
  2. /api/usage 按项目、Agent、角色聚合，并支持 Agent 与时间范围过滤，
     聚合值与作业明细一致；项目详情返回本项目汇总；
  3. mock 不返回 usage 时字段为未知（null）而非 0，聚合单独统计未知作业；
  4. 未知用量不参与限额扣减：Agent 当前窗口用量只累加已知作业；
  5. 用量限额低于已消耗值时，调度跳过该 Agent 并在项目上给出限额等待原因；
     其他 Agent 提高限额后可继续派发；
  6. 重置用量窗口后该 Agent 自动恢复可用并重新获得派发；
  7. 配置不包含当前时间的每周可用时段时不派发并给出时段等待原因；
     包含当前时间后恢复派发；
  8. 服务重启后用量记录、聚合结果、可用时段与限额配置保持。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录，
退出码 0 表示全部通过。
"""

import json
import time
from datetime import datetime, timedelta, timezone

from harness import (
    Server,
    ensure_server_built,
    init_repo,
    temp_root,
    wait_for,
)

EXPECTED_INPUT = 100
EXPECTED_OUTPUT = 40
EXPECTED_COST = 0.25
EXPECTED_DURATION = 1234


def main():
    with temp_root("domo-usage-limit-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        state_path = temp / "state.json"

        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "60",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "80",
                "DOMO_MOCK_USAGE": "1",
                "DOMO_MOCK_INPUT_TOKENS": str(EXPECTED_INPUT),
                "DOMO_MOCK_OUTPUT_TOKENS": str(EXPECTED_OUTPUT),
                "DOMO_MOCK_COST": str(EXPECTED_COST),
                "DOMO_MOCK_USAGE_DURATION_MS": str(EXPECTED_DURATION),
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
                    pending_goals[result["project"]["id"]] = goal
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

            def completed(detail):
                return detail["project"]["stage"] == "summarizing" and bool(
                    detail["project"]["report"]
                )

            def agent_by_id(agents, agent_id):
                return next(agent for agent in agents if agent["id"] == agent_id)

            def usage(path):
                return call("GET", path)

            def jobs_by(detail):
                return detail["jobs"]

            def assert_totals(actual, expected_jobs, label, unknown=0):
                assert actual["jobs"] == expected_jobs, (label, actual)
                assert actual["input_tokens"] == EXPECTED_INPUT * (
                    expected_jobs - unknown
                ), (label, actual)
                assert actual["output_tokens"] == EXPECTED_OUTPUT * (
                    expected_jobs - unknown
                ), (label, actual)
                assert abs(
                    actual["cost"] - EXPECTED_COST * (expected_jobs - unknown)
                ) < 1e-9, (label, actual)
                assert actual["unknown_jobs"] == unknown, (label, actual)

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})

            high = call(
                "POST",
                "/api/agents",
                {
                    "name": "Usage High",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )
            low = call(
                "POST",
                "/api/agents",
                {
                    "name": "Usage Low",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )
            # 新建项目的项目池继承默认团队。

            # 限额配置同时让 Agent 暴露当前窗口用量。
            high_limit = {
                "enabled": True,
                "metric": "tokens",
                "window": "day",
                "limit": 1000000,
            }
            for agent in (high, low):
                call("PATCH", f"/api/agents/{agent['id']}", {"usage_limit": high_limit})

            # 1. 带 usage 的项目：作业字段与多维聚合一致。
            p1 = create_project("UsageKnown", "[usage] 两个依赖任务的用量记录")
            call("POST", f"/api/projects/{p1}/confirm", {})
            detail1 = wait_project(p1, completed, "带用量项目完成")
            jobs1 = jobs_by(detail1)
            assert len(jobs1) >= 6, jobs1
            for job in jobs1:
                assert job["usage_known"] is True, job
                assert job["input_tokens"] == EXPECTED_INPUT, job
                assert job["output_tokens"] == EXPECTED_OUTPUT, job
                assert abs(job["cost"] - EXPECTED_COST) < 1e-9, job
                assert job["duration_ms"] == EXPECTED_DURATION, job

            assert_totals(detail1["usage"]["totals"], len(jobs1), "项目详情用量")

            by_project = usage(f"/api/usage?project_id={p1}&group_by=project")
            assert_totals(by_project["totals"], len(jobs1), "按项目聚合")
            assert len(by_project["groups"]) == 1, by_project
            assert by_project["groups"][0]["key"] == p1, by_project
            assert_totals(by_project["groups"][0], len(jobs1), "项目分组")

            expected_agents = {}
            expected_roles = {}
            for job in jobs1:
                agent_key = job["agent_id"] or "unknown"
                for bucket, key in (
                    (expected_agents, agent_key),
                    (expected_roles, job["role"]),
                ):
                    entry = bucket.setdefault(
                        key,
                        {
                            "jobs": 0,
                            "input_tokens": 0,
                            "output_tokens": 0,
                            "cost": 0.0,
                        },
                    )
                    entry["jobs"] += 1
                    entry["input_tokens"] += job["input_tokens"]
                    entry["output_tokens"] += job["output_tokens"]
                    entry["cost"] += job["cost"]

            by_agent = usage(f"/api/usage?project_id={p1}&group_by=agent")
            assert by_agent["totals"]["jobs"] == len(jobs1), by_agent
            groups_by_agent = {group["key"]: group for group in by_agent["groups"]}
            assert set(groups_by_agent) == set(expected_agents), (
                groups_by_agent,
                expected_agents,
            )
            for key, expected in expected_agents.items():
                group = groups_by_agent[key]
                assert group["jobs"] == expected["jobs"], (group, expected)
                assert group["input_tokens"] == expected["input_tokens"], (
                    group,
                    expected,
                )
                assert group["output_tokens"] == expected["output_tokens"], (
                    group,
                    expected,
                )
                assert abs(group["cost"] - expected["cost"]) < 1e-9, (group, expected)

            by_role = usage(f"/api/usage?project_id={p1}&group_by=role")
            groups_by_role = {group["key"]: group for group in by_role["groups"]}
            assert set(groups_by_role) == set(expected_roles), (
                groups_by_role,
                expected_roles,
            )
            for key, expected in expected_roles.items():
                group = groups_by_role[key]
                assert group["jobs"] == expected["jobs"], (group, expected)
                assert group["input_tokens"] == expected["input_tokens"], (
                    group,
                    expected,
                )

            # 项目详情按 Agent/角色汇总与 /api/usage 一致。
            detail_agent = {
                (group["agent_id"] or "unknown"): group
                for group in detail1["usage"]["by_agent"]
            }
            for key, expected in expected_agents.items():
                assert detail_agent[key]["jobs"] == expected["jobs"], detail_agent[key]
            detail_role = {
                group["role"]: group for group in detail1["usage"]["by_role"]
            }
            for key, expected in expected_roles.items():
                assert detail_role[key]["jobs"] == expected["jobs"], detail_role[key]

            by_high = usage(f"/api/usage?project_id={p1}&agent_id={high['id']}")
            assert by_high["totals"]["jobs"] == expected_agents[high["id"]]["jobs"], (
                by_high
            )
            future = (datetime.now(timezone.utc) + timedelta(hours=1)).strftime(
                "%Y-%m-%dT%H:%M:%SZ"
            )
            empty = usage(f"/api/usage?project_id={p1}&from={future}")
            assert empty["totals"]["jobs"] == 0 and empty["groups"] == [], empty

            # 2. 不返回 usage 的项目：字段为未知而非 0。
            p2 = create_project("UsageUnknown", "[nousage] 未知用量不应扣减限额")
            call("POST", f"/api/projects/{p2}/confirm", {})
            detail2 = wait_project(p2, completed, "未知用量项目完成")
            jobs2 = jobs_by(detail2)
            assert jobs2, detail2
            for job in jobs2:
                assert job["usage_known"] is False, job
                assert job["input_tokens"] is None, job
                assert job["output_tokens"] is None, job
                assert job["cost"] is None, job
                assert job["duration_ms"] is not None and job["duration_ms"] >= 0, job

            unknown = usage(f"/api/usage?project_id={p2}")
            assert unknown["totals"]["jobs"] == len(jobs2), unknown
            assert unknown["totals"]["unknown_jobs"] == len(jobs2), unknown
            assert unknown["totals"]["input_tokens"] == 0, unknown
            assert unknown["totals"]["output_tokens"] == 0, unknown
            assert unknown["totals"]["cost"] == 0, unknown

            # 当前窗口用量只累加已知作业；未知项目作业不扣减限额。
            all_usage = usage("/api/usage?group_by=agent")
            known_by_agent = {}
            for group in all_usage["groups"]:
                key = group["agent_id"] or "unknown"
                known_by_agent[key] = group["input_tokens"] + group["output_tokens"]
            agents = call("GET", "/api/agents")
            for agent in (high, low):
                current = agent_by_id(agents, agent["id"])
                window = current["usage_window"]
                assert window is not None, current
                expected_used = known_by_agent.get(agent["id"], 0)
                assert window["used"] == expected_used, (
                    current["name"],
                    window,
                    expected_used,
                )
                assert window["exceeded"] is False, window
            assert known_by_agent.get(high["id"], 0) > 0, known_by_agent

            # 让低优先级 Agent 也产生已知用量，便于同时验证限额跳过。
            warm = create_project("UsageWarmLow", "[single] 低优先级 Agent 起量")
            for role in ("planner", "executor", "verifier"):
                call(
                    "PATCH",
                    f"/api/projects/{warm}/agents/{role}/{high['id']}",
                    {"enabled": False},
                )
            call("POST", f"/api/projects/{warm}/confirm", {})
            warm_detail = wait_project(warm, completed, "低优先级 Agent 起量完成")
            assert warm_detail["jobs"] and all(
                job["agent_id"] == low["id"] for job in warm_detail["jobs"]
            ), warm_detail["jobs"]
            low_used = next(
                agent["usage_window"]["used"]
                for agent in call("GET", "/api/agents")
                if agent["id"] == low["id"]
            )
            assert low_used > 0, low_used

            # 3. 限额低于已消耗值：跳过 Agent 并显示等待原因，其他 Agent 可继续。
            low_limit = {
                "enabled": True,
                "metric": "tokens",
                "window": "day",
                "limit": 1,
            }
            for agent in (high, low):
                call("PATCH", f"/api/agents/{agent['id']}", {"usage_limit": low_limit})

            p3 = create_project("UsageLimited", "[usage] 限额保护与等待原因")
            call("POST", f"/api/projects/{p3}/confirm", {})

            def limited_waiting(detail):
                reason = detail["project"]["blocked_reason"] or ""
                return "限额" in reason and not detail["jobs"]

            limited = wait_project(p3, limited_waiting, "限额等待原因", 40)
            assert "Usage High" in limited["project"]["blocked_reason"], limited[
                "project"
            ]

            # 放宽另一 Agent 的限额后继续派发，受限 Agent 不被使用。
            call(
                "PATCH",
                f"/api/agents/{low['id']}",
                {
                    "usage_limit": {
                        "enabled": True,
                        "metric": "tokens",
                        "window": "day",
                        "limit": 1000000,
                    }
                },
            )
            detail3 = wait_project(p3, completed, "提高其他 Agent 限额后完成")
            assert detail3["jobs"], detail3
            assert all(
                job["agent_id"] == low["id"] for job in detail3["jobs"]
            ), detail3["jobs"]

            agents = call("GET", "/api/agents")
            limited_high = agent_by_id(agents, high["id"])
            assert limited_high["usage_window"]["exceeded"] is True, limited_high
            assert (
                agent_by_id(agents, low["id"])["usage_window"]["exceeded"] is False
            ), agents

            # 4. 窗口重置后恢复可用并重新获得派发。
            reset = call("POST", f"/api/agents/{high['id']}/usage/reset", {})
            assert reset["usage_window"]["exceeded"] is False, reset
            assert reset["usage_window"]["used"] == 0, reset

            p4 = create_project("UsageWindowReset", "[single] 窗口重置后恢复派发")
            call("POST", f"/api/projects/{p4}/confirm", {})
            resumed = wait_project(
                p4,
                lambda detail: any(
                    job["agent_id"] == high["id"] for job in detail["jobs"]
                ),
                "窗口重置后重新派发",
            )
            assert any(
                job["agent_id"] == high["id"] for job in resumed["jobs"]
            ), resumed["jobs"]
            wait_project(p4, completed, "重置窗口后的项目完成")

            # 收紧第二个 Agent，随后仅由时段控制是否派发。
            call("PATCH", f"/api/agents/{low['id']}", {"usage_limit": low_limit})

            now = datetime.now()
            other_weekday = (now.weekday() + 1) % 7 + 1  # API: 1=周一, 7=周日
            current_weekday = now.weekday() + 1
            slot_agent = call(
                "POST",
                "/api/agents",
                {
                    "name": "Slot Agent",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                    "availability": {
                        "enabled": True,
                        "slots": [
                            {
                                "weekday": other_weekday,
                                "start": "00:00",
                                "end": "23:59",
                            }
                        ],
                    },
                },
            )

            p5 = create_project("UsageSchedule", "[single] 可用时段保护")
            call("POST", f"/api/projects/{p5}/confirm", {})

            def schedule_waiting(detail):
                reason = detail["project"]["blocked_reason"] or ""
                return "可用时段" in reason and not detail["jobs"]

            scheduled = wait_project(p5, schedule_waiting, "可用时段等待原因", 40)
            assert "Slot Agent" in scheduled["project"]["blocked_reason"], scheduled[
                "project"
            ]

            call(
                "PATCH",
                f"/api/agents/{slot_agent['id']}",
                {
                    "availability": {
                        "enabled": True,
                        "slots": [
                            {
                                "weekday": current_weekday,
                                "start": "00:00",
                                "end": "23:59",
                            }
                        ],
                    }
                },
            )
            detail5 = wait_project(
                p5,
                lambda detail: any(
                    job["agent_id"] == slot_agent["id"] for job in detail["jobs"]
                )
                and completed(detail),
                "包含当前时间后恢复派发",
            )
            assert all(
                job["agent_id"] == slot_agent["id"] for job in detail5["jobs"]
            ), detail5["jobs"]

            with open(state_path, "w", encoding="utf-8") as handle:
                json.dump(
                    {
                        "project1": p1,
                        "project1_totals": detail1["usage"]["totals"],
                        "high_id": high["id"],
                        "slot_id": slot_agent["id"],
                        "current_weekday": current_weekday,
                    },
                    handle,
                    ensure_ascii=False,
                )
            print("用量记录、聚合、限额与可用时段场景通过")

            print("==> 重启服务，验证用量与可用性配置持久化")
            server.restart()

            with open(state_path, encoding="utf-8") as handle:
                state = json.load(handle)

            def get(path):
                return server.call("GET", path)

            agents = get("/api/agents")
            high = next(agent for agent in agents if agent["id"] == state["high_id"])
            assert high["usage_limit"] == {
                "enabled": True,
                "metric": "tokens",
                "window": "day",
                "limit": 1,
            }, high
            assert high["usage_window"]["exceeded"] is True, high
            assert high["usage_window"]["used"] > 0, high

            slot = next(agent for agent in agents if agent["id"] == state["slot_id"])
            assert slot["availability"] == {
                "enabled": True,
                "slots": [
                    {
                        "weekday": state["current_weekday"],
                        "start": "00:00",
                        "end": "23:59",
                    }
                ],
            }, slot

            detail = get(f"/api/projects/{state['project1']}")
            saved = state["project1_totals"]
            assert detail["usage"]["totals"] == saved, (detail["usage"]["totals"], saved)
            report = get(f"/api/usage?project_id={state['project1']}")
            assert report["totals"] == saved, (report["totals"], saved)

            print("重启后用量记录、聚合与可用性配置保持")
            print("==> 通过：test_usage_limit.py")


if __name__ == "__main__":
    main()
