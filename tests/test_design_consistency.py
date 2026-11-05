#!/usr/bin/env python3
"""交付控制边界回归（mock 与标准库 CLI，不访问模型或外网）。

覆盖：应用重规划后的运行时失败结算；Git/普通目录和直接完成/任务模式的
报告重写保留暂停；任务重试保留暂停且不能替代里程碑验收；项目验收/总结的
永久错误换人及一次性跳过清理；里程碑合并冲突退回执行、重新双重验收且不计拒绝；
token/费用独立覆盖；手工登记 Git 产物版本、预览、归档和重启保留。
"""
import sqlite3
import sys
import time
from pathlib import Path

from harness import Server, git, init_repo, temp_root, wait_for


def wait_detail(call, base, predicate, label):
    try:
        return wait_for(lambda: d if predicate(d := call("GET", base)) else None, label, 30)
    except AssertionError as error:
        detail = call("GET", base)
        raise AssertionError(f"{error}; project={detail['project']}; tasks={detail['tasks']}; jobs={detail['jobs']}") from error


def team(call):
    for agent in call("GET", "/api/agents"):
        call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
    return call("POST", "/api/agents", {"name": "mock", "kind": "mock",
        "roles": ["planner", "executor", "verifier"], "max_concurrency": 3})["id"]


def reject(call, method, path, data):
    try:
        call(method, path, data)
    except AssertionError:
        return
    raise AssertionError(f"request must fail: {method} {path}")


def test_pause(root):
    repo = root / "repo"
    init_repo(repo)
    with Server(root, env={"DOMO_MOCK_DELAY_MS": "40"}) as server:
        call = server.call
        team(call)
        for workspace in ("git", "plain"):
            for mode in ("single", "direct"):
                p = call("POST", "/api/projects", {"name": f"{workspace}-{mode}",
                    "workspace_type": workspace,
                    "repo_path": str(repo if workspace == "git" else root / mode),
                    "goal": f"[{mode}] 生成研究报告", "confirm_mode": "auto"})["project"]
                base = f"/api/projects/{p['id']}"
                wait_detail(call, base, lambda d: bool(d["project"]["report"]), "报告")
                call("POST", base + "/pause", {})
                paused = wait_detail(call, base, lambda d: d["project"]["status"] == "paused", "暂停")
                job_count = len(paused["jobs"])
                call("POST", base + "/report", {"action": "reject", "feedback": "补充报告结论"})
                time.sleep(0.25)
                d = call("GET", base)
                assert d["project"]["status"] == "paused", d["project"]
                assert d["project"]["report"] is None and len(d["jobs"]) == job_count
                call("POST", base + "/resume", {})
                wait_detail(call, base, lambda d: bool(d["project"]["report"]), "恢复后重写")
                call("POST", base + "/archive", {})

        # A task retry is queued under a paused project, never a project resume.
        server.env["DOMO_MOCK_FAIL_ALWAYS"] = "1"
        server.restart()
        p = call("POST", "/api/projects", {"name": "retry-pause", "repo_path": str(repo),
            "goal": "[single] 任务失败", "confirm_mode": "auto"})["project"]
        base = f"/api/projects/{p['id']}"
        d = wait_detail(call, base, lambda d: any(t["status"] == "blocked" for t in d["tasks"]), "失败阻塞")
        tid = d["tasks"][0]["id"]
        call("POST", base + "/pause", {})
        d = wait_detail(call, base, lambda d: d["project"]["status"] == "paused", "任务重试前暂停")
        count = len(d["jobs"])
        retry_job = next(j for j in reversed(d["jobs"]) if j["role"] == "executor")
        call("POST", base + f"/tasks/{tid}/retry", {})
        time.sleep(0.25)
        d = call("GET", base)
        assert d["project"]["status"] == "paused" and len(d["jobs"]) == count
        server.env.pop("DOMO_MOCK_FAIL_ALWAYS")
        server.restart()
        assert call("GET", base)["project"]["status"] == "paused"
        call("POST", base + "/resume", {})
        d = wait_detail(call, base, lambda d: bool(d["project"]["report"]), "任务恢复完成")
        assert next(j for j in d["jobs"] if j["id"] == retry_job["id"])["status"] == "failed"
        assert any(j["role"] == "executor" and j["status"] == "succeeded" and j["id"] != retry_job["id"] for j in d["jobs"])
        assert call("GET", f"/api/jobs/{retry_job['id']}/history")["runs"] == []


def test_runtime_replan(root):
    repo = root / "repo"
    init_repo(repo)
    with Server(root, env={"DOMO_MOCK_DELAY_MS": "50", "DOMO_MOCK_RUNTIME_FAIL_ROLE": "executor",
        "DOMO_MOCK_FAILURE_CALLBACK_DELAY_MS": "300"}) as server:
        call = server.call
        aid = team(call)
        p = call("POST", "/api/projects", {"name": "runtime-replan", "repo_path": str(repo),
            "goal": "[single] 写入报告", "confirm_mode": "manual"})["project"]
        base = f"/api/projects/{p['id']}"
        wait_detail(call, base, lambda d: d["proposal"] is not None, "首次方案")
        call("PATCH", base + f"/agents/executor/{aid}", {"enabled": False})
        call("POST", base + "/analysis/confirm", {})
        call("POST", base + "/replan", {"reason": "细化任务"})
        wait_detail(call, base, lambda d: d["project"]["replan_active"] == 1 and d["proposal"] is not None, "重规划草稿")
        applied = call("POST", base + "/analysis/confirm", {})
        for task in applied["tasks"]:
            if task["status"] != "canceled":
                call("PATCH", base + f"/tasks/{task['id']}/agents/executor/{aid}", {"enabled": True})
        call("PATCH", base + f"/agents/executor/{aid}", {"enabled": True})
        d = wait_detail(call, base, lambda d: any(j["role"] == "executor" and j["status"] == "failed" for j in d["jobs"]), "新计划运行时失败结算")
        assert d["project"]["replan_active"] == 0
        assert not any(j["role"] == "executor" and j["status"] == "canceled" for j in d["jobs"])
        assert any(e["kind"] == "job_failed" for e in d["events"])
        call("POST", base + "/pause", {"force": True})
        server.env.pop("DOMO_MOCK_RUNTIME_FAIL_ROLE")
        server.restart()
        call("POST", base + "/resume", {})
        wait_detail(call, base, lambda d: bool(d["project"]["report"]), "异常恢复交付")


PLANNER = r'''
import json, os, pathlib, sys
jd = pathlib.Path(os.environ["DOMO_JOB_DIR"])
c = json.loads((jd / "context.json").read_text(encoding="utf-8"))
role = c["role"]
if "fail-" + role in c["project"]["goal"]:
    sys.stderr.write("permanent error: invalid API key\n")
    sys.exit(1)
elif role == "analysis":
    r = {"kind":"plan", "summary":"生成报告", "acceptance":["报告存在"],
        "skip_confirmation":True, "tasks":[{"key":"T1", "title":"写报告", "description":"write domo_mock/T1.txt",
        "acceptance":["domo_mock/T1.txt exists"], "deps":[]}]}
elif role == "project_acceptance":
    r = {"kind":"project_acceptance", "contract_id":c["contract"]["id"], "artifact_version":c["artifact_version"],
        "criteria":[{"criterion":"报告存在", "verdict":"met", "evidence":["检查 domo_mock/T1.txt 文件存在"]}]}
else:
    r = {"kind":"report", "summary":"报告就绪"}
(jd / "result.json").write_text(json.dumps(r), encoding="utf-8")
'''


def test_project_switch(root):
    repo = root / "repo"
    init_repo(repo)
    script = root / "planner.py"
    script.write_text(PLANNER, encoding="utf-8")
    with Server(root, env={"DOMO_MOCK_DELAY_MS": "40"}) as server:
        call = server.call
        team(call)
        for agent in call("GET", "/api/agents"):
            call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
        primary = call("POST", "/api/agents", {"name": "primary", "kind": "cli", "roles": ["planner"],
            "command": [sys.executable, str(script)]})["id"]
        backup = call("POST", "/api/agents", {"name": "backup", "kind": "mock", "roles": ["planner"]})["id"]
        call("POST", "/api/agents", {"name": "worker", "kind": "mock", "roles": ["executor", "verifier"]})
        for role in ("project_acceptance", "summary"):
            p = call("POST", "/api/projects", {"name": role, "repo_path": str(repo),
                "goal": f"[single] fail-{role}", "confirm_mode": "auto"})["project"]
            base = f"/api/projects/{p['id']}"
            d = wait_detail(call, base, lambda d: bool(d["project"]["report"]), "永久错误自动换人")
            jobs = [j for j in d["jobs"] if j["role"] == role]
            assert [(j["agent_id"], j["status"]) for j in jobs] == [(primary, "failed"), (backup, "succeeded")], jobs
            with sqlite3.connect(root / "data" / "domo.db") as db:
                assert db.execute("SELECT avoid_agent_id, avoid_agent_role FROM projects WHERE id = ?", (p["id"],)).fetchone() == (None, None)
            if role == "summary":
                call("POST", base + "/report", {"action": "reject", "feedback": "再核对一次"})
                d = wait_detail(call, base, lambda d: bool(d["project"]["report"]) and len([j for j in d["jobs"] if j["role"] == role]) == 4, "下一轮从最高优先级开始")
                assert [j["agent_id"] for j in d["jobs"] if j["role"] == role] == [primary, backup, primary, backup]


def test_milestone_conflict_and_artifact(root):
    repo = root / "repo"
    init_repo(repo)
    (repo / "source.txt").write_text("original input", encoding="utf-8")
    git(["add", "."], repo)
    git(["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-qm", "source"], repo)
    with Server(root, env={"DOMO_MOCK_DELAY_MS": "40", "DOMO_MOCK_EXECUTOR_DELAY_MS": "500"}) as server:
        call = server.call
        team(call)
        p = call("POST", "/api/projects", {"name": "milestone", "repo_path": str(repo)})["project"]
        base = f"/api/projects/{p['id']}"
        for _ in range(2):
            assets = call("POST", base + "/artifacts", {"path": "source.txt", "kind": "input"})
        artifact = next(a for a in assets["items"] if a["path"] == "source.txt")
        preview = call("GET", base + "/artifacts/" + artifact["id"])
        assert preview["text"] == "original input" and preview["versions"] == [artifact["commit"]]
        reject(call, "POST", base + "/artifacts", {"path": "missing.txt", "kind": "delivery"})
        reject(call, "GET", base + "/artifacts/" + artifact["id"] + "?commit=" + "0" * 40, None)
        call("POST", base + "/analysis/messages", {"message": "[single] 生成报告"})
        wait_detail(call, base, lambda d: d["proposal"] is not None, "里程碑方案")
        d = call("POST", base + "/analysis/confirm", {})
        tid = d["tasks"][0]["id"]
        call("POST", base + f"/tasks/{tid}/milestone", {"milestone": True})
        d = wait_detail(call, base, lambda d: d["tasks"][0]["human_gate"] == "milestone", "首次人工验收")
        verifier_count = len([j for j in d["jobs"] if j["role"] == "verifier"])
        call("POST", base + "/pause", {})
        wait_detail(call, base, lambda d: d["project"]["status"] == "paused", "冲突前暂停")
        reject(call, "POST", base + f"/tasks/{tid}/retry", {})
        assert call("GET", base)["tasks"][0]["human_gate"] == "milestone"
        checkout = Path(p["workspace"]) / "repo"
        file = checkout / "domo_mock" / "T1.txt"
        file.parent.mkdir(exist_ok=True)
        file.write_text("competing integration\n", encoding="utf-8")
        git(["add", "."], checkout)
        git(["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-qm", "competing integration"], checkout)
        d = call("POST", base + f"/tasks/{tid}/milestone/accept", {"reason": "产物通过人工检查"})
        task = d["tasks"][0]
        assert (task["stage"], task["status"], task["human_gate"], task["rejections"]) == ("executing", "queued", None, 0), task
        assert d["project"]["status"] == "paused"
        assert "domo_mock/T1.txt" in task["last_error"]
        assert git(["diff", "--name-only", "--diff-filter=U"], task["worktree"]).strip() == "domo_mock/T1.txt"
        call("POST", base + "/resume", {})
        d = wait_detail(call, base, lambda d: d["tasks"][0]["human_gate"] == "milestone", "冲突修复后重新双重验收")
        assert len([j for j in d["jobs"] if j["role"] == "verifier"]) == verifier_count + 1
        assert d["tasks"][0]["stage"] != "completed" and d["tasks"][0]["rejections"] == 0
        call("POST", base + f"/tasks/{tid}/milestone/accept", {})
        wait_detail(call, base, lambda d: bool(d["project"]["report"]), "里程碑完成")
        call("POST", base + "/archive", {})
        server.restart()
        assert call("GET", base + "/artifacts/" + artifact["id"])["text"] == "original input"


TOKEN_WRITER = r'''
import json, os, pathlib
p = pathlib.Path("domo_mock/T1.txt"); p.parent.mkdir(exist_ok=True); p.write_text("T1", encoding="utf-8")
pathlib.Path(os.environ["DOMO_JOB_DIR"], "result.json").write_text(json.dumps({"kind":"done", "summary":"written", "commit_files":["domo_mock/T1.txt"],
    "usage":{"input_tokens":123, "output_tokens":12}}), encoding="utf-8")
'''


def test_metric_coverage(root):
    script = root / "writer.py"
    script.write_text(TOKEN_WRITER, encoding="utf-8")
    with Server(root, env={"DOMO_MOCK_DELAY_MS": "40"}) as server:
        call = server.call
        team(call)
        writer = call("POST", "/api/agents", {"name": "tokens-only", "kind": "cli", "roles": ["executor"],
            "command": [sys.executable, str(script)]})["id"]
        p = call("POST", "/api/projects", {"name": "研究报告费用", "workspace_type": "plain",
            "repo_path": str(root / "report"), "confirm_mode": "manual", "goal": "[single] 生成研究报告"})["project"]
        base = f"/api/projects/{p['id']}"
        wait_detail(call, base, lambda d: d["proposal"] is not None, "费用方案")
        pool = call("GET", base + "/agents")
        for group in pool["roles"]:
            if group["role"] == "executor":
                for entry in group["entries"]:
                    call("PATCH", base + f"/agents/executor/{entry['agent_id']}", {"enabled": entry["agent_id"] == writer})
        call("POST", base + "/analysis/confirm", {})
        d = wait_detail(call, base, lambda d: bool(d["project"]["report"]), "无费用报告")
        job = next(j for j in d["jobs"] if j["role"] == "executor")
        assert job["cost"] is None and job["input_tokens"] == 123
        totals = call("GET", "/api/usage?group_by=agent&agent_id=" + writer)["totals"]
        assert totals["known_jobs"] == 1 and totals["known_metrics"]["cost"] == 0
        assert totals["known_metrics"]["input_tokens"] == totals["known_metrics"]["output_tokens"] == 1
        assert d["delivery"]["unknown_cost_jobs"] > 0


def main():
    with temp_root("domo-design-consistency-") as root:
        for test in (test_pause, test_runtime_replan, test_project_switch, test_milestone_conflict_and_artifact, test_metric_coverage):
            if len(sys.argv) > 1 and not any(name in test.__name__ for name in sys.argv[1:]):
                continue
            folder = root / test.__name__
            folder.mkdir()
            test(folder)
            print(test.__name__ + " passed", flush=True)


if __name__ == "__main__":
    main()
