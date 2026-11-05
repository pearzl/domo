#!/usr/bin/env python3
"""普通目录原地覆盖保护与 Git worktree 写入边界：精确授权、失败费用、
文件现场检查和中断恢复。脚本化执行者改写输入，验证普通目录的检测而非
沙箱/自动回滚语义，以及 Git 项目不要求额外逐路径授权且主工作树保持不变。
"""
import sys
from harness import Server, git, init_repo, temp_root, wait_for

SCRIPT = r'''
import json, os, pathlib, sys
prompt = sys.stdin.read()
context = json.loads(pathlib.Path(os.environ["DOMO_JOB_DIR"], "context.json").read_text(encoding="utf-8"))
if context["project"]["workspace_type"] == "git":
    assert "无需额外逐路径覆盖授权" in prompt
    assert "未经授权的原始输入必须保留" not in prompt
    assert "缺少覆盖授权时停止对应修改" not in prompt
else:
    assert "修改自己任务的产物" in prompt
    assert "permissions 中授权给当前任务的精确路径" in prompt
pathlib.Path("source.txt").write_text("agent-written input", encoding="utf-8")
output = pathlib.Path("domo_mock/T1.txt")
output.parent.mkdir(exist_ok=True)
output.write_text("T1", encoding="utf-8")
result = {"kind":"done", "summary":"updated source", "commit_files":["source.txt", "domo_mock/T1.txt"], "usage":{"cost":0.5, "input_tokens":10, "output_tokens":5}}
pathlib.Path(os.environ["DOMO_JOB_DIR"], "result.json").write_text(json.dumps(result), encoding="utf-8")
'''


def main():
    with temp_root("domo-workspace-protection-") as root:
        script = root / "writer.py"
        script.write_text(SCRIPT, encoding="utf-8")
        with Server(root, env={"DOMO_MOCK_DELAY_MS": "30"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call("POST", "/api/agents", {"name": "planner", "kind": "mock", "roles": ["planner", "verifier"]})
            call("POST", "/api/agents", {"name": "writer", "kind": "cli", "roles": ["executor"], "command": [sys.executable, str(script)]})
            for permitted in [False, True]:
                directory = root / ("authorized" if permitted else "protected")
                directory.mkdir()
                source = directory / "source.txt"
                source.write_text("original input", encoding="utf-8")
                p = call("POST", "/api/projects", {"name": directory.name, "workspace_type": "plain", "repo_path": str(directory), "goal": "[single] 生成研究报告", "confirm_mode": "manual"})["project"]
                base = f"/api/projects/{p['id']}"
                wait_for(lambda: call("GET", base)["proposal"], "方案草稿", 15)
                call("PATCH", base + "/analysis/policy", {"mode": "manual"})
                # The task identity is allocated at confirmation. Suspend the
                # executor until the explicit permission is attached.
                executor = next(a for a in call("GET", "/api/agents") if a["name"] == "writer")
                call("PATCH", f"/api/agents/{executor['id']}", {"enabled": False})
                detail = call("POST", base + "/analysis/confirm", {})
                task = detail["tasks"][0]
                if permitted:
                    call("POST", base + "/workspace/permissions", {"path": "source.txt", "task_id": task["id"], "reason": "允许报告任务原地修正源资料"})
                call("PATCH", f"/api/agents/{executor['id']}", {"enabled": True})
                def settled():
                    d = call("GET", base)
                    if permitted:
                        return d if d["project"]["report"] else None
                    return d if any(t["last_failure_kind"] == "workspace" for t in d["tasks"]) else None
                try:
                    detail = wait_for(settled, "授权成功或覆盖阻塞", 30)
                except AssertionError as error:
                    raise AssertionError(f"{error}; permitted={permitted}; detail={call('GET', base)}") from error
                assert source.read_text(encoding="utf-8") == "agent-written input"
                assert detail["delivery"]["known_cost"] >= 0.5, detail["delivery"]
                check = call("GET", base + "/artifacts")["check"]
                assert check["interrupted"] is (not permitted), check
                if not permitted:
                    server.restart()
                    assert call("GET", base + "/artifacts")["check"]["interrupted"]
                    call("POST", base + "/workspace/inspect", {"reason": "已核对未授权覆盖，保留当前内容，不自动重试"})
                    assert not call("GET", base + "/artifacts")["check"]["interrupted"]
                call("POST", base + "/archive?force=true", {})

            repo = root / "git-repo"
            init_repo(repo)
            (repo / "source.txt").write_text("original input", encoding="utf-8")
            git(["add", "source.txt"], repo)
            git(["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-qm", "fixture"], repo)
            p = call("POST", "/api/projects", {"name": "Git 原地修改", "repo_path": str(repo), "base_branch": "main", "goal": "[single] 更新输入", "confirm_mode": "auto"})["project"]
            base = f"/api/projects/{p['id']}"
            detail = wait_for(lambda: (d if (d := call("GET", base))["project"]["report"] else None), "Git 无逐路径授权完成", 30)
            assert all(task["stage"] == "completed" for task in detail["tasks"]), detail
            assert call("GET", base + "/artifacts")["permissions"] == []
            assert (repo / "source.txt").read_text(encoding="utf-8") == "original input"
            assert git(["show", p["branch"] + ":source.txt"], repo) == "agent-written input"
            for job in detail["jobs"]:
                if job["role"] in {"analysis", "verifier", "acceptance", "summary"}:
                    prompt = call("GET", f"/api/jobs/{job['id']}")["prompt"]
                    assert "不授予本角色修改交付文件的权限" in prompt
                    assert "只能修改自己任务的产物" not in prompt
            call("POST", base + "/archive", {})
            print("普通工作区覆盖授权与恢复、Git 隔离修改和只读角色提示通过")


if __name__ == "__main__":
    main()
