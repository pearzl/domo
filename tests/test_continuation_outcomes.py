#!/usr/bin/env python3
"""Automatic evidence gates, result collection, protocol and resume failures.
All agents are scripted local processes or mocks, with temporary Git/plain data.
"""
import json
import sys

from harness import Server, init_repo, temp_root, wait_for


SCRIPT = r'''
import json, os, pathlib, sys, time
control = pathlib.Path(sys.argv[1]); mode = sys.argv[2]; session = sys.argv[3]
sys.stdin.read()
job = pathlib.Path(os.environ["DOMO_JOB_DIR"])
calls = control / "calls"
n = len(calls.read_text().splitlines()) + 1 if calls.exists() else 1
with calls.open("a") as log: log.write(json.dumps({"job":job.name, "session":session}) + "\n")
print(json.dumps({"session_id":session}), flush=True)
out = pathlib.Path("domo_mock/T1.txt"); out.parent.mkdir(exist_ok=True); out.write_text("T1")
if mode == "protocol":
    sys.exit(0)
if mode == "resume_failure":
    if n == 1: sys.exit(1)
    print(json.dumps({"type":"error", "error":{"code":"session_not_found"}}), flush=True)
    sys.exit(1)
if mode == "repeat":
    sys.exit(1)
if mode in ("temporary", "timeout") and n == 1:
    if mode == "temporary":
        print(json.dumps({"type":"error", "error":{"status":503,"message":"unavailable"}}), flush=True)
        sys.exit(1)
    while True:
        print("working", flush=True); time.sleep(0.1)
result = {"kind":"done", "summary":"delivered", "commit_files":["domo_mock/T1.txt"],
    "usage":{"cost":0.25, "input_tokens":10}}
(job / "result.json").write_text(json.dumps(result))
if mode == "collect":
    (control / "ready").touch()
    while True:
        print("waiting after result", flush=True); time.sleep(0.1)
if mode == "valid_exit_error": sys.exit(1)
'''


def current(server, base, predicate, label):
    try:
        return wait_for(lambda: d if predicate(d := server.call("GET", base)) else None, label, 25)
    except AssertionError:
        d = server.call("GET", base)
        print(json.dumps({"project":d["project"], "tasks":d["tasks"], "jobs":d["jobs"]}), flush=True)
        raise


def main():
    with temp_root("domo-continuation-outcomes-") as root:
        for workspace, mode in [("git", "temporary"), ("plain", "temporary"), ("git", "timeout"),
            ("git", "valid_exit_error"), ("git", "collect"), ("plain", "collect"),
            ("git", "protocol"), ("git", "resume_failure"), ("git", "repeat")]:
            print(f"continuation outcome: {workspace}/{mode}", flush=True)
            folder = root / (workspace + "-" + mode); folder.mkdir()
            repo = folder / "repo"; init_repo(repo)
            control = folder / "control"; control.mkdir()
            script = folder / "agent.py"; script.write_text(SCRIPT)
            with Server(folder, env={"DOMO_MOCK_DELAY_MS":"10", "DOMO_JOB_TIMEOUT_SECS":"1" if mode == "timeout" else "30"}) as server:
                call = server.call
                for agent in call("GET", "/api/agents"):
                    call("PATCH", "/api/agents/" + agent["id"], {"enabled":False})
                call("POST", "/api/agents", {"name":"planner", "kind":"mock", "roles":["planner", "verifier"]})
                call("POST", "/api/agents", {"name":"writer", "kind":"cli", "roles":["executor"],
                    "command":[sys.executable, str(script), str(control), mode, "{session_id}"]})
                project = call("POST", "/api/projects", {"name":mode, "repo_path":str(repo), "workspace_type":workspace,
                    "goal":"[single] continuation outcome", "confirm_mode":"auto"})["project"]
                base = "/api/projects/" + project["id"]
                if mode == "collect":
                    wait_for(lambda: (control / "ready").exists(), "result written")
                    server.restart()
                if mode in ("protocol", "resume_failure", "repeat"):
                    d = current(server, base, lambda d: bool(d["tasks"]) and d["tasks"][0]["status"] == "blocked"
                        and all(j["status"] != "running" for j in d["jobs"]), "human decision")
                    job = next(j for j in reversed(d["jobs"]) if j["role"] == "executor")
                    endpoint = "/api/jobs/" + job["id"]
                    # Wait for usage/output settlement before requesting another turn.
                    if mode == "protocol":
                        assert not call("GET", endpoint)["can_continue"]
                        assert "没有写 result.json" in job["error"]
                        assert len((control / "calls").read_text().splitlines()) == 6
                    else:
                        for turn in range(1 if mode == "resume_failure" else 2):
                            wait_for(lambda: call("GET", endpoint)["can_continue"], "settlement completed")
                            call("POST", endpoint + "/continue", {})
                            current(server, base, lambda d: d["tasks"][0]["status"] == "blocked"
                                and len((control / "calls").read_text().splitlines()) == turn + 2, "resumed failure")
                        view = call("GET", endpoint)
                        assert not view["can_continue"], view
                        if mode == "resume_failure":
                            assert view["diagnostic"]["kind"] == "session_unavailable", view
                        history = call("GET", endpoint + "/history")
                        assert len(history["runs"]) == (1 if mode == "resume_failure" else 2), history
                        assert len(history["diagnostics"]) >= 2, history
                else:
                    d = current(server, base, lambda d: bool(d["project"]["report"]), "result settled")
                    jobs = [j for j in d["jobs"] if j["role"] == "executor"]
                    assert len(jobs) == 1 and jobs[0]["status"] == "succeeded", jobs
                    calls = [json.loads(line) for line in (control / "calls").read_text().splitlines()]
                    assert len(calls) == (2 if mode in ("temporary", "timeout") else 1), calls
                    assert len({c["session"] for c in calls}) == 1, calls
                    if mode == "collect":
                        assert jobs[0]["cost"] == 0.25, jobs
                    assert not jobs[0]["can_continue"]
        print("continuation outcome tests passed")


if __name__ == "__main__":
    main()
