#!/usr/bin/env python3
"""逐代际重规划清理队列在多轮 apply 与失败后保持完整。"""

import sqlite3
import time
import urllib.request
from pathlib import Path

from harness import Server, ensure_server_built, git, init_repo, temp_root, wait_for


def confirm_crash(server, pid):
    request = urllib.request.Request(
        server.base + f"/api/projects/{pid}/analysis/confirm",
        data=b"{}",
        method="POST",
        headers={"Content-Type": "application/json"},
    )
    try:
        urllib.request.urlopen(request, timeout=10).read()
    except Exception:
        pass
    deadline = time.time() + 20
    while server.process.poll() is None and time.time() < deadline:
        time.sleep(0.1)
    assert server.process.poll() is not None, "提交后崩溃钩子未触发"


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
            call("POST", f"/api/projects/{pid}/resume", {})
            wait_project(lambda item: item.get("proposal") is not None, "第一轮草稿")
            Path(old_task["worktree"], "dirty-replan-cleanup.txt").write_text("dirty")

            confirm_crash(server, pid)
            server.env.pop("DOMO_MOCK_REPLAN_CRASH_AFTER_APPLY")
            server.restart()
            wait_project(lambda item: item["project"]["stage"] == "executing", "第一轮恢复")
            db = sqlite3.connect(temp / "data" / "domo.db")
            pending = db.execute(
                "SELECT COUNT(*) FROM replan_cleanup WHERE project_id = ? AND status = 'pending'",
                (pid,),
            ).fetchone()[0]
            assert pending == 1, pending
            db.close()

            server.env["DOMO_MOCK_REPLAN_CRASH_AFTER_APPLY"] = "1"
            server.restart()
            call("POST", f"/api/projects/{pid}/replan", {"reason": "第二轮清理"})
            wait_project(lambda item: item.get("proposal") is not None, "第二轮草稿")
            confirm_crash(server, pid)

            db = sqlite3.connect(temp / "data" / "domo.db")
            rows = db.execute(
                "SELECT generation, status FROM replan_cleanup WHERE project_id = ? ORDER BY generation",
                (pid,),
            ).fetchall()
            assert len(rows) == 2 and all(status == "pending" for _, status in rows), rows
            worktrees = [
                row[0]
                for row in db.execute(
                    "SELECT DISTINCT worktree FROM tasks WHERE project_id = ? \
                     AND status = 'canceled' AND worktree IS NOT NULL",
                    (pid,),
                )
            ]
            db.close()
            for worktree in worktrees:
                if Path(worktree).exists():
                    git(["reset", "--hard"], worktree)
                    git(["clean", "-fd"], worktree)

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

            # 草稿期的旧标志只能归属上一代，不能让当前代际提前完成清理。
            call("POST", f"/api/projects/{pid}/replan", {"reason": "草稿期迁移归属"})
            detail = wait_project(lambda item: item.get("proposal") is not None, "迁移前草稿")
            generation = 3
            db = sqlite3.connect(temp / "data" / "domo.db")
            db.execute(
                "UPDATE projects SET replan_cleanup_needed = 1 WHERE id = ?", (pid,)
            )
            db.commit()
            db.close()
            server.restart()
            db = sqlite3.connect(temp / "data" / "domo.db")
            current = db.execute(
                "SELECT COUNT(*) FROM replan_cleanup WHERE project_id = ? AND generation = ?",
                (pid, generation),
            ).fetchone()[0]
            db.close()
            assert current == 0, current
            detail = call("POST", f"/api/projects/{pid}/analysis/confirm", {})
            assert detail["project"]["stage"] == "executing"
            db = sqlite3.connect(temp / "data" / "domo.db")
            status = db.execute(
                "SELECT status FROM replan_cleanup WHERE project_id = ? AND generation = ?",
                (pid, generation),
            ).fetchone()[0]
            db.close()
            assert status == "done", status
            print("两轮 apply 与清理失败后的逐代际恢复测试通过")


if __name__ == "__main__":
    main()
