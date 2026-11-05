#!/usr/bin/env python3
"""普通目录串行接力与 Git 隔离：当前文件是输入，跨任务修改无需逐路径授权，
失败保留半成品并由后续执行检查接续，重要成果显式登记而非自动枚举。
覆盖非软件报告、登记输入不等于只读、已登记 Git 成果版本更新与只读角色。
"""
import sys
from harness import Server, git, init_repo, temp_root, wait_for

SCRIPT = r'''
import json, os, pathlib
prompt = __import__("sys").stdin.read()
context = json.loads(pathlib.Path(os.environ["DOMO_JOB_DIR"], "context.json").read_text(encoding="utf-8"))
source = pathlib.Path("source.txt")
key = context["task"]["key"]
assert "permissions" not in context["artifacts"]
assert "用户明确" in prompt
if context["project"]["workspace_type"] == "plain":
    assert "所选目录及其内容就是项目输入" in prompt
    assert "不因文件来源任务不同要求逐路径授权" in prompt
if key == "T1":
    assert source.read_text(encoding="utf-8") == "original input"
    source.write_text("first task result", encoding="utf-8")
else:
    assert source.read_text(encoding="utf-8") == "first task result"
    source.write_text("second task result", encoding="utf-8")
output = pathlib.Path("domo_mock", key + ".txt")
output.parent.mkdir(exist_ok=True)
output.write_text(key, encoding="utf-8")
pathlib.Path("unregistered.txt").write_text("not a deliverable", encoding="utf-8")
result = {"kind":"done", "summary":"报告任务完成", "commit_files":["source.txt", str(output).replace("\\", "/")],
    "artifacts":[{"path":str(output).replace("\\", "/"), "kind":"delivery", "format":"text/plain"}],
    "usage":{"cost":0.5, "input_tokens":10, "output_tokens":5}}
if key == "FOLLOWUP":
    result.pop("artifacts")
if key == "FOLLOWUP" and context["project"]["workspace_type"] == "plain" and not pathlib.Path("interrupted.marker").exists():
    pathlib.Path("interrupted.marker").write_text("partial", encoding="utf-8")
    source.write_text("first task result", encoding="utf-8")
    result = {"kind":"failed", "reason":"本轮中断，保留部分报告供下一轮检查", "usage":{"cost":0.5}}
pathlib.Path(os.environ["DOMO_JOB_DIR"], "result.json").write_text(json.dumps(result), encoding="utf-8")
'''


def main():
    with temp_root("domo-workspace-handoff-") as root:
        script = root / "writer.py"
        script.write_text(SCRIPT, encoding="utf-8")
        with Server(root, env={"DOMO_MOCK_DELAY_MS": "30"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call("POST", "/api/agents", {"name": "planner", "kind": "mock", "roles": ["planner", "verifier"]})
            call("POST", "/api/agents", {"name": "writer", "kind": "cli", "roles": ["executor"], "command": [sys.executable, str(script)]})
            for workspace in ["plain", "git"]:
                directory = root / workspace
                if workspace == "git":
                    init_repo(directory)
                else:
                    directory.mkdir()
                source = directory / "source.txt"
                source.write_text("original input", encoding="utf-8")
                if workspace == "git":
                    git(["add", "source.txt"], directory)
                    git(["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-qm", "fixture"], directory)
                p = call("POST", "/api/projects", {"name": "研究报告接力", "workspace_type": workspace,
                    "repo_path": str(directory), "confirm_mode": "auto"})["project"]
                base = f"/api/projects/{p['id']}"
                assert call("GET", base + "/artifacts")["items"] == []
                # Explicit input registration describes a reference, not write protection.
                call("POST", base + "/artifacts", {"path": "source.txt", "kind": "input", "format": "text/plain"})
                call("POST", base + "/analysis/messages", {"message": "[single] 生成研究报告"})
                wait_for(lambda: call("GET", base)["project"]["report"], "首个任务完成", 30)
                original = next(item for item in call("GET", base + "/artifacts")["items"] if item["path"] == "source.txt")
                first_version = original["commit"]
                followup = call("POST", base + "/tasks", {"key": "FOLLOWUP", "title": "复核前序报告", "description": "基于当前资料完善报告", "acceptance": ["报告已复核"]})

                def completed():
                    detail = call("GET", base)
                    return detail if detail["project"]["report"] and any(t["id"] == followup["id"] and t["stage"] == "completed" for t in detail["tasks"]) else None

                detail = wait_for(completed, "跨任务修改与失败后接续", 30)
                items = call("GET", base + "/artifacts")["items"]
                assert {item["path"] for item in items} == {"source.txt", "domo_mock/T1.txt"}, items
                preview = call("GET", base + "/artifacts/" + original["id"])
                assert preview["text"] == "second task result", preview
                if workspace == "plain":
                    jobs = [j for j in detail["jobs"] if j["task_id"] == followup["id"] and j["role"] == "executor"]
                    assert len(jobs) == 2 and jobs[0]["status"] == "failed" and jobs[1]["status"] == "succeeded", jobs
                    assert (directory / "interrupted.marker").exists()
                else:
                    assert source.read_text(encoding="utf-8") == "original input"
                    assert git(["show", p["branch"] + ":source.txt"], directory) == "second task result"
                    assert len(preview["versions"]) == 3 and first_version in preview["versions"], preview
                    assert call("GET", base + "/artifacts/" + original["id"] + "?commit=" + first_version)["text"] == "first task result"
                assert detail["delivery"]["known_cost"] >= 1.0
                for job in detail["jobs"]:
                    if job["role"] in {"analysis", "verifier", "acceptance", "summary"}:
                        prompt = call("GET", f"/api/jobs/{job['id']}")["prompt"]
                        assert "不授予本角色修改交付文件的权限" in prompt
                call("POST", base + "/archive?force=true", {})
            print("普通目录当前现场接力、失败恢复、显式成果与 Git 隔离版本通过")


if __name__ == "__main__":
    main()
