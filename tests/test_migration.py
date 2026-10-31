#!/usr/bin/env python3
"""旧数据库迁移测试。

覆盖范围：
  1. 目标确认/规划阶段合并为项目分析阶段；
  2. confirm/goal 作业角色合并为 analysis；
  3. goal_confirm_mode / goal_proposal / goal_confirmed_at 列重命名为
     confirm_mode / proposal / confirmed_at；
  4. 旧 ready 方案被清空，已确认时间保持不变；
  5. 旧 agents 表缺少 allow_priority_fallback 时由迁移补齐，列保持
     INTEGER NOT NULL DEFAULT 0；
  6. 历史 Agent 行在迁移后读取开关值为 0。

约定：只启动一次服务完成迁移，不派发作业；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
"""

import sqlite3

from harness import Server, ensure_server_built, temp_root


def main():
    with temp_root("domo-migration-") as temp:
        data_dir = temp / "data"
        data_dir.mkdir(parents=True, exist_ok=True)
        db_path = data_dir / "domo.db"

        db = sqlite3.connect(db_path)
        db.executescript(
            """
CREATE TABLE agents (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL,
    model TEXT NOT NULL DEFAULT '', roles TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    max_concurrency INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    command_json TEXT, model_args_json TEXT,
    cooldown_until TEXT, last_error TEXT,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE projects (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, goal TEXT NOT NULL,
    repo_path TEXT NOT NULL, base_branch TEXT NOT NULL, branch TEXT NOT NULL,
    workspace TEXT NOT NULL, stage TEXT NOT NULL, status TEXT, blocked_reason TEXT,
    report TEXT, report_feedback TEXT, replan_reason TEXT, replan_source TEXT,
    replan_requested_at TEXT, goal_confirm_mode TEXT NOT NULL DEFAULT 'manual',
    goal_proposal TEXT, goal_confirmed_at TEXT,
    completion_mode TEXT NOT NULL DEFAULT 'tasks', merged_at TEXT, merged_commit TEXT,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
    role TEXT NOT NULL, agent_id TEXT, status TEXT NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 0, prompt TEXT NOT NULL,
    result TEXT, stdout TEXT, stderr TEXT, error TEXT, exit_code INTEGER, pid INTEGER,
    failure_kind TEXT, input_tokens INTEGER, output_tokens INTEGER,
    cache_read_tokens INTEGER, cache_write_tokens INTEGER, reasoning_tokens INTEGER,
    cost REAL, duration_ms INTEGER, usage_known INTEGER NOT NULL DEFAULT 0,
    started_at TEXT, finished_at TEXT, created_at TEXT NOT NULL
);
INSERT INTO projects VALUES
 ('p1','OldGoal','g1','/tmp/repo','main','b1','/tmp/w1','goal_confirm','blocked','等待用户答复或确认目标',NULL,NULL,NULL,NULL,NULL,'manual','{"kind":"ready","summary":"s","goal":"g1","assumptions":[],"acceptance":[],"plan":[],"verification":[],"skip_confirmation":false}',NULL,'tasks',NULL,NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
 ('p2','OldPlanning','g2','/tmp/repo','main','b2','/tmp/w2','planning','paused',NULL,NULL,NULL,'why',NULL,'2026-01-01T00:00:00Z','auto',NULL,'2026-01-01T00:00:00Z','tasks',NULL,NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
 ('p3','OldExecuting','g3','/tmp/repo','main','b3','/tmp/w3','executing','paused',NULL,NULL,NULL,NULL,NULL,NULL,'manual',NULL,'2026-01-01T00:00:00Z','tasks',NULL,NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
INSERT INTO jobs (id, project_id, role, status, prompt, created_at) VALUES
 ('j1','p1','confirm','succeeded','x','2026-01-01T00:00:00Z'),
 ('j2','p2','goal','succeeded','x','2026-01-01T00:00:00Z');
INSERT INTO agents (id, name, kind, model, roles, priority, max_concurrency, enabled, command_json, model_args_json, created_at, updated_at) VALUES
 ('agt_legacy','Legacy','opencode','','["planner","executor","verifier"]',0,1,1,NULL,NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
"""
        )
        db.commit()
        db.close()

        ensure_server_built()

        with Server(temp) as server:
            # 启动成功即完成迁移；随后优雅停止，再直接检查数据库。
            server.interrupt()

        db = sqlite3.connect(db_path)
        columns = {row[1] for row in db.execute("PRAGMA table_info('projects')")}
        assert {"confirm_mode", "proposal", "confirmed_at"} <= columns, columns
        assert not (
            {"goal_confirm_mode", "goal_proposal", "goal_confirmed_at"} & columns
        ), columns

        rows = {
            row[0]: row[1:]
            for row in db.execute(
                "SELECT id, stage, confirm_mode, proposal, confirmed_at FROM projects ORDER BY id"
            )
        }
        assert rows["p1"][0] == "analysis", rows
        assert rows["p1"][1] == "manual", rows
        assert rows["p1"][2] is None, rows
        assert rows["p1"][3] is None, rows
        assert rows["p2"][0] == "analysis", rows
        assert rows["p2"][1] == "auto", rows
        assert rows["p2"][3] == "2026-01-01T00:00:00Z", rows
        assert rows["p3"][0] == "executing", rows

        roles = {row[0]: row[1] for row in db.execute("SELECT id, role FROM jobs")}
        assert roles == {"j1": "analysis", "j2": "analysis"}, roles

        agent_columns = {row[1]: row for row in db.execute("PRAGMA table_info('agents')")}
        assert "allow_priority_fallback" in agent_columns, agent_columns.keys()
        column = agent_columns["allow_priority_fallback"]
        assert column[2].upper() == "INTEGER", column
        assert column[3] == 1, column
        assert column[4] in ("0", 0), column
        historical = db.execute(
            "SELECT allow_priority_fallback FROM agents WHERE id = 'agt_legacy'"
        ).fetchone()
        assert historical is not None, historical
        assert historical[0] == 0, historical

        db.close()

        print("旧数据库迁移通过")


if __name__ == "__main__":
    main()
