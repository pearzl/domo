#!/usr/bin/env python3
"""离线备份与校验：SQLite 关联、共享仓库、普通产物、jobs、Git 归档引用、
保留的脏 worktree、运行时锁、篡改检测、拒绝覆盖及不支持恢复命令。
"""
import os
import json
import subprocess
import sqlite3
from pathlib import Path
from harness import Server, SERVER_BIN, git, init_repo, temp_root, wait_for


def main():
    with temp_root("domo-backup-") as root:
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
            cli("--backup", backup, ok=False)
            destination = root / "unsupported-target"
            cli("--restore", backup, destination, ok=False)
            assert not destination.exists()
            manifest = json.loads((backup / "manifest.json").read_text(encoding="utf-8"))

            def saved(path):
                candidates = [entry for entry in manifest["roots"] if Path(path).is_relative_to(entry["original"])]
                entry = max(candidates, key=lambda item: len(Path(item["original"]).parts))
                return backup / entry["relative"] / Path(path).relative_to(entry["original"])

            with sqlite3.connect(backup / "data/domo.db") as db:
                for original in projects:
                    p = original["project"]
                    path, workspace, status = db.execute("SELECT repo_path, workspace, status FROM projects WHERE id = ?", (p["id"],)).fetchone()
                    assert path == p["repo_path"] and workspace == p["workspace"]
                    assert status == "archived"
                    assert (saved(workspace) / "shared/.git").is_dir()
                    assert any((saved(workspace) / "jobs").iterdir())
                    if p["workspace_type"] == "plain":
                        assert (saved(path) / "source.txt").read_text(encoding="utf-8") == "研究资料"
                        assert (saved(path) / "domo_mock/T1.txt").is_file()
                    else:
                        refs = git(["--git-dir=.", "for-each-ref", "refs/domo-archive"], saved(Path(path) / ".git"))
                        assert "domo-task" in refs, refs
                        assert (saved(workspace) / "repo/user-note.txt").read_text(encoding="utf-8") == "归档时保留的人工文件"
            # Corrupt only a test-owned backup copy. Verification must refuse it.
            (backup / "data/domo.db").write_bytes(b"tampered")
            cli("--verify-backup", backup, ok=False)
            print("离线备份、引用与审计完整性、锁及篡改拒绝通过")


if __name__ == "__main__":
    main()
