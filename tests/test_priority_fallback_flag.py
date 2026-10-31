#!/usr/bin/env python3
"""Agent 并发已满回退开关的数据层与 API 测试。

覆盖范围：
  1. 新建库的 agents 表含 allow_priority_fallback，类型 INTEGER、NOT NULL、默认 0；
  2. 旧库缺少该列时启动后由迁移补齐，既有 Agent 取默认值 0；
  3. GET /api/agents 的每个元素带布尔 allow_priority_fallback，默认 false；
  4. PATCH 与 POST 可读写该开关；
  5. 默认团队、项目池的项目级与任务级视图同步该值，Agent 删除后条目不崩溃。

约定：只启动服务与创建项目，不派发作业；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
"""

import sqlite3

from harness import Server, call, ensure_server_built, git, init_repo, run, temp_root


def agent_flag(entry):
    value = entry.get("allow_priority_fallback")
    assert isinstance(value, bool), f"allow_priority_fallback 不是布尔值: {entry!r}"
    return value


def pool_entries(pool):
    result = {}
    for group in pool["roles"]:
        for entry in group["entries"]:
            result[entry["agent_id"]] = entry
    return result


def check_column(db_path):
    db = sqlite3.connect(db_path)
    try:
        rows = db.execute("PRAGMA table_info('agents')").fetchall()
    finally:
        db.close()
    columns = {row[1]: row for row in rows}
    assert "allow_priority_fallback" in columns, columns.keys()
    row = columns["allow_priority_fallback"]
    kind, notnull, default = row[2], row[3], row[4]
    assert kind.upper() == "INTEGER", row
    assert notnull == 1, row
    assert default in ("0", 0), row


def check_fresh_server(temp):
    (temp / "fresh").mkdir(parents=True, exist_ok=True)
    with Server(temp / "fresh") as server:
        check_column(temp / "fresh" / "data" / "domo.db")

        agents = server.call("GET", "/api/agents")
        assert agents, "默认团队应至少有一个 Agent"
        for agent in agents:
            assert agent_flag(agent) is False, agent

        target = agents[0]["id"]
        patched = server.call("PATCH", f"/api/agents/{target}", {"allow_priority_fallback": True})
        assert agent_flag(patched) is True, patched
        fetched = next(item for item in server.call("GET", "/api/agents") if item["id"] == target)
        assert agent_flag(fetched) is True, fetched

        # 其他字段的 PATCH 不应重置开关。
        renamed = server.call("PATCH", f"/api/agents/{target}", {"name": fetched["name"]})
        assert agent_flag(renamed) is True, renamed

        restored = server.call("PATCH", f"/api/agents/{target}", {"allow_priority_fallback": False})
        assert agent_flag(restored) is False, restored

        created = server.call(
            "POST",
            "/api/agents",
            {
                "name": "回退开关 Agent",
                "kind": agents[0]["kind"],
                "roles": ["executor"],
                "command": agents[0]["command"],
                "allow_priority_fallback": True,
            },
        )
        assert agent_flag(created) is True, created
        default_created = server.call(
            "POST",
            "/api/agents",
            {
                "name": "缺省开关 Agent",
                "kind": agents[0]["kind"],
                "roles": ["executor"],
                "command": agents[0]["command"],
            },
        )
        assert agent_flag(default_created) is False, default_created

        expected = {agent["id"]: agent_flag(agent) for agent in server.call("GET", "/api/agents")}
        team = server.call("GET", "/api/team/default")
        team_entries = pool_entries(team)
        for agent_id, value in expected.items():
            assert agent_id in team_entries, agent_id
            assert agent_flag(team_entries[agent_id]) is value, team_entries[agent_id]

        with temp_root("domo-fallback-repo-") as repo_root:
            repo = repo_root / "repo"
            repo.mkdir()
            init_repo(repo)
            git(["remote", "add", "origin", str(repo)], repo)
            project = server.call(
                "POST",
                "/api/projects",
                {"name": "回退开关项目", "repo_path": str(repo)},
            )
            project_id = project["project"]["id"]
            project_agents = server.call("GET", f"/api/projects/{project_id}/agents")
            project_entries = pool_entries(project_agents)
            for agent_id, value in expected.items():
                assert agent_id in project_entries, agent_id
                assert agent_flag(project_entries[agent_id]) is value, project_entries[agent_id]

            with temp_root("domo-fallback-task-") as task_root:
                task_repo = task_root / "repo"
                task_repo.mkdir()
                init_repo(task_repo)
                git(["remote", "add", "origin", str(task_repo)], task_repo)
                task_project = server.call(
                    "POST",
                    "/api/projects",
                    {"name": "回退开关任务", "repo_path": str(task_repo)},
                )
                task_project_id = task_project["project"]["id"]
                executor_ids = {
                    entry["agent_id"]
                    for group in project_agents["roles"]
                    for entry in group["entries"]
                    if group["role"] == "executor"
                }
                for index, agent_id in enumerate(sorted(executor_ids)):
                    task = server.call(
                        "POST",
                        f"/api/projects/{task_project_id}/tasks",
                        {
                            "key": f"PF{index}",
                            "title": f"开关任务 {index}",
                            "description": "验证任务池开关",
                            "deps": [],
                        },
                    )
                    task_agents = server.call(
                        "GET", f"/api/projects/{task_project_id}/tasks/{task['id']}/agents"
                    )
                    for entry in pool_entries(task_agents).values():
                        assert agent_flag(entry) is expected[entry["agent_id"]], entry

            # 删除 Agent 后对应条目缺失，其余条目仍可读取。
            server.call("DELETE", f"/api/agents/{created['id']}")
            team_after = pool_entries(server.call("GET", "/api/team/default"))
            assert created["id"] not in team_after
            project_after = pool_entries(
                server.call("GET", f"/api/projects/{project_id}/agents")
            )
            assert project_after, "项目池不应为空"
            for entry in project_after.values():
                flag = agent_flag(entry)
                if entry["agent_id"] == created["id"]:
                    assert entry["unavailable_reason"], entry
                else:
                    assert flag is expected[entry["agent_id"]], entry


def check_legacy_migration(temp):
    root = temp / "legacy"
    data_dir = root / "data"
    data_dir.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(data_dir / "domo.db")
    try:
        db.executescript(
            """
CREATE TABLE agents (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    model TEXT NOT NULL DEFAULT '',
    roles TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    max_concurrency INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    command_json TEXT,
    model_args_json TEXT,
    cooldown_until TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
INSERT INTO agents (id, name, kind, model, roles, priority, max_concurrency, enabled, \
 command_json, model_args_json, created_at, updated_at) VALUES
 ('agt_legacy', 'Legacy', 'opencode', '', '["planner","executor","verifier"]', 0, 1,
  1, NULL, NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
"""
        )
        db.commit()
    finally:
        db.close()

    with Server(root) as server:
        check_column(data_dir / "domo.db")
        agents = server.call("GET", "/api/agents")
        legacy = next(item for item in agents if item["id"] == "agt_legacy")
        assert agent_flag(legacy) is False, legacy


def main():
    ensure_server_built()
    with temp_root("domo-fallback-") as temp:
        check_legacy_migration(temp)
        check_fresh_server(temp / "server")
    print("priority fallback flag ok")


if __name__ == "__main__":
    main()