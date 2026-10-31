#!/usr/bin/env python3
"""暂停项目重启恢复测试。

场景：项目暂停请求已提交（暂停中），重启服务前项目中仍有阻塞任务或等待人工
验收的任务。验收标准：

  1. 重启恢复不会把暂停中的项目改写成阻塞；
  2. 恢复完成后项目保持已暂停，前端继续显示“恢复”入口；
  3. 阻塞任务及其等待状态在重启后保持。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
"""

import sqlite3

from harness import Server, init_repo, temp_root, wait_for


def create_project(server, repo, name):
    return server.call(
        "POST",
        "/api/projects",
        {
            "name": name,
            "repo_path": str(repo),
            "base_branch": "main",
        },
    )["project"]["id"]


def pause_in_flight(db_path, project_id, human_gate=None):
    """把项目写成暂停中，并留下一个阻塞的未完成任务。"""
    db = sqlite3.connect(db_path)
    timestamp = "2026-01-01T00:00:00Z"
    db.execute(
        "UPDATE projects SET stage = 'executing', status = 'pausing', updated_at = ? "
        "WHERE id = ?",
        (timestamp, project_id),
    )
    db.execute(
        "INSERT INTO tasks (id, project_id, seq, queue_order, key, title, description, "
        "acceptance, deps, stage, status, human_gate, last_error, created_at, updated_at) "
        "VALUES (?, ?, 1, 1, 'T1', '阻塞任务', '等待外部处理', '[]', '[]', "
        "'executing', 'blocked', ?, '需要人工处理', ?, ?)",
        (f"tsk_{project_id}", project_id, human_gate, timestamp, timestamp),
    )
    db.commit()
    db.close()


def main():
    with temp_root("domo-pause-restart-") as temp:
        repo = temp / "repo"
        init_repo(repo)

        with Server(temp) as server:
            blocked_id = create_project(server, repo, "Blocked")
            gated_id = create_project(server, repo, "Gated")

        db_path = temp / "data" / "domo.db"
        pause_in_flight(db_path, blocked_id)
        pause_in_flight(db_path, gated_id, "milestone")

        with Server(temp) as server:
            def projects_paused():
                states = {}
                for project_id in (blocked_id, gated_id):
                    detail = server.call("GET", f"/api/projects/{project_id}")
                    states[project_id] = detail["project"]["status"]
                if all(status == "paused" for status in states.values()):
                    return states
                return None

            states = wait_for(projects_paused, "暂停中的项目恢复为已暂停", 30)
            assert states == {
                blocked_id: "paused",
                gated_id: "paused",
            }, states

            detail = server.call("GET", f"/api/projects/{blocked_id}")
            task = detail["tasks"][0]
            assert task["status"] == "blocked", task
            assert task["last_error"] == "需要人工处理", task

            detail = server.call("GET", f"/api/projects/{gated_id}")
            task = detail["tasks"][0]
            assert task["status"] == "blocked", task
            assert task["human_gate"] == "milestone", task

        print("暂停项目重启恢复测试通过")


if __name__ == "__main__":
    main()
