#!/usr/bin/env python3
"""逐代际重规划清理在提交后崩溃时恢复，取消任务的脏工作区直接丢弃。"""

import sqlite3
import time
from pathlib import Path

from harness import Server, ensure_server_built, init_repo, temp_root, wait_for


def wait_crash(server):
    deadline = time.time() + 30
    while server.process.poll() is None and time.time() < deadline:
        time.sleep(0.1)
    assert server.process.poll() is not None, "自动应用后的崩溃钩子未触发"


def main():
    with temp_root("domo-replan-cleanup-queue-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()
        env = {"DOMO_MOCK_DELAY_MS": "70", "DOMO_MOCK_REPLAN_CRASH_AFTER_APPLY": "1"}
        with Server(temp, env=env) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Cleanup Queue Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )
            project = call(
                "POST",
                "/api/projects",
                {"name": "CleanupQueue", "repo_path": str(repo), "base_branch": "main"},
            )["project"]
            pid = project["id"]
            call(
                "POST",
                f"/api/projects/{pid}/analysis/messages",
                {"message": "两轮清理队列测试", "confirm_mode": "auto"},
            )

            def wait_project(predicate, label, timeout=120):
                return wait_for(
                    lambda: (
                        lambda detail: detail if predicate(detail) else None
                    )(call("GET", f"/api/projects/{pid}")),
                    label,
                    timeout,
                )

            detail = wait_project(
                lambda item: any(
                    job["role"] == "verifier" and job["status"] == "running"
                    for job in item["jobs"]
                ),
                "首轮旧任务验收在途",
            )
            old_task = next(task for task in detail["tasks"] if task["worktree"])
            call("POST", f"/api/projects/{pid}/pause", {"force": True})
            call("POST", f"/api/projects/{pid}/replan", {"reason": "第一轮清理"})
            Path(old_task["worktree"], "dirty-replan-cleanup.txt").write_text("dirty")
            call("POST", f"/api/projects/{pid}/resume", {})

            wait_crash(server)
            server.env.pop("DOMO_MOCK_REPLAN_CRASH_AFTER_APPLY")
            server.restart()
            wait_project(lambda item: item["project"]["stage"] == "executing", "第一轮恢复")
            db = sqlite3.connect(temp / "data" / "domo.db")
            done = db.execute(
                "SELECT COUNT(*) FROM replan_cleanup WHERE project_id = ? AND status = 'done'",
                (pid,),
            ).fetchone()[0]
            assert done == 1, done
            assert not Path(old_task["worktree"]).exists()
            db.close()

            server.env["DOMO_MOCK_REPLAN_CRASH_AFTER_APPLY"] = "1"
            server.restart()
            call("POST", f"/api/projects/{pid}/replan", {"reason": "第二轮清理"})
            wait_crash(server)

            db = sqlite3.connect(temp / "data" / "domo.db")
            rows = db.execute(
                "SELECT generation, status FROM replan_cleanup WHERE project_id = ? ORDER BY generation",
                (pid,),
            ).fetchall()
            assert len(rows) == 2 and rows[0][1] == "done" and rows[1][1] == "pending", rows
            worktrees = [
                row[0]
                for row in db.execute(
                    "SELECT DISTINCT worktree FROM tasks WHERE project_id = ? \
                     AND status = 'canceled' AND worktree IS NOT NULL",
                    (pid,),
                )
            ]
            db.close()
            server.env.pop("DOMO_MOCK_REPLAN_CRASH_AFTER_APPLY")
            server.restart()
            wait_project(lambda item: item["project"]["stage"] == "executing", "第二轮恢复")
            db = sqlite3.connect(temp / "data" / "domo.db")
            rows = db.execute(
                "SELECT generation, status FROM replan_cleanup WHERE project_id = ? ORDER BY generation",
                (pid,),
            ).fetchall()
            assert len(rows) == 2 and all(status == "done" for _, status in rows), rows
            unreleased = db.execute(
                "SELECT COUNT(*) FROM tasks WHERE project_id = ? AND status = 'canceled' \
                 AND worktree IS NOT NULL AND (artifacts_released_at IS NULL \
                 OR artifacts_released_at = 'releasing')",
                (pid,),
            ).fetchone()[0]
            db.close()
            assert unreleased == 0, unreleased
            assert all(not Path(worktree).exists() for worktree in worktrees)

            print("两轮应用崩溃后的逐代际恢复与取消工作区丢弃测试通过")


if __name__ == "__main__":
    main()
