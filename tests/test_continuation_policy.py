#!/usr/bin/env python3
"""Reason-based manual continuation and independent dispatch gates.

Scripted local CLI fixtures use isolated Git/plain workspaces and temporary data.
"""
import json
import sqlite3
import sys
from pathlib import Path

from harness import Server, init_repo, temp_root, wait_for


SCRIPT = r'''
import json, os, pathlib, sys, time
control = pathlib.Path(sys.argv[1])
session = sys.argv[2]
prompt = sys.stdin.read()
job = pathlib.Path(os.environ["DOMO_JOB_DIR"])
context = json.loads((job / "context.json").read_text())
if context["role"] == "analysis":
    (job / "result.json").write_text(json.dumps({"kind":"direct", "summary":"direct plan",
        "plan":["write artifact"], "verification":["check artifact"], "skip_confirmation":True}))
    sys.exit(0)
with (control / "calls.jsonl").open("a") as out:
    out.write(json.dumps({"session":session, "job":context["job_id"], "prompt":prompt}) + "\n")
print(json.dumps({"session_id":session}), flush=True)
partial = pathlib.Path("partial.txt")
if not (control / "failed").exists():
    partial.write_text("preserve this workspace")
    (control / "started").touch()
    while not (control / "release").exists():
        if not (control / "stall").exists():
            print("working", flush=True)
        time.sleep(0.05)
    (control / "failed").touch()
    error = (control / "error").read_text() if (control / "error").exists() else "unknown failure"
    print(error, flush=True)
    sys.exit(1)
assert partial.read_text() == "preserve this workspace"
out = pathlib.Path("domo_mock/T1.txt")
out.parent.mkdir(exist_ok=True); out.write_text("T1")
result = {"kind":"done", "summary":"delivered",
    "commit_files":["domo_mock/T1.txt"], "usage":{"cost":0.5, "input_tokens":20}}
if context["role"] == "direct":
    result.update(kind="direct_done", report="# Delivered")
(job / "result.json").write_text(json.dumps(result))
'''


def denied(server, path):
    try:
        server.call("POST", path, {})
    except AssertionError as error:
        assert "-> 409:" in str(error) or "-> 400:" in str(error), error
    else:
        raise AssertionError("operation should be rejected: " + path)


def fixture(root, project_job=False, case="continue"):
    root.mkdir()
    repo = root / "repo"
    init_repo(repo)
    control = root / "control"
    control.mkdir()
    if case in ("auth", "quota", "config", "session"):
        code = {"auth":"invalid_api_key", "quota":"insufficient_quota", "config":"model_not_found", "session":"session_not_found"}[case]
        (control / "error").write_text(json.dumps({"type":"error", "error":{"code":code}}))
    if case == "stall":
        (control / "stall").touch()
    script = root / "agent.py"
    script.write_text(SCRIPT)
    server = Server(root, env={"DOMO_MOCK_DELAY_MS": "10", "DOMO_STALL_SECS":"1"}).start()
    call = server.call
    for agent in call("GET", "/api/agents"):
        call("PATCH", "/api/agents/" + agent["id"], {"enabled": False})
    if not project_job:
        call("POST", "/api/agents", {"name":"planner", "kind":"mock", "roles":["planner", "verifier"]})
    agent = call("POST", "/api/agents", {"name":"writer", "kind":"cli", "roles":["planner"] if project_job else ["executor"],
        "command":[sys.executable, str(script), str(control), "{session_id}"]})
    project = call("POST", "/api/projects", {"name":"continuation fixture", "repo_path":str(repo), "base_branch":"main", "workspace_type":"plain" if case == "plain" else "git"})["project"]
    base = "/api/projects/" + project["id"]
    call("POST", base + "/analysis/messages", {"message":"[single] continuation fixture", "confirm_mode":"auto"})
    wait_for(lambda: (control / "started").exists(), "first process started")
    if case == "restart":
        server.stop()
        (control / "failed").touch()
        server.start()
    elif case.startswith("force"):
        call("POST", base + "/pause", {"force":True})
        (control / "failed").touch()
        call("POST", base + "/resume", {})
    elif case == "stall":
        wait_for(lambda: call("GET", base)["tasks"][0]["status"] == "blocked", "stalled process settled")
        (control / "failed").touch()
    else:
        (control / "release").touch()
    wait_for(lambda: call("GET", base)["project"]["status"] == "blocked", "interruption settled")
    detail = call("GET", base)
    task = None if project_job else detail["tasks"][0]
    job = next(j for j in reversed(detail["jobs"]) if j["role"] == ("direct" if project_job else "executor"))
    assert job["status"] == ("canceled" if case.startswith("force") else "failed"), job
    return server, base, agent, task, job, control


def main():
    with temp_root("domo-continuation-policy-") as root:
        for case in (sys.argv[1:] or ["continue", "project", "guide", "plain", "dirty", "auth", "quota", "config", "session", "queued_input", "budget", "force", "force_restart", "force_project_restart", "restart", "stall"]):
            print("continuation case: " + case, flush=True)
            project_job = case in ("project", "force_project_restart")
            server, base, agent, task, job, control = fixture(root / case, project_job, case)
            try:
                call = server.call
                endpoint = "/api/jobs/" + job["id"]
                view = call("GET", endpoint)
                if case == "session":
                    assert not view["can_continue"]
                    assert view["diagnostic"]["kind"] == "session_unavailable", view
                    denied(server, endpoint + "/continue")
                    continue
                view = wait_for(lambda: v if (v := call("GET", endpoint))["can_continue"] else None, "previous run settled", 10)
                assert view["can_continue"], (case, view)
                if case == "dirty":
                    Path(task["worktree"], "external.txt").write_text("external change")
                if case == "config":
                    call("PATCH", "/api/agents/" + agent["id"], {"model":"different"})
                    assert call("GET", endpoint)["can_continue"]
                if case == "budget":
                    call("PUT", base + "/limits", {"task_attempt_limit":1})
                    denied(server, endpoint + "/continue")
                    continue
                if case == "queued_input":
                    # Hold the original agent by its periodic availability gate.
                    server.stop()
                    with sqlite3.connect(server.root / "data/domo.db") as db:
                        db.execute("UPDATE agents SET cooldown_until='2099-01-01T00:00:00Z' WHERE id=?", (agent["id"],))
                    server.start()
                    call("POST", endpoint + "/continue", {})
                    Path(task["worktree"], "external.txt").write_text("changed while queued")
                    queued = call("GET", base)
                    assert queued["tasks"][0]["status"] == "queued", queued
                    assert len((control / "calls.jsonl").read_text().splitlines()) == 1
                    server.stop()
                    with sqlite3.connect(server.root / "data/domo.db") as db:
                        db.execute("UPDATE agents SET cooldown_until=NULL WHERE id=?", (agent["id"],))
                    server.start()
                elif case == "guide":
                    call("POST", base + "/tasks/" + task["id"] + "/arbitration", {"action":"guide", "guidance":"inspect existing files"})
                elif case in ("force_restart", "force_project_restart"):
                    call("POST", endpoint + "/restart", {})
                else:
                    if task:
                        call("PUT", base + "/limits", {"task_attempt_limit":2})
                    call("POST", endpoint + "/continue", {})
                try:
                    detail = wait_for(lambda: completed(call, base, project_job), "completed with preserved workspace", 20)
                except AssertionError:
                    print(json.dumps(call("GET", base), ensure_ascii=False), flush=True)
                    raise
                calls = [json.loads(line) for line in (control / "calls.jsonl").read_text().splitlines()]
                assert len(calls) == 2, calls
                if case in ("guide", "force_restart", "force_project_restart"):
                    assert calls[0]["job"] != calls[1]["job"] and calls[0]["session"] != calls[1]["session"], calls
                    if case == "guide":
                        assert "inspect existing files" in calls[1]["prompt"]
                    else:
                        assert call("GET", endpoint)["status"] == "canceled"
                        assert not call("GET", endpoint)["can_continue"]
                else:
                    assert calls[0]["job"] == calls[1]["job"] and calls[0]["session"] == calls[1]["session"], calls
                    if task and case not in ("force", "restart", "stall"):
                        assert detail["tasks"][0]["attempts"] == 1, detail["tasks"]
                    assert len(call("GET", endpoint + "/history")["runs"]) == 1
                if case in ("auth", "quota", "config"):
                    assert view["diagnostic"]["evidence"] == "structured", view
                    assert calls[1]["session"] == calls[0]["session"]
            finally:
                server.stop()
        print("continuation policy tests passed")


def completed(call, base, project_job=False):
    detail = call("GET", base)
    return detail if (detail["project"]["report"] if project_job else detail["tasks"][0]["stage"] == "completed") else None


if __name__ == "__main__":
    main()
