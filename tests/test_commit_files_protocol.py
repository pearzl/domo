#!/usr/bin/env python3
"""Writing results require file lists; correction is bounded and never stages all.

Uses a scripted local CLI and mock planning/verification in temporary repositories.
"""
import json
import sys
from pathlib import Path

from harness import Server, git, init_repo, temp_root, wait_for


SCRIPT = r'''
import json, os, pathlib, sys
control = pathlib.Path(sys.argv[1])
job = pathlib.Path(os.environ["DOMO_JOB_DIR"])
context = json.loads((job / "context.json").read_text())
marker = job / "invocations"
n = int(marker.read_text()) + 1 if marker.exists() else 1
marker.write_text(str(n))
with (control / "calls").open("a") as log:
    log.write(json.dumps({"job":context["job_id"], "n":n}) + "\n")
sys.stdin.read()
out = pathlib.Path("domo_mock/T1.txt"); out.parent.mkdir(exist_ok=True); out.write_text("T1")
pathlib.Path("scratch.log").write_text("not a delivery")
build = pathlib.Path("build"); build.mkdir(exist_ok=True); (build / "log").write_text("ignored")
result = {"kind":"done", "summary":"complete"}
if (control / "allow").exists() or (sys.argv[2] == "repair" and n > 1):
    result["commit_files"] = ["domo_mock/T1.txt"]
(job / "result.json").write_text(json.dumps(result))
'''


def main():
    with temp_root("domo-commit-files-") as root:
        repo = root / "repo"
        init_repo(repo)
        (repo / ".gitignore").write_text("build/\n")
        git(["add", ".gitignore"], repo)
        git(["-c", "user.name=test", "-c", "user.email=test@localhost", "commit", "-m", "ignore generated files"], repo)
        script = root / "agent.py"
        script.write_text(SCRIPT)
        with Server(root, env={"DOMO_MOCK_DELAY_MS":"10"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", "/api/agents/" + agent["id"], {"enabled":False})
            call("POST", "/api/agents", {"name":"planner", "kind":"mock", "roles":["planner", "verifier"]})
            writer = call("POST", "/api/agents", {"name":"writer", "kind":"cli", "roles":["executor"],
                "command":[sys.executable, str(script), str(root), "repair"]})
            for mode in ["repair", "missing"]:
                control = root / mode; control.mkdir()
                call("PATCH", "/api/agents/" + writer["id"], {"command":[sys.executable, str(script), str(control), mode]})
                project = call("POST", "/api/projects", {"name":mode, "repo_path":str(repo),
                    "goal":"[single] file declaration", "confirm_mode":"auto"})["project"]
                base = "/api/projects/" + project["id"]
                if mode == "missing":
                    detail = wait_for(lambda: d if (d := call("GET", base))["tasks"] and d["tasks"][0]["status"] == "blocked" else None, "invalid file list blocked")
                    jobs = [j for j in detail["jobs"] if j["role"] == "executor"]
                    assert len(jobs) == 1 and jobs[0]["status"] == "failed", jobs
                    assert "commit_files" in jobs[0]["error"] and "次数用尽" in jobs[0]["error"], jobs
                    assert not jobs[0]["can_continue"]
                    calls = [json.loads(line) for line in (control / "calls").read_text().splitlines()]
                    assert len(calls) == 6 and len({item["job"] for item in calls}) == 1, calls
                    assert git(["ls-tree", "-r", "--name-only", project["branch"]], repo).strip() == ".gitignore"
                    (control / "allow").touch()
                    call("POST", "/api/jobs/" + jobs[0]["id"] + "/restart", {})
                detail = wait_for(lambda: d if (d := call("GET", base))["project"]["report"] else None, "declared delivery")
                assert git(["show", project["branch"] + ":domo_mock/T1.txt"], repo).strip() == "T1"
                files = git(["ls-tree", "-r", "--name-only", project["branch"]], repo).splitlines()
                assert "scratch.log" not in files and "build/log" not in files, files
                calls = [json.loads(line) for line in (control / "calls").read_text().splitlines()]
                assert len(calls) == (2 if mode == "repair" else 7), calls
                if mode == "missing":
                    assert calls[-1]["job"] != calls[0]["job"], calls
                worktree = detail["tasks"][0]["worktree"]
                assert Path(worktree, "scratch.log").read_text() == "not a delivery"
                call("POST", base + "/archive?force=true", {})
        print("commit_files protocol correction, exhaustion, exact staging and new-session restart passed")


if __name__ == "__main__":
    main()
