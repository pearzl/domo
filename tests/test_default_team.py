#!/usr/bin/env python3
"""DOMO 默认团队配置测试。

覆盖范围：
  1. 空库启动时默认团队按种子 Agent 初始化，三个角色都有条目；
  2. 注册 Agent 按其角色自动进入默认团队，条目追加到角色末尾且编号连续；
  3. 修改 Agent 角色后默认团队条目与角色保持一致，删除全局 Agent 后同步清理并重编号；
  4. PATCH 上下游移（相邻交换、编号连续）与启停行为；
  5. 默认团队只提供排序与启停接口，角色池成员由全局 Agent 角色决定；
  6. 全局 Agent 创建/更新不接收优先级字段，视图不输出优先级；
  7. 新建项目后项目池与默认团队快照一致（角色、顺序、启停）；
  8. 调整默认团队后再次新建项目反映新配置，旧项目池保持不变。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
项目停留在项目分析阶段，不派发任何作业。退出码 0 表示全部通过。
"""

import json
import urllib.error
import urllib.request

from harness import Server, ensure_server_built, init_repo, temp_root


def main():
    with temp_root("domo-default-team-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(temp) as server:

            def call(method, path, data=None):
                return server.call(method, path, data)

            def expect_status(method, path, data, status):
                body = json.dumps(data).encode() if data is not None else None
                request = urllib.request.Request(
                    server.base + path,
                    data=body,
                    method=method,
                    headers={"Content-Type": "application/json"},
                )
                try:
                    with urllib.request.urlopen(request, timeout=60) as response:
                        actual = response.status
                except urllib.error.HTTPError as error:
                    actual = error.code
                assert actual == status, (method, path, status, actual)

            def roles_of(payload):
                return {group["role"]: group["entries"] for group in payload["roles"]}

            def ids(entries):
                return [entry["agent_id"] for entry in entries]

            def priorities(entries):
                return [entry["priority"] for entry in entries]

            def create_project(name, goal):
                # 创建时去掉 goal，目标留待确认阶段提交。
                payload = {"name": name, "repo_path": str(repo), "base_branch": "main"}
                return call("POST", "/api/projects", payload)["project"]

            # 1. 空库启动：默认团队按种子 Agent 初始化，三个角色各有一个启用的条目。
            team = call("GET", "/api/team/default")
            assert "__status__" not in team, team
            assert team["project_id"] == "default", team
            team_roles = roles_of(team)
            assert set(team_roles) == {"planner", "executor", "verifier"}, team_roles
            for role, entries in team_roles.items():
                assert len(entries) == 1, entries
                assert entries[0]["name"] == "OpenCode 默认团队", entries
                assert entries[0]["kind"] == "opencode", entries
                assert entries[0]["enabled"] is True, entries
                assert priorities(entries) == [0], entries

            default_agent_id = team_roles["planner"][0]["agent_id"]
            assert (
                default_agent_id
                == team_roles["executor"][0]["agent_id"]
                == team_roles["verifier"][0]["agent_id"]
            ), team_roles

            # 2. 注册 Agent 自动进入默认团队：按角色追加到末尾，编号连续。
            alpha = call(
                "POST",
                "/api/agents",
                {
                    "name": "Alpha Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )
            assert "__status__" not in alpha, alpha
            assert "priority" not in alpha, "全局 Agent 不应再暴露优先级"

            beta = call(
                "POST",
                "/api/agents",
                {
                    "name": "Beta Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )

            solo = call(
                "POST",
                "/api/agents",
                {"name": "Solo Executor", "kind": "mock", "roles": ["executor"]},
            )

            team = call("GET", "/api/team/default")
            team_roles = roles_of(team)
            for role in ("planner", "verifier"):
                assert ids(team_roles[role]) == [
                    default_agent_id,
                    alpha["id"],
                    beta["id"],
                ], (role, team_roles[role])
                assert priorities(team_roles[role]) == [0, 1, 2], team_roles[role]
                assert all(entry["enabled"] for entry in team_roles[role]), team_roles[role]
            assert ids(team_roles["executor"]) == [
                default_agent_id,
                alpha["id"],
                beta["id"],
                solo["id"],
            ], team_roles["executor"]
            assert priorities(team_roles["executor"]) == [0, 1, 2, 3], team_roles["executor"]
            assert all(entry["enabled"] for entry in team_roles["executor"]), team_roles["executor"]
            assert ids(team_roles["planner"]) == [
                default_agent_id,
                alpha["id"],
                beta["id"],
            ], team_roles["planner"]
            assert ids(team_roles["verifier"]) == [
                default_agent_id,
                alpha["id"],
                beta["id"],
            ], team_roles["verifier"]

            # 默认团队只提供排序与启停接口。
            expect_status(
                "POST",
                "/api/team/default",
                {"role": "planner", "agent_id": alpha["id"]},
                405,
            )
            expect_status(
                "DELETE", f"/api/team/default/planner/{alpha['id']}", None, 405
            )
            expect_status(
                "PATCH",
                f"/api/team/default/planner/{alpha['id']}",
                {"direction": "sideways"},
                400,
            )

            # 3. 上下移调整：相邻交换，编号保持连续。
            call("PATCH", f"/api/team/default/planner/{beta['id']}", {"direction": "up"})
            team = call("GET", "/api/team/default")
            planner_entries = roles_of(team)["planner"]
            assert ids(planner_entries) == [
                default_agent_id,
                beta["id"],
                alpha["id"],
            ], planner_entries
            assert priorities(planner_entries) == [0, 1, 2], planner_entries

            call("PATCH", f"/api/team/default/planner/{beta['id']}", {"direction": "up"})
            team = call("GET", "/api/team/default")
            planner_entries = roles_of(team)["planner"]
            assert ids(planner_entries) == [
                beta["id"],
                default_agent_id,
                alpha["id"],
            ], planner_entries
            assert priorities(planner_entries) == [0, 1, 2], planner_entries

            # 4. 全局禁用默认 Agent，默认团队视图同步显示停用。
            call("PATCH", f"/api/agents/{default_agent_id}", {"enabled": False})
            team = call("GET", "/api/team/default")
            team_roles = roles_of(team)
            for role in ("planner", "executor", "verifier"):
                default_entry = next(
                    entry
                    for entry in team_roles[role]
                    if entry["agent_id"] == default_agent_id
                )
                assert default_entry["enabled"] is False, (role, default_entry)

            # 5. 新建项目的项目池继承默认团队（角色、顺序、启停）。
            project = create_project("Inherit", "项目池继承默认团队")
            pool = call("GET", f"/api/projects/{project['id']}/agents")
            pool_roles = roles_of(pool)
            for role, entries in pool_roles.items():
                expected = team_roles[role]
                assert ids(entries) == ids(expected), (role, entries, expected)
                assert priorities(entries) == priorities(expected), (role, entries, expected)
                assert [entry["enabled"] for entry in entries] == [
                    entry["enabled"] for entry in expected
                ], (role, entries, expected)

            # 在默认团队中停用 Alpha 的执行角色，新项目继续反映；旧项目池保持。
            call("PATCH", f"/api/team/default/executor/{alpha['id']}", {"enabled": False})
            team = call("GET", "/api/team/default")
            team_roles = roles_of(team)
            alpha_executor = next(
                entry
                for entry in team_roles["executor"]
                if entry["agent_id"] == alpha["id"]
            )
            assert alpha_executor["enabled"] is False, team_roles["executor"]

            project2 = create_project("Inherit2", "第二次继承反映默认团队调整")
            pool2 = call("GET", f"/api/projects/{project2['id']}/agents")
            pool2_roles = roles_of(pool2)
            assert ids(pool2_roles["executor"]) == ids(
                team_roles["executor"]
            ), pool2_roles["executor"]
            assert (
                next(
                    entry
                    for entry in pool2_roles["executor"]
                    if entry["agent_id"] == alpha["id"]
                )["enabled"]
                is False
            ), pool2_roles["executor"]

            pool = call("GET", f"/api/projects/{project['id']}/agents")
            assert ids(roles_of(pool)["executor"]) == ids(
                pool2_roles["executor"]
            ), pool
            assert (
                next(
                    entry
                    for entry in roles_of(pool)["executor"]
                    if entry["agent_id"] == alpha["id"]
                )["enabled"]
                is True
            ), "旧项目池不应跟随默认团队变化"

            # 6. 修改 Agent 角色后默认团队同步：角色集合决定条目，新角色条目排在末尾。
            call("PATCH", f"/api/agents/{alpha['id']}", {"roles": ["executor"]})
            team = call("GET", "/api/team/default")
            team_roles = roles_of(team)
            assert ids(team_roles["planner"]) == [
                beta["id"],
                default_agent_id,
            ], team_roles["planner"]
            assert priorities(team_roles["planner"]) == [0, 1], team_roles["planner"]
            assert ids(team_roles["verifier"]) == [
                default_agent_id,
                beta["id"],
            ], team_roles["verifier"]
            assert priorities(team_roles["verifier"]) == [0, 1], team_roles["verifier"]
            assert ids(team_roles["executor"]) == [
                default_agent_id,
                alpha["id"],
                beta["id"],
                solo["id"],
            ], team_roles["executor"]

            project3 = create_project("Inherit3", "角色调整后再次继承")
            pool3 = call("GET", f"/api/projects/{project3['id']}/agents")
            assert ids(roles_of(pool3)["planner"]) == [
                beta["id"],
                default_agent_id,
            ], pool3

            call(
                "PATCH",
                f"/api/agents/{alpha['id']}",
                {"roles": ["planner", "executor", "verifier"]},
            )
            team = call("GET", "/api/team/default")
            team_roles = roles_of(team)
            assert ids(team_roles["planner"]) == [
                beta["id"],
                default_agent_id,
                alpha["id"],
            ], team_roles["planner"]
            assert ids(team_roles["verifier"]) == [
                default_agent_id,
                beta["id"],
                alpha["id"],
            ], team_roles["verifier"]
            assert ids(team_roles["executor"]) == [
                default_agent_id,
                alpha["id"],
                beta["id"],
                solo["id"],
            ], team_roles["executor"]

            # 角色恢复只影响默认团队，不改写已创建的项目池。
            pool3 = call("GET", f"/api/projects/{project3['id']}/agents")
            assert ids(roles_of(pool3)["planner"]) == [
                beta["id"],
                default_agent_id,
            ], pool3

            # 7. 删除全局 Agent 时默认团队同步清理并重新编号。
            call("DELETE", f"/api/agents/{beta['id']}")
            team = call("GET", "/api/team/default")
            team_roles = roles_of(team)
            assert ids(team_roles["planner"]) == [
                default_agent_id,
                alpha["id"],
            ], team_roles["planner"]
            assert priorities(team_roles["planner"]) == [0, 1], team_roles["planner"]
            assert ids(team_roles["executor"]) == [
                default_agent_id,
                alpha["id"],
                solo["id"],
            ], team_roles["executor"]
            assert priorities(team_roles["executor"]) == [0, 1, 2], team_roles["executor"]
            assert ids(team_roles["verifier"]) == [
                default_agent_id,
                alpha["id"],
            ], team_roles["verifier"]

            # 旧项目的池保持快照语义，被删除的全局 Agent 条目标记为不可用。
            pool = call("GET", f"/api/projects/{project3['id']}/agents")
            beta_entry = next(
                entry
                for entry in roles_of(pool)["planner"]
                if entry["agent_id"] == beta["id"]
            )
            assert beta_entry["available"] is False, beta_entry
            assert "删除" in (beta_entry["unavailable_reason"] or ""), beta_entry

            expected_planner = ids(team_roles["planner"])
            project3_planner = ids(roles_of(pool3)["planner"])

            # 重启服务，验证默认团队配置持久化。
            server.restart()

            team = call("GET", "/api/team/default")
            planner = next(group for group in team["roles"] if group["role"] == "planner")
            assert [entry["priority"] for entry in planner["entries"]] == list(
                range(len(planner["entries"]))
            ), planner
            assert ids(planner["entries"]) == expected_planner, planner

            pool = call("GET", f"/api/projects/{project3['id']}/agents")
            planner_pool = next(
                group for group in pool["roles"] if group["role"] == "planner"
            )
            assert [entry["priority"] for entry in planner_pool["entries"]] == list(
                range(len(planner_pool["entries"]))
            ), planner_pool
            assert ids(planner_pool["entries"]) == project3_planner, planner_pool

            print("默认团队初始化、编辑与项目继承测试通过")
            print("默认团队配置重启后保持")


if __name__ == "__main__":
    main()
