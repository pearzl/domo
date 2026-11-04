#!/usr/bin/env python3
"""重规划分析中断与提交后清理崩溃恢复测试。"""

import sqlite3
import time
import urllib.request
from pathlib import Path

from harness import Server, ensure_server_built, init_repo, temp_root, wait_for


def main():
    with temp_root("domo-safe-replan-recovery-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        def prepare(server):
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Replan Recovery Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )

        def new_project(server, name):
            project = server.call(
                "POST",
                "/api/projects",
                {"name": name, "repo_path": str(repo), "base_branch": "main"},
            )["project"]
            server.call(
                "POST",
                f"/api/projects/{project['id']}/analysis/messages",
                {"message": "中断恢复测试", "confirm_mode": "auto"},
            )
            wait_for(
                lambda: (
                    lambda detail: detail
                    if detail["project"]["stage"] == "executing"
                    and len(detail["tasks"]) == 2
                    else None
                )(server.call("GET", f"/api/projects/{project['id']}")),
                f"{name} 初始计划",
            )
            return project["id"]

        def wait_project(server, project_id, predicate, label, timeout=120):
            def check():
                detail = server.call("GET", f"/api/projects/{project_id}")
                return detail if predicate(detail) else None

            return wait_for(check, label, timeout)

        def analysis_running(server, project_id, minimum):
            return wait_project(
                server,
                project_id,
                lambda detail: len(
                    [job for job in detail["jobs"] if job["role"] == "analysis"]
                ) >= minimum
                and any(
                    job["role"] == "analysis" and job["status"] == "running"
                    for job in detail["jobs"]
                ),
                "重规划分析在途",
            )

        def wait_applied(server, project_id, minimum):
            return wait_project(
                server,
                project_id,
                lambda detail: len(
                    [job for job in detail["jobs"] if job["role"] == "analysis"]
                ) >= minimum
                and detail["project"]["stage"] == "executing"
                and any(
                    event["kind"] == "replan_applied"
                    for event in detail["events"]
                )
                and not any(
                    job["role"] == "analysis" and job["status"] == "running"
                    for job in detail["jobs"]
                ),
                "中断后的重规划自动应用",
            )

        interrupted_root = temp / "interrupted"
        interrupted_root.mkdir()
        with Server(interrupted_root, env={"DOMO_MOCK_DELAY_MS": "1800"}) as server:
            prepare(server)
            paused_id = new_project(server, "ForcedPauseRecovery")
            server.call(
                "POST",
                f"/api/projects/{paused_id}/replan",
                {"reason": "强制暂停后恢复分析"},
            )
            analysis_running(server, paused_id, 2)
            server.call("POST", f"/api/projects/{paused_id}/pause", {"force": True})
            detail = server.call("GET", f"/api/projects/{paused_id}")
            assert detail["project"]["status"] == "paused"
            server.call("POST", f"/api/projects/{paused_id}/resume", {})
            wait_applied(server, paused_id, 3)

            restart_id = new_project(server, "RestartRecovery")
            server.call(
                "POST",
                f"/api/projects/{restart_id}/replan",
                {"reason": "进程重启后恢复分析"},
            )
            analysis_running(server, restart_id, 2)
            server.kill()
            server.restart()
            wait_applied(server, restart_id, 3)
            print("强制暂停与进程重启后的分析恢复通过")

        crash_root = temp / "apply-crash"
        crash_root.mkdir()
        with Server(
            crash_root,
            env={
                "DOMO_MOCK_DELAY_MS": "80",
                "DOMO_MOCK_REJECT_ONCE": "1",
                "DOMO_MOCK_REPLAN_CRASH_AFTER_APPLY": "1",
            },
        ) as server:
            prepare(server)
            pid = new_project(server, "ApplyCrashRecovery")
            detail = wait_project(
                server,
                pid,
                lambda item: any(
                    job["role"] == "verifier" and job["status"] == "running"
                    for job in item["jobs"]
                ),
                "旧任务验收在途",
            )
            old_tasks = detail["tasks"]
            server.call("POST", f"/api/projects/{pid}/pause", {"force": True})
            server.call(
                "POST",
                f"/api/projects/{pid}/replan",
                {"reason": "验证提交后的清理恢复"},
            )
            server.call("POST", f"/api/projects/{pid}/resume", {})
            cancel_candidate = next(
                task
                for task in old_tasks
                if task["stage"] != "completed" and task["worktree"]
            )
            deadline = time.time() + 30
            while server.process.poll() is None and time.time() < deadline:
                time.sleep(0.1)
            assert server.process.poll() is not None, "自动应用后的崩溃钩子未触发"
            server.env.pop("DOMO_MOCK_REPLAN_CRASH_AFTER_APPLY")
            server.restart()
            detail = wait_project(
                server,
                pid,
                lambda item: item["project"]["stage"] == "executing"
                and any(task["status"] != "canceled" for task in item["tasks"]),
                "崩溃后的计划执行",
            )
            restored_old = next(
                task for task in detail["tasks"] if task["id"] == cancel_candidate["id"]
            )
            assert restored_old["status"] == "canceled", restored_old
            assert restored_old["artifacts_released_at"] not in (None, "releasing"), restored_old
            if cancel_candidate["worktree"]:
                assert not Path(cancel_candidate["worktree"]).exists()
            db = sqlite3.connect(crash_root / "data" / "domo.db")
            cleanup = db.execute(
                "SELECT status FROM replan_cleanup WHERE project_id = ? \
                 ORDER BY generation DESC LIMIT 1",
                (pid,),
            ).fetchone()[0]
            db.close()
            assert cleanup == "done", cleanup
            print("提交后崩溃的进程与任务工作区清理恢复通过")


if __name__ == "__main__":
    main()
