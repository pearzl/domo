#!/usr/bin/env python3
"""成果登记协议回归：Git/普通目录都只登记显式成果，省略声明不自动索引。
非法声明在提交前由同作业纠正；纠错耗尽不发布部分索引或 Git 提交。
覆盖目录成果、文件变更清单与成果清单分离，不访问外网或模型。
"""
import sys
from harness import Server, git, init_repo, temp_root, wait_for

SCRIPT = r'''
import json, os, pathlib, sys
prompt = sys.stdin.read()
job_dir = pathlib.Path(os.environ["DOMO_JOB_DIR"])
context = json.loads((job_dir / "context.json").read_text(encoding="utf-8"))
mode = context["project"]["name"]
marker = job_dir / "declaration-attempt"
attempt = int(marker.read_text() if marker.exists() else "0")
marker.write_text(str(attempt + 1))
if attempt:
    assert "artifacts 声明不合法" in prompt and "missing.md" in prompt
output = pathlib.Path("domo_mock/T1.txt")
output.parent.mkdir(exist_ok=True)
output.write_text("T1", encoding="utf-8")
pathlib.Path("helper.txt").write_text("implementation detail", encoding="utf-8")
result = {"kind":"done", "summary":"报告完成", "commit_files":["domo_mock/T1.txt", "helper.txt"]}
if mode != "none":
    result["artifacts"] = [{"path":"domo_mock" if mode == "directory" else "domo_mock/T1.txt", "kind":"delivery"}]
if mode == "invalid-never" or (mode == "invalid-once" and attempt == 0):
    result["artifacts"].append({"path":"missing.md", "kind":"delivery"})
(job_dir / "result.json").write_text(json.dumps(result), encoding="utf-8")
'''


def main():
    with temp_root("domo-artifact-declarations-") as root:
        script = root / "writer.py"
        script.write_text(SCRIPT, encoding="utf-8")
        with Server(root, env={"DOMO_MOCK_DELAY_MS": "20"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call("POST", "/api/agents", {"name": "planner", "kind": "mock", "roles": ["planner", "verifier"]})
            call("POST", "/api/agents", {"name": "writer", "kind": "cli", "roles": ["executor"], "command": [sys.executable, str(script)]})
            for workspace in ["plain", "git"]:
                for mode in ["none", "directory", "invalid-once", "invalid-never"]:
                    directory = root / f"{workspace}-{mode}"
                    if workspace == "git":
                        init_repo(directory)
                    else:
                        directory.mkdir()
                    p = call("POST", "/api/projects", {"name": mode, "workspace_type": workspace,
                        "repo_path": str(directory), "goal": "[single] 生成研究报告", "confirm_mode": "auto"})["project"]
                    base = f"/api/projects/{p['id']}"

                    def settled():
                        detail = call("GET", base)
                        if mode == "invalid-never":
                            return detail if detail["tasks"] and detail["tasks"][0]["status"] == "blocked" else None
                        return detail if detail["project"]["report"] else None

                    detail = wait_for(settled, f"{workspace}/{mode} 成果协议", 30)
                    jobs = [job for job in detail["jobs"] if job["role"] == "executor"]
                    assert len(jobs) == 1, jobs
                    job = call("GET", f"/api/jobs/{jobs[0]['id']}")
                    items = call("GET", base + "/artifacts")["items"]
                    if mode == "invalid-never":
                        assert jobs[0]["failure_kind"] == "protocol", jobs
                        assert "missing.md" in job["error"]
                        assert not items, items
                        if workspace == "git":
                            assert git(["rev-parse", p["branch"]], directory) == git(["rev-parse", "main"], directory)
                    elif mode == "none":
                        assert items == [], items
                    else:
                        expected = "domo_mock" if mode == "directory" else "domo_mock/T1.txt"
                        assert len(items) == 1 and items[0]["path"] == expected, items
                        assert items[0]["kind"] == "delivery" and items[0]["job_id"] == jobs[0]["id"], items
                        if workspace == "git":
                            preview = call("GET", base + "/artifacts/" + items[0]["id"])
                            assert preview["commit"] in preview["versions"]
                    call("POST", base + "/archive?force=true", {})
            print("显式成果、目录登记、无声明与有界协议纠错通过")


if __name__ == "__main__":
    main()
