#!/usr/bin/env python3
"""合入中断恢复：操作前状态回收、已完成快进补记、脏现场阻塞及重试保护。
使用 mock 和本地 Git；不访问外网，不覆盖外部改动。
"""
import sqlite3
from pathlib import Path

from harness import Server, git, init_repo, temp_root, wait_for


def main():
    with temp_root("domo-merge-recovery-") as root:
        repo = root / "repo"
        init_repo(repo)
        with Server(root, env={"DOMO_MOCK_DELAY_MS": "30"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call("POST", "/api/agents", {"name": "merge recovery", "kind": "mock", "roles": ["planner", "executor", "verifier"]})
            p = call("POST", "/api/projects", {"name": "merge", "repo_path": str(repo),
                "goal": "[direct] 创建报告", "confirm_mode": "auto"})["project"]
            pid = p["id"]
            path = f"/api/projects/{pid}"
            wait_for(lambda: call("GET", path)["project"]["report"], "报告", 20)
            call("POST", path + "/report", {"action": "accept"})
            p = call("GET", path)["project"]
            base = git(["rev-parse", "main"], repo).strip()
            tip = git(["rev-parse", p["branch"]], repo).strip()
            dbpath = server.root / "data" / "domo.db"

            def record(phase, prepared=None):
                with sqlite3.connect(dbpath) as db:
                    db.execute("INSERT OR REPLACE INTO merge_operations(project_id, base_before, project_before, prepared_commit, phase, started_at) VALUES(?, ?, ?, ?, ?, ?)",
                               (pid, base, tip, prepared, phase, "2026-01-01T00:00:00Z"))

            server.stop()
            record("rebasing")
            server.start()
            wait_for(lambda: call("GET", path)["delivery"]["merge_operation"]["phase"] == "rolled_back", "未变更操作回收", 10)
            assert git(["rev-parse", "main"], repo).strip() == base
            assert git(["rev-parse", p["branch"]], repo).strip() == tip

            server.stop()
            record("rebasing")
            checkout = Path(p["workspace"]) / "repo"
            marker = checkout / "user-note.txt"
            marker.write_text("不可覆盖的人工现场", encoding="utf-8")
            server.start()
            wait_for(lambda: call("GET", path)["delivery"]["merge_operation"]["phase"] == "recovery_required", "保留脏现场", 10)
            try:
                call("POST", path + "/merge", {})
            except AssertionError as error:
                assert "未恢复" in str(error), error
            else:
                raise AssertionError("恢复失败时不应允许重试")
            assert marker.read_text(encoding="utf-8") == "不可覆盖的人工现场"
            # 测试创建的现场由测试自身清理，模拟人工核对后的重启恢复。
            marker.unlink()
            server.restart()
            wait_for(lambda: call("GET", path)["delivery"]["merge_operation"]["phase"] == "rolled_back", "人工核对后恢复", 10)

            server.stop()
            record("fast_forwarding", tip)
            git(["merge", "--ff-only", p["branch"]], repo)
            server.start()
            detail = wait_for(lambda: (d if (d := call("GET", path))["delivery"]["merge_operation"]["phase"] == "completed" else None), "快进成功补记", 10)
            assert detail["project"]["merged_commit"] == tip
            assert detail["project"]["stage"] == "completed"
            print("合入中断恢复与现场保护测试通过")


if __name__ == "__main__":
    main()
