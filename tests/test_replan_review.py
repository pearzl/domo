#!/usr/bin/env python3
"""Task owners decide and generate replacement plans in the same control job."""

import json
import sqlite3
from pathlib import Path

from harness import Server, git, init_repo, temp_root, wait_for


def configure(server):
    for agent in server.call("GET", "/api/agents"):
        server.call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
    planner = server.call("POST", "/api/agents", {
        "name": "Planner", "kind": "mock", "roles": ["planner"], "max_concurrency": 2,
    })
    executor = server.call("POST", "/api/agents", {
        "name": "Executor", "kind": "mock", "roles": ["executor", "verifier"], "max_concurrency": 4,
    })
    return planner, executor


def create(server, repo, goal, mode="auto"):
    project = server.call("POST", "/api/projects", {
        "name": goal, "repo_path": str(repo), "base_branch": "main",
    })["project"]
    server.call("POST", f"/api/projects/{project['id']}/analysis/messages", {
        "message": goal, "confirm_mode": mode,
    })
    return project["id"]


def detail(server, project_id, predicate, label):
    def check():
        item = server.call("GET", f"/api/projects/{project_id}")
        return item if predicate(item) else None
    try:
        return wait_for(check, label, timeout=45)
    except AssertionError:
        item = server.call("GET", f"/api/projects/{project_id}")
        print("project:", item["project"], flush=True)
        print("tasks:", [(t["key"], t["stage"], t["status"], t["waiting_reason"], t["last_error"]) for t in item["tasks"]], flush=True)
        print("jobs:", [(j["role"], j["status"], (j["error"] or "")[:250]) for j in item["jobs"]], flush=True)
        print(server.logs()[-5000:], flush=True)
        raise


def request_rows(root):
    with sqlite3.connect(root / "data" / "domo.db") as conn:
        conn.row_factory = sqlite3.Row
        return [dict(row) for row in conn.execute("SELECT * FROM replan_requests ORDER BY created_at")]


def generation(root, project_id):
    with sqlite3.connect(root / "data" / "domo.db") as conn:
        return conn.execute("SELECT replan_generation FROM projects WHERE id = ?", (project_id,)).fetchone()[0]


def test_planner_reject(root, repo):
    with Server(root, env={
        "DOMO_MOCK_REPLAN_ONCE": "1", "DOMO_MOCK_REPLAN_KEY": "T1",
        "DOMO_MOCK_PLANNER_REPLAN_DECISION": "reject", "DOMO_MOCK_REPLAN_REVIEW_DELAY_MS": "2000",
    }) as server:
        planner, _ = configure(server)
        project_id = create(server, repo, "[parallel] planner rejects")
        current = detail(server, project_id, lambda d: any(
            j["role"] == "replan_review" and j["status"] == "running" for j in d["jobs"]
        ), "planner review running")
        assert current["project"]["stage"] == "executing"
        assert current["project"]["replan_active"] == 0
        assert generation(root, project_id) == 0
        assert not any(t["status"] == "canceled" for t in current["tasks"])
        review = next(j for j in current["jobs"] if j["role"] == "replan_review")
        assert review["agent_id"] == planner["id"]
        # Independent tasks continue while the decision is still in flight.
        detail(server, project_id, lambda d: any(
            t["key"] != "T1" and t["stage"] == "completed" for t in d["tasks"]
        ) and any(j["role"] == "replan_review" and j["status"] == "running" for j in d["jobs"]),
               "other tasks finish during review")
        current = detail(server, project_id, lambda d: d["project"]["report"] is not None, "rejected request resumes")
        task = next(t for t in current["tasks"] if t["key"] == "T1")
        assert "处理指导" in task["guidance"]
        assert task["attempts"] == 0 and task["rejections"] == 0
        assert generation(root, project_id) == 0
        assert len([j for j in current["jobs"] if j["role"] == "analysis"]) == 1
        assert request_rows(root)[0]["state"] == "rejected"


def test_project_plan(root, repo):
    with Server(root, env={"DOMO_MOCK_REPLAN_ONCE": "1", "DOMO_MOCK_REPLAN_KEY": "T1"}) as server:
        configure(server)
        project_id = create(server, repo, "[single] planner replaces")
        current = detail(server, project_id, lambda d: d["project"]["report"] is not None, "replacement completes")
        assert generation(root, project_id) == 1
        assert len([j for j in current["jobs"] if j["role"] == "analysis"]) == 1, current["jobs"]
        assert len([j for j in current["jobs"] if j["role"] == "replan_review"]) == 1
        old = min(current["tasks"], key=lambda t: t["seq"])
        assert old["status"] == "canceled"
        assert not Path(old["worktree"]).exists()
        summary_job = next(j for j in current["jobs"] if j["role"] == "project_acceptance")
        workspace = project_id.split("_")[0] + "_" + project_id.split("_")[1][:16]
        summary_dir = root / "ws" / workspace / "jobs" / summary_job["id"]
        context = json.loads((summary_dir / "context.json").read_text(encoding="utf-8"))
        completed = [t for t in current["tasks"] if t["stage"] == "completed" and t["status"] != "canceled"]
        assert len(context["summary_tasks"]) == len(completed), context["summary_tasks"]
        assert len(context["canceled_tasks"]) == 1, context["canceled_tasks"]
        assert "已取消任务" in (summary_dir / "prompt.md").read_text(encoding="utf-8")
        assert request_rows(root)[0]["state"] == "applied"


def test_parent_plan(root, repo):
    with Server(root, env={
        "DOMO_MOCK_DERIVE_ONCE": "1", "DOMO_MOCK_DERIVE_KEY": "T1",
        "DOMO_MOCK_REPLAN_ONCE": "1", "DOMO_MOCK_REPLAN_KEY": "T1.1",
        "DOMO_MOCK_REPLAN_REVIEW_DELAY_MS": "1500",
        "DOMO_MOCK_PARENT_REPLAN_DEPENDENCIES": "1",
    }) as server:
        _, executor = configure(server)
        project_id = create(server, repo, "[parallel] parent replaces children")
        current = detail(server, project_id, lambda d: any(
            e["kind"] == "task_replan_applied" for e in d["events"]
        ), "parent family replanned without dependency deadlock")
        assert generation(root, project_id) == 0
        review = next(j for j in current["jobs"] if j["role"] == "replan_review")
        assert review["agent_id"] == executor["id"]
        context = json.loads((Path(current["project"]["workspace"]) / "jobs" / review["id"] / "context.json").read_text())
        assert context["replan_review"]["reviewer"] == "parent"
        assert context["cwd"] == context["job_dir"]
        assert not any(t["status"] == "canceled" and not t["parent_id"] for t in current["tasks"])
        current = detail(server, project_id, lambda d: d["project"]["report"] is not None, "local replacement completes")
        parent = next(t for t in current["tasks"] if t["key"] == "T1")
        old = next(t for t in current["tasks"] if t["key"] == "T1.1")
        sibling = next(t for t in current["tasks"] if t["key"] == "T1.2")
        replacement = next(t for t in current["tasks"] if t["key"] == "T1.3")
        assert old["status"] == "canceled"
        assert sibling["stage"] == "completed" and sibling["status"] is None
        assert replacement["parent_id"] == parent["id"]
        successor = next(t for t in current["tasks"] if t["key"] == "T1.4")
        assert successor["parent_id"] == parent["id"]
        assert successor["deps"] == ["T1.3"]
        assert "dep T1.3 was merged" in git(["show", f"{current['project']['branch']}:domo_mock/T1.4.txt"], repo)
        for task in (replacement, successor):
            assert any(j["task_id"] == task["id"] and j["role"] == "verifier" and j["status"] == "succeeded" for j in current["jobs"])
        assert "T1.1" not in parent["deps"] and "T1.3" in parent["deps"]
        assert len([j for j in current["jobs"] if j["role"] == "replan_review"]) == 1
        assert len([j for j in current["jobs"] if j["role"] == "analysis"]) == 1


def test_wait_and_retry(root, repo):
    env = {"DOMO_MOCK_REPLAN_ONCE": "1", "DOMO_MOCK_REPLAN_KEY": "T1",
           "DOMO_MOCK_PLANNER_REPLAN_DECISION": "wait"}
    with Server(root, env=env) as server:
        configure(server)
        project_id = create(server, repo, "[single] missing resource")
        current = detail(server, project_id, lambda d: any(e["kind"] == "replan_review_waiting" for e in d["events"]), "resource wait")
        task = next(t for t in current["tasks"] if t["key"] == "T1")
        assert task["status"] == "blocked" and task["attempts"] == 0
        assert generation(root, project_id) == 0
        server.call("POST", f"/api/projects/{project_id}/tasks/{task['id']}/arbitration", {
            "action": "guide", "guidance": "资源已补齐，按原任务执行",
        })
        detail(server, project_id, lambda d: d["project"]["report"] is not None, "human resolves resource wait")
        assert request_rows(root)[0]["state"] == "resolved"


def test_manual_plan(root, repo):
    with Server(root, env={"DOMO_MOCK_REPLAN_ONCE": "1", "DOMO_MOCK_REPLAN_KEY": "T1"}) as server:
        configure(server)
        project_id = create(server, repo, "[single] manual replacement", mode="manual")
        detail(server, project_id, lambda d: d["proposal"] is not None, "initial manual plan")
        server.call("POST", f"/api/projects/{project_id}/analysis/confirm", {})
        current = detail(server, project_id, lambda d: d["project"]["replan_active"] == 1 and d["proposal"] is not None, "replacement manual draft")
        assert not any(t["status"] == "canceled" for t in current["tasks"])
        assert len([j for j in current["jobs"] if j["role"] == "analysis"]) == 1
        assert request_rows(root)[0]["state"] == "ready"
        server.call("POST", f"/api/projects/{project_id}/analysis/confirm", {})
        detail(server, project_id, lambda d: d["project"]["report"] is not None, "manual replacement finishes")
        wait_for(lambda: request_rows(root)[0]["state"] == "applied", "manual request finalized")


def test_review_restart(root, repo):
    with Server(root, env={
        "DOMO_MOCK_REPLAN_ONCE": "1", "DOMO_MOCK_REPLAN_KEY": "T1",
        "DOMO_MOCK_REPLAN_REVIEW_DELAY_MS": "5000", "DOMO_MOCK_PLANNER_REPLAN_DECISION": "reject",
    }) as server:
        configure(server)
        project_id = create(server, repo, "[single] restart review")
        detail(server, project_id, lambda d: any(j["role"] == "replan_review" and j["status"] == "running" for j in d["jobs"]), "decision in flight")
        server.kill()
        server.env["DOMO_MOCK_REPLAN_REVIEW_DELAY_MS"] = "100"
        server.restart()
        current = detail(server, project_id, lambda d: d["project"]["report"] is not None, "decision recovered")
        assert generation(root, project_id) == 0
        assert len([j for j in current["jobs"] if j["role"] == "replan_review"]) == 2
        assert next(t for t in current["tasks"] if t["key"] == "T1")["attempts"] == 0
        assert request_rows(root)[0]["attempts"] == 1


def test_apply_crash(root, repo):
    with Server(root, env={
        "DOMO_MOCK_REPLAN_ONCE": "1", "DOMO_MOCK_REPLAN_KEY": "T1",
        "DOMO_MOCK_REPLAN_CRASH_AFTER_APPLY": "1",
    }) as server:
        configure(server)
        project_id = create(server, repo, "[single] crash after replacement")
        wait_for(lambda: server.process.poll() is not None, "replacement apply crash")
        server.env.pop("DOMO_MOCK_REPLAN_CRASH_AFTER_APPLY")
        server.restart()
        current = detail(server, project_id, lambda d: d["project"]["report"] is not None, "applied plan recovered")
        assert generation(root, project_id) == 1
        assert len([j for j in current["jobs"] if j["role"] == "analysis"]) == 1
        assert len([j for j in current["jobs"] if j["role"] == "replan_review"]) == 1
        assert request_rows(root)[0]["state"] == "applied"
        assert all(t["artifacts_released_at"] for t in current["tasks"] if t["status"] == "canceled")


def test_parent_reject(root, repo):
    with Server(root, env={
        "DOMO_MOCK_DERIVE_ONCE": "1", "DOMO_MOCK_DERIVE_KEY": "T1",
        "DOMO_MOCK_REPLAN_ONCE": "1", "DOMO_MOCK_REPLAN_KEY": "T1.1",
        "DOMO_MOCK_PARENT_REPLAN_DECISION": "reject",
    }) as server:
        _, executor = configure(server)
        project_id = create(server, repo, "[single] parent rejects")
        current = detail(server, project_id, lambda d: d["project"]["report"] is not None, "parent guides child")
        assert generation(root, project_id) == 0
        assert not any(t["status"] == "canceled" for t in current["tasks"])
        child = next(t for t in current["tasks"] if t["key"] == "T1.1")
        assert "父任务重规划审批" in child["guidance"]
        assert next(j for j in current["jobs"] if j["role"] == "replan_review")["agent_id"] == executor["id"]


def test_abandon_draft(root, repo):
    with Server(root, env={"DOMO_MOCK_REPLAN_ONCE": "1", "DOMO_MOCK_REPLAN_KEY": "T1"}) as server:
        configure(server)
        project_id = create(server, repo, "[single] abandon replacement", mode="manual")
        detail(server, project_id, lambda d: d["proposal"] is not None, "initial draft")
        server.call("POST", f"/api/projects/{project_id}/analysis/confirm", {})
        detail(server, project_id, lambda d: d["project"]["replan_active"] == 1 and d["proposal"] is not None, "replacement draft")
        server.call("POST", f"/api/projects/{project_id}/replan/abandon", {})
        current = detail(server, project_id, lambda d: d["project"]["report"] is not None, "original plan restored")
        assert not any(t["status"] == "canceled" for t in current["tasks"])
        assert request_rows(root)[0]["state"] == "resolved"
        assert len([j for j in current["jobs"] if j["role"] == "replan_review"]) == 1


def main():
    with temp_root("domo-replan-review-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        for name, test in [
            ("reject", test_planner_reject), ("project", test_project_plan),
            ("parent", test_parent_plan), ("wait", test_wait_and_retry),
            ("manual", test_manual_plan), ("restart", test_review_restart),
            ("apply-crash", test_apply_crash),
            ("parent-reject", test_parent_reject), ("abandon", test_abandon_draft),
        ]:
            root = temp / name
            root.mkdir()
            test(root, repo)
            print(f"replan review {name}: passed", flush=True)


if __name__ == "__main__":
    main()
