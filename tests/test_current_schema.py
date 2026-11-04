#!/usr/bin/env python3
"""当前数据结构：空目录初始化、同版本重启、无旧覆盖字段或懒初始化标记。
使用本地服务和 SQLite，不调用模型，不读取或改写用户数据。
"""
import sqlite3

from harness import Server, temp_root


def main():
    with temp_root("domo-current-schema-") as root:
        with Server(root) as server:
            before = server.call("GET", "/api/agents")
            server.restart()
            assert server.call("GET", "/api/agents") == before
        with sqlite3.connect(root / "data" / "domo.db") as db:
            assert db.execute("PRAGMA user_version").fetchone()[0] == 2
            columns = {row[1] for row in db.execute("PRAGMA table_info(tasks)")}
            assert not {"executor_agent_id", "verifier_agent_id"} & columns
            assert not db.execute("SELECT 1 FROM sqlite_master WHERE name = 'task_agent_roles'").fetchall()
            assert "execution_job_id" in {row[1] for row in db.execute("PRAGMA table_info(jobs)")}
            assert db.execute("PRAGMA foreign_key_check").fetchall() == []
    print("current schema ok")


if __name__ == "__main__":
    main()
