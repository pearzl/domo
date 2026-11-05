#!/usr/bin/env python3
"""父子分支闭环与当前验收反馈：父任务提交现场、子任务继承与隔离、
兄弟冲突及里程碑重验、在途子任务重启恢复、分支回收和最终发布；
多轮拒绝替换反馈、通过清空、空清单拒绝保留原因、历史证据可查。
使用脚本化 CLI 与 mock，不调用真实模型或外网。
"""
import sys
from pathlib import Path

from harness import Server, git, init_repo, run, temp_root, wait_for


SCRIPT = r'''
import json, os, pathlib, subprocess, sys, time
control = pathlib.Path(sys.argv[1])
prompt = sys.stdin.read()
job_dir = pathlib.Path(os.environ["DOMO_JOB_DIR"])
context = json.loads((job_dir / "context.json").read_text(encoding="utf-8"))
task = context["task"]
key = task["key"]
role = context["role"]
feedback = "feedback" in context["project"]["goal"]

def gate(name):
    deadline = time.monotonic() + 90
    while not (control / name).exists():
        assert time.monotonic() < deadline, name
        print("waiting " + name, flush=True)
        time.sleep(0.1)

def save(result):
    (job_dir / "result.json").write_text(json.dumps(result), encoding="utf-8")

out = pathlib.Path("domo_mock")
out.mkdir(exist_ok=True)
if feedback:
    n = task["rejections"]
    empty = "empty" in context["project"]["goal"]
    expected = [] if n == 0 else (["EMPTY_REASON"] if empty else
        (["OLD", "KEEP"] if n == 1 else ["KEEP", "NEW"]))
    assert context["verifier_issues"] == expected, context["verifier_issues"]
    if role == "executor":
        if n and not empty:
            gate("feedback-" + str(n))
        if n == 2:
            assert "OLD" not in prompt
        assert "逐项修复并给出对应验证证据" in prompt
        (out / "T1.txt").write_text("T1", encoding="utf-8")
        save({"kind": "done", "summary": "T1", "commit_files": ["domo_mock/T1.txt"]})
    else:
        assert "本轮确认仍未解决的完整缺口" in prompt
        passed = n >= (1 if empty else 2)
        issues = [] if passed or empty else (["OLD", "KEEP"] if n == 0 else ["KEEP", "NEW"])
        result = {"pass": passed, "reason": "EMPTY_REASON" if empty and not passed else "current check", "issues": issues}
        if not passed:
            result.update(next_action="execute", guidance="EMPTY_REASON" if empty else "KEEP")
        result["evidence"] = {
            "contract_id": context["contract"]["id"],
            "artifact_version": context["artifact_version"],
            "artifacts": ["domo_mock/T1.txt"], "risks": [], "human_checks": [],
            "criteria": [{"criterion": criterion, "passed": passed,
                "checks": [{"method": "read domo_mock/T1.txt", "result": (out / "T1.txt").read_text()}]}
                for criterion in task["acceptance"]],
        }
        save(result)
elif not task["parent_id"]:
    if not task["deps"]:
        gate("start-parent")
        (out / "seed.txt").write_text("parent unaccepted seed", encoding="utf-8")
        pathlib.Path("shared.txt").write_text("base\n", encoding="utf-8")
        save({"kind": "derive", "summary": "parent staged input", "commit_files": ["domo_mock/seed.txt", "shared.txt"], "tasks": [
            {"key": "T1.1", "title": "one", "description": "modify shared and write T1.1",
             "acceptance": ["T1.1 delivered"], "deps": []},
            {"key": "T1.2", "title": "two", "description": "modify shared and write T1.2",
             "acceptance": ["T1.2 delivered"], "deps": []},
        ]})
    else:
        assert (out / "T1.1.txt").exists() and (out / "T1.2.txt").exists()
        assert pathlib.Path("shared.txt").read_text() == "one + two\n"
        (out / "T1.txt").write_text("T1 integrated children", encoding="utf-8")
        save({"kind": "done", "summary": "parent integrated children", "commit_files": ["domo_mock/T1.txt"]})
else:
    assert (out / "seed.txt").read_text() == "parent unaccepted seed"
    probe = subprocess.run(["git", "cat-file", "-e", context["project"]["branch"] + ":domo_mock/seed.txt"], capture_output=True)
    assert probe.returncode != 0, "parent input leaked to project branch"
    if key == "T1.2":
        (control / "child-two-started").write_text("1")
        gate("release-child-two")
    else:
        gate("child-two-started")
    status = subprocess.check_output(["git", "status", "--porcelain"], text=True)
    if "UU shared.txt" in status:
        (control / "conflict-seen").write_text("1")
        pathlib.Path("shared.txt").write_text("one + two\n", encoding="utf-8")
    else:
        pathlib.Path("shared.txt").write_text("one\n" if key == "T1.1" else "two\n", encoding="utf-8")
    (out / (key + ".txt")).write_text(key, encoding="utf-8")
    staged = subprocess.check_output(["git", "diff", "--cached", "--name-only", "--no-renames", "-z"]).decode().split("\0")
    files = list(dict.fromkeys(["shared.txt", "domo_mock/" + key + ".txt"] + [p for p in staged if p]))
    save({"kind": "done", "summary": "delivered " + key, "commit_files": files})
'''


def configure(server, script, control, feedback=False):
    call = server.call
    for agent in call("GET", "/api/agents"):
        call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
    call("POST", "/api/agents", {"name": "planner", "kind": "mock", "roles": ["planner"]})
    if not feedback:
        call("POST", "/api/agents", {"name": "verifier", "kind": "mock", "roles": ["verifier"]})
    return call("POST", "/api/agents", {
        "name": "script", "kind": "cli", "roles": ["executor", "verifier"] if feedback else ["executor"],
        "command": [sys.executable, str(script), str(control)], "max_concurrency": 2,
        "enabled": True,
    })


def task(detail, key):
    return next(t for t in detail["tasks"] if t["key"] == key)


def current(server, base, predicate, label):
    def check():
        d = server.call("GET", base)
        return d if predicate(d) else None
    try:
        return wait_for(check, label, 45)
    except AssertionError:
        d = server.call("GET", base)
        print([(t["key"], t["stage"], t["status"], t["last_error"]) for t in d["tasks"]])
        print([(j["role"], j["status"], j["error"]) for j in d["jobs"]])
        raise


def create(server, repo, goal, workspace_type="git"):
    p = server.call("POST", "/api/projects", {
        "name": goal, "goal": "[single] " + goal, "repo_path": str(repo),
        "base_branch": "main", "workspace_type": workspace_type, "confirm_mode": "auto",
    })["project"]
    return f"/api/projects/{p['id']}"


def test_branches(root, script):
    root.mkdir()
    repo = root / "repo"
    init_repo(repo)
    with Server(root, env={"DOMO_MOCK_DELAY_MS": "30"}) as server:
        configure(server, script, root)
        call = server.call
        base = create(server, repo, "parent branches")
        d = current(server, base, lambda d: bool(d["tasks"]), "parent allocated")
        parent_id = task(d, "T1")["id"]
        call("POST", f"{base}/tasks/{parent_id}/milestone", {"milestone": True})
        (root / "start-parent").write_text("1")
        current(server, base, lambda d: (root / "child-two-started").exists(), "child started")
        d = current(server, base, lambda d: any(t["key"] == "T1.1" and t["stage"] == "completed" for t in d["tasks"]), "child one merged")
        parent = task(d, "T1")
        second = task(d, "T1.2")
        first = task(d, "T1.1")
        call("POST", f"{base}/tasks/{second['id']}/milestone", {"milestone": True})
        assert git(["show", parent["branch"] + ":domo_mock/T1.1.txt"], repo) == "T1.1"
        # No parent or child content is published before parent acceptance.
        assert run(["git", "cat-file", "-e", d["project"]["branch"] + ":domo_mock/seed.txt"], cwd=repo, check=False).returncode != 0
        wait_for(lambda: not Path(first["worktree"]).exists(), "completed child checkout released", 15)
        archive = "refs/domo-archive/heads/" + first["branch"]
        assert git(["show", archive + ":domo_mock/seed.txt"], repo) == "parent unaccepted seed"
        assert Path(parent["worktree"]).exists()
        # Restart an in-flight child and rebuild its clean checkout from the
        # saved branch, rather than silently falling back to the project branch.
        server.kill()
        assert git(["status", "--porcelain"], second["worktree"]).strip() == ""
        git(["worktree", "remove", second["worktree"]], repo)
        server.restart()
        (root / "release-child-two").write_text("1")
        d = current(server, base, lambda d: task(d, "T1.2")["human_gate"] == "milestone", "child milestone after restart")
        assert (Path(task(d, "T1.2")["worktree"]) / "domo_mock/seed.txt").exists()
        call("POST", f"{base}/tasks/{second['id']}/milestone/accept", {})
        d = current(server, base, lambda d: (root / "conflict-seen").exists() and task(d, "T1.2")["human_gate"] == "milestone", "conflict resolved and reverified")
        assert task(d, "T1.2")["rejections"] == 0
        assert any(e["kind"] == "merge_conflict" and "父任务分支" in e["message"] for e in d["events"])
        assert task(d, "T1.2")["verifier_issues"] == []
        call("POST", f"{base}/tasks/{second['id']}/milestone/accept", {})
        d = current(server, base, lambda d: task(d, "T1")["human_gate"] == "milestone", "parent integrated and verified")
        assert task(d, "T1.2")["stage"] == "completed"
        assert git(["show", parent["branch"] + ":shared.txt"], repo) == "one + two\n"
        assert run(["git", "cat-file", "-e", d["project"]["branch"] + ":domo_mock/T1.2.txt"], cwd=repo, check=False).returncode != 0
        call("POST", f"{base}/tasks/{parent_id}/milestone/accept", {})
        d = current(server, base, lambda d: bool(d["project"]["report"]), "parent publishes integrated result")
        assert git(["show", d["project"]["branch"] + ":domo_mock/T1.2.txt"], repo) == "T1.2"
        assert git(["show", d["project"]["branch"] + ":shared.txt"], repo) == "one + two\n"
        wait_for(lambda: not Path(parent["worktree"]).exists(), "parent checkout released", 15)


def test_feedback(root, script):
    root.mkdir()
    repo = root / "repo"
    init_repo(repo)
    with Server(root, env={"DOMO_MOCK_DELAY_MS": "30"}) as server:
        configure(server, script, root, feedback=True)
        base = create(server, repo, "feedback")
        d = current(server, base, lambda d: bool(d["tasks"]) and task(d, "T1")["rejections"] == 1, "first feedback")
        assert task(d, "T1")["verifier_issues"] == ["OLD", "KEEP"]
        (root / "feedback-1").write_text("1")
        d = current(server, base, lambda d: task(d, "T1")["rejections"] == 2, "replacement feedback")
        assert task(d, "T1")["verifier_issues"] == ["KEEP", "NEW"]
        (root / "feedback-2").write_text("1")
        d = current(server, base, lambda d: bool(d["project"]["report"]), "passing clears issues")
        t = task(d, "T1")
        assert t["verifier_issues"] == []
        history = server.call("GET", f"{base}/tasks/{t['id']}/verifications")["verifications"]
        assert [v["issues"] for v in history] == [["OLD", "KEEP"], ["KEEP", "NEW"], []]
        assert all(v["evidence"] for v in history)
        base = create(server, repo, "feedback empty")
        d = current(server, base, lambda d: bool(d["project"]["report"]), "empty rejection reason stays actionable")
        assert task(d, "T1")["rejections"] == 1
        assert task(d, "T1")["verifier_issues"] == []
        # The same feedback lifecycle applies to a non-software/plain delivery.
        delivery = root / "research-report"
        delivery.mkdir()
        (delivery / "source.txt").write_text("original research input", encoding="utf-8")
        base = create(server, delivery, "feedback 调研报告", workspace_type="plain")
        d = current(server, base, lambda d: bool(d["project"]["report"]), "plain report feedback converges")
        t = task(d, "T1")
        assert t["rejections"] == 2 and t["verifier_issues"] == []
        history = server.call("GET", f"{base}/tasks/{t['id']}/verifications")["verifications"]
        assert [v["issues"] for v in history] == [["OLD", "KEEP"], ["KEEP", "NEW"], []]
        assert (delivery / "source.txt").read_text(encoding="utf-8") == "original research input"
        assert not (delivery / ".git").exists()


def main():
    with temp_root("domo-branch-feedback-") as root:
        script = root / "agent.py"
        script.write_text(SCRIPT, encoding="utf-8")
        test_branches(root / "branches", script)
        test_feedback(root / "feedback", script)
        print("父子分支隔离、合入冲突、里程碑、重启恢复与当前反馈/历史审计通过")


if __name__ == "__main__":
    main()
