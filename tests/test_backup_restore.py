#!/usr/bin/env python3
"""真实离线备份/异路径恢复：SQLite 关联、共享仓库、普通产物、jobs、
Git 归档引用和保留的脏 worktree；运行时锁、篡改检测及拒绝覆盖已有目录。
"""
import os
import subprocess
import sqlite3
from pathlib import Path
from harness import Server, SERVER_BIN, git, init_repo, temp_root, wait_for


def main():
    with temp_root("domo-backup-restore-") as root:
        repo = root / "repo"
        init_repo(repo)
        plain = root / "research"
        plain.mkdir()
        (plain / "source.txt").write_text("研究资料", encoding="utf-8")
        with Server(root, env={"DOMO_MOCK_DELAY_MS": "40"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call("POST", "/api/agents", {"name": "backup", "kind": "mock", "roles": ["planner", "executor", "verifier"]})
            projects = []
            for directory, kind in [(repo, "git"), (plain, "plain")]:
                p = call("POST", "/api/projects", {"name": "backup-" + kind, "repo_path": str(directory), "workspace_type": kind,
                    "goal": "[single] 形成可核对的研究报告", "confirm_mode": "auto"})["project"]
                base = f"/api/projects/{p['id']}"
                wait_for(lambda: call("GET", base)["project"]["report"], "项目报告", 30)
                call("POST", base + "/shared", [{"topic": "报告证据", "body": "资料已核对", "evidence": ["domo_mock/T1.txt"]}])
                call("POST", base + "/report", {"action": "accept"})
                if kind == "git":
                    delivery = Path(p["workspace"]) / "repo"
                    (delivery / "user-note.txt").write_text("归档时保留的人工文件", encoding="utf-8")
                call("POST", base + "/archive?force=true", {})
                projects.append(call("GET", base))
            env = os.environ.copy()
            env.update({"DOMO_DATA_ROOT": str(server.root / "data"), "DOMO_WORKSPACE_ROOT": str(server.root / "ws"), "DOMO_LOG_ROOT": str(server.root / "log")})
            backup = root / "snapshot"
            def cli(*args, ok=True):
                result = subprocess.run([str(SERVER_BIN), *map(str, args)], env=env, capture_output=True, text=True, encoding="utf-8")
                if ok:
                    assert result.returncode == 0, result.stdout + result.stderr
                else:
                    assert result.returncode != 0, result.stdout
                return result
            locked = cli("--backup", backup, ok=False)
            assert "停止服务" in locked.stderr, locked.stderr
            server.stop()
            print(cli("--backup", backup).stdout.strip())
            print(cli("--verify-backup", backup).stdout.strip())
            restored = root / "restored"
            print(cli("--restore", backup, restored).stdout.strip())
            cli("--restore", backup, restored, ok=False)
            with sqlite3.connect(restored / "data/domo.db") as db:
                for original in projects:
                    p = original["project"]
                    path, workspace, status = db.execute("SELECT repo_path, workspace, status FROM projects WHERE id = ?", (p["id"],)).fetchone()
                    assert str(restored) in path and str(restored) in workspace
                    assert status == "archived"
                    assert (Path(workspace) / "shared/.git").is_dir()
                    assert any((Path(workspace) / "jobs").iterdir())
                    if p["workspace_type"] == "plain":
                        assert (Path(path) / "source.txt").read_text(encoding="utf-8") == "研究资料"
                        assert (Path(path) / "domo_mock/T1.txt").is_file()
                    else:
                        refs = git(["for-each-ref", "refs/domo-archive"], Path(path))
                        assert "domo-task" in refs, refs
                        assert (Path(workspace) / "repo/user-note.txt").read_text(encoding="utf-8") == "归档时保留的人工文件"
                        git(["status", "--porcelain"], Path(workspace) / "repo")
            with Server(restored, env={"DOMO_WORKSPACE_ROOT": str(restored / "trees/workspace")}) as recovered:
                for original in projects:
                    base = f"/api/projects/{original['project']['id']}"
                    assert recovered.call("GET", base)["project"]["status"] == "archived"
                    assert recovered.call("GET", base + "/shared")["records"]
                    assert recovered.call("GET", base + "/artifacts")["items"]
            # Corrupt only a test-owned backup copy. Verification must refuse it.
            (backup / "data/domo.db").write_bytes(b"tampered")
            cli("--verify-backup", backup, ok=False)
            print("备份异路径恢复、引用与审计完整性、锁及篡改拒绝通过")


if __name__ == "__main__":
    main()
