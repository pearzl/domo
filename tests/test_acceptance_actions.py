#!/usr/bin/env python3
"""验收结论与处置分离：Git/普通目录等待、恢复、上级决策和只读报告重写。

覆盖设备缺项等待不返工、不重规划、不增加拒绝计数；重启保留等待；
重复恢复请求不重复派发；验收决策可恢复到执行或验收；重复拒绝只自动决策一次；
首次收尾只有一次调用，非软件交付及直接完成报告重写不重新实施。
"""
import json
import sqlite3
import time

from harness import Server, init_repo, temp_root, wait_for


def configure(server):
    for agent in server.call("GET", "/api/agents"):
        server.call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
    server.call("POST", "/api/agents", {
        "name": "验收团队", "kind": "mock", "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 2,
    })


def create(server, root, mode, goal="[single] 整理一份研究报告"):
    directory = root / "deliverables"
    if mode == "git":
        init_repo(directory)
    else:
        directory.mkdir()
    project = server.call("POST", "/api/projects", {
        "name": "研究报告", "workspace_type": mode, "repo_path": str(directory),
        "base_branch": "main", "goal": goal, "confirm_mode": "auto",
    })["project"]
    return project["id"]


def detail(server, pid, predicate, label="流转完成"):
    def check():
        current = server.call("GET", f"/api/projects/{pid}")
        return current if predicate(current) else None
    return wait_for(check, label, 90)


def settled(current):
    return not any(j["status"] == "running" for j in current["jobs"])


def report(server, pid):
    return detail(server, pid, lambda d: bool(d["project"]["report"]) and settled(d), "项目报告")


def rejects(server, method, path, body=None):
    try:
        server.call(method, path, body)
    except AssertionError as error:
        assert "400" in str(error) or "409" in str(error), error
    else:
        raise AssertionError(f"请求应被拒绝：{path}")


def jobs(current, role):
    return [j for j in current["jobs"] if j["role"] == role]


def task_wait(root, mode):
    root.mkdir()
    with Server(root, env={"DOMO_MOCK_VERIFIER_ACTION_ONCE": "wait"}) as server:
        configure(server)
        pid = create(server, root, mode)
        current = detail(server, pid, lambda d: any(t["stage"] == "verifying" and t["status"] == "blocked" for t in d["tasks"]) and settled(d))
        task = current["tasks"][0]
        assert task["rejections"] == 0, task
        assert len(jobs(current, "executor")) == 1
        server.call("PUT", f"/api/projects/{pid}/limits", {"task_attempt_limit": 1})
        server.restart()
        current = server.call("GET", f"/api/projects/{pid}")
        assert current["tasks"][0]["stage"] == "verifying"
        path = f"/api/projects/{pid}/tasks/{task['id']}/arbitration"
        server.call("POST", f"/api/projects/{pid}/pause", {})
        detail(server, pid, lambda d: d["project"]["status"] == "paused")
        server.call("POST", path, {"action": "retry"})
        rejects(server, "POST", path, {"action": "retry"})
        assert len(jobs(server.call("GET", f"/api/projects/{pid}"), "verifier")) == 1
        server.call("POST", f"/api/projects/{pid}/resume", {})
        current = report(server, pid)
        assert len(jobs(current, "executor")) == 1
        assert len(jobs(current, "verifier")) == 2
        history = server.call("GET", f"/api/projects/{pid}/tasks/{task['id']}/verifications")["verifications"]
        assert history[0]["next_action"] == "wait" and history[0]["guidance"]
        latest = sorted(jobs(current, "verifier"), key=lambda job: job["created_at"])[-1]
        assert "用户补齐设备后重新验收" in server.call("GET", f"/api/jobs/{latest['id']}")["prompt"]
        assert len(jobs(current, "project_acceptance")) == 1
        assert not jobs(current, "summary")
        assert current["tasks"][0]["rejections"] == 0
        if mode == "plain":
            assert not (root / "deliverables" / ".git").exists()


def project_wait(root, action):
    root.mkdir()
    with Server(root, env={"DOMO_MOCK_ACCEPT_ACTION_ONCE": action}) as server:
        configure(server)
        pid = create(server, root, "plain")
        current = detail(server, pid, lambda d: d["project"]["stage"] == "summarizing" and d["project"]["status"] == "blocked" and settled(d))
        assert not current["project"]["report"]
        assert json.loads(current["project"]["acceptance"])["next_action"] == action
        assert not any(e["kind"] == "replan_requested" for e in current["events"])
        server.restart()
        path = f"/api/projects/{pid}/acceptance/retry"
        server.call("PUT", f"/api/projects/{pid}/limits", {"cost_budget": 0})
        rejects(server, "POST", path)
        server.call("PUT", f"/api/projects/{pid}/limits", {})
        server.call("POST", path)
        rejects(server, "POST", path)
        current = report(server, pid)
        assert len(jobs(current, "executor")) == 1
        assert len(jobs(current, "project_acceptance")) == 2
        assert not jobs(current, "summary")


def verifier_review(root, stage):
    root.mkdir()
    with Server(root, env={"DOMO_MOCK_VERIFIER_ACTION_ONCE": "review",
                           "DOMO_MOCK_PLANNER_REPLAN_DECISION": "reject",
                           "DOMO_MOCK_REVIEW_RESUME_STAGE": stage}) as server:
        configure(server)
        pid = create(server, root, "plain")
        current = report(server, pid)
        assert len(jobs(current, "replan_review")) == 1
        assert len(jobs(current, "executor")) == (2 if stage == "executing" else 1)
        assert len(jobs(current, "verifier")) == 2
        assert current["tasks"][0]["rejections"] == 0


def repeated_rejections(root):
    root.mkdir()
    with Server(root, env={"DOMO_MOCK_REJECT_ALWAYS": "1", "DOMO_MOCK_PLANNER_REPLAN_DECISION": "reject"}) as server:
        configure(server)
        pid = create(server, root, "plain")
        current = detail(server, pid, lambda d: any(t["rejections"] >= 4 and t["status"] == "blocked" for t in d["tasks"]) and settled(d))
        assert len(jobs(current, "replan_review")) == 1
        count = len(current["jobs"])
        server.restart()
        time.sleep(0.4)
        assert len(server.call("GET", f"/api/projects/{pid}")["jobs"]) == count
        with sqlite3.connect(root / "data" / "domo.db") as db:
            assert db.execute("SELECT COUNT(*) FROM replan_requests WHERE origin = 'rejections'").fetchone()[0] == 1


def decision_wait(root):
    root.mkdir()
    with Server(root, env={"DOMO_MOCK_VERIFIER_ACTION_ONCE": "review",
                           "DOMO_MOCK_PLANNER_REPLAN_DECISION": "wait"}) as server:
        configure(server)
        pid = create(server, root, "plain")
        current = detail(server, pid, lambda d: bool(jobs(d, "replan_review")) and settled(d))
        task = current["tasks"][0]
        assert task["stage"] == "verifying" and task["rejections"] == 0
        server.restart()
        server.call("POST", f"/api/projects/{pid}/tasks/{task['id']}/arbitration", {
            "action": "guide", "guidance": "设备已连接，检查当前现场并补齐验证",
        })
        current = report(server, pid)
        assert len(jobs(current, "executor")) == 1
        assert len(jobs(current, "verifier")) == 2
        assert len(jobs(current, "replan_review")) == 1


def direct_report_rewrite(root):
    root.mkdir()
    with Server(root) as server:
        configure(server)
        pid = create(server, root, "plain", "[direct] 整理一份简短研究报告")
        current = report(server, pid)
        output = root / "deliverables" / "domo_mock" / "direct.txt"
        before = output.read_bytes(), output.stat().st_mtime_ns
        server.call("POST", f"/api/projects/{pid}/report", {"action": "reject", "feedback": "补充报告使用说明"})
        current = report(server, pid)
        assert len(jobs(current, "direct")) == 1
        assert len(jobs(current, "summary")) == 1
        assert (output.read_bytes(), output.stat().st_mtime_ns) == before
        assert "补充报告使用说明" in current["project"]["report"]


def child_verifier_review(root):
    root.mkdir()
    with Server(root, env={"DOMO_MOCK_DERIVE_ONCE": "1", "DOMO_MOCK_VERIFIER_ACTION_ONCE": "review",
                           "DOMO_MOCK_PARENT_REPLAN_DECISION": "reject"}) as server:
        configure(server)
        pid = create(server, root, "git")
        current = report(server, pid)
        assert len(jobs(current, "replan_review")) == 1
        with sqlite3.connect(root / "data" / "domo.db") as db:
            row = db.execute("SELECT origin, resume_stage, reviewer_task_id FROM replan_requests").fetchone()
            assert row[:2] == ("verifier", "verifying") and row[2], row
        assert all(t["rejections"] == 0 for t in current["tasks"])


def main():
    with temp_root("domo-acceptance-actions-") as root:
        for mode in ["git", "plain"]:
            task_wait(root / mode, mode)
        for action in ["wait", "arbitrate"]:
            project_wait(root / action, action)
        for stage in ["executing", "verifying"]:
            verifier_review(root / stage, stage)
        repeated_rejections(root / "rejections")
        decision_wait(root / "decision-wait")
        direct_report_rewrite(root / "direct")
        child_verifier_review(root / "child")
    print("acceptance actions: ok")


if __name__ == "__main__":
    main()
