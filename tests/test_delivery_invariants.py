#!/usr/bin/env python3
"""交付一致性回归：非软件普通目录、目录独占、人工重规划确认、
任务暂停意图、契约版本及作业引用、执行与验收关联、预算授权和事务审计。
使用 mock，不访问外网；全部产物在系统临时目录。
"""
import sqlite3
import time

from harness import Server, temp_root, wait_for


def main():
    with temp_root("domo-delivery-invariants-") as root:
        with Server(root, env={"DOMO_MOCK_DELAY_MS": "120", "DOMO_MOCK_EXECUTOR_DELAY_MS": "700"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call("POST", "/api/agents", {"name": "evidence", "kind": "mock",
                 "roles": ["planner", "executor", "verifier"], "max_concurrency": 3})
            detail = call("POST", "/api/projects", {"name": "研究报告", "workspace_type": "plain",
                "repo_path": str(root / "report"), "confirm_mode": "manual", "goal": "整理一份本地研究报告"})
            pid = detail["project"]["id"]
            base = f"/api/projects/{pid}"

            def get():
                return call("GET", base)

            def wait(predicate, label):
                return wait_for(lambda: (value if predicate(value := get()) else None), label, 30)

            def rejected(method, path, body):
                try:
                    call(method, path, body)
                except AssertionError:
                    return
                raise AssertionError(f"request should fail: {path}")

            for path in [root / "report", root / "report" / "child", root]:
                rejected("POST", "/api/projects", {"name": "冲突目录", "workspace_type": "plain", "repo_path": str(path)})

            draft = wait(lambda d: d["proposal"] is not None and d["project"]["status"] == "blocked", "人工确认草稿")
            rejected("POST", base + "/analysis/confirm", {"proposal": {"kind": "direct", "summary": "旧草稿"}})
            call("POST", base + "/analysis/confirm", {"proposal": draft["proposal"]})
            active = wait(lambda d: bool(d["tasks"]), "任务创建")
            task = active["tasks"][-1]
            call("POST", base + f"/tasks/{task['id']}/pause", {"force": True})
            call("POST", base + "/pause", {"force": True})
            if call("GET", base + "/artifacts")["check"]:
                call("POST", base + "/workspace/inspect", {"reason": "已检查强制暂停后的普通文件现场，保留当前内容"})
            call("POST", base + "/resume", {})
            paused = next(t for t in get()["tasks"] if t["id"] == task["id"])
            assert paused["status"] == "paused", paused

            call("POST", base + "/replan", {"reason": "检查报告结构与证据"})
            replan = wait(lambda d: d["project"]["replan_active"] == 1 and d["proposal"] is not None
                          and d["project"]["status"] == "blocked", "重规划草稿等待确认")
            time.sleep(0.4)
            assert get()["project"]["replan_active"] == 1
            old_ids = {t["id"] for t in replan["tasks"]}
            call("POST", base + "/analysis/confirm", {"proposal": replan["proposal"]})
            applied = get()
            assert applied["project"]["replan_active"] == 0
            assert any(t["id"] not in old_ids for t in applied["tasks"])
            versions = applied["delivery"]["contracts"]
            assert len(versions) >= 3, versions

            budgeted = call("POST", "/api/projects", {"name": "预算报告", "workspace_type": "plain",
                "repo_path": str(root / "budget"), "confirm_mode": "auto"})
            bid = budgeted["project"]["id"]
            bpath = f"/api/projects/{bid}"
            call("PUT", bpath + "/limits", {"cost_budget": 0, "task_attempt_limit": 1, "replan_limit": 0})
            call("POST", bpath + "/analysis/messages", {"message": "整理预算报告"})
            limited = wait_for(lambda: (v if (v := call("GET", bpath))["project"]["status"] == "blocked"
                and "费用预算" in (v["project"]["blocked_reason"] or "") else None), "零预算阻止派发", 10)
            assert not limited["jobs"], limited["jobs"]
            call("PUT", bpath + "/limits", {"cost_budget": None, "task_attempt_limit": 1, "replan_limit": 0})
            wait_for(lambda: call("GET", bpath)["jobs"], "授权后自动派发", 10)
            with sqlite3.connect(server.root / "data" / "domo.db") as db:
                assert db.execute("SELECT COUNT(*) FROM jobs WHERE contract_id IS NULL").fetchone()[0] == 0
                assert db.execute("SELECT COUNT(*) FROM tasks WHERE contract_id IS NULL").fetchone()[0] == 0
                assert db.execute("SELECT COUNT(*) FROM events WHERE kind = 'project_state_changed' AND json_valid(payload)").fetchone()[0] > 0

            parallel = call("POST", "/api/projects", {"name": "多里程碑报告", "workspace_type": "plain",
                "repo_path": str(root / "milestones"), "goal": "[parallel] 整理并核对报告", "confirm_mode": "manual"})
            mid = parallel["project"]["id"]
            mpath = f"/api/projects/{mid}"
            wait_for(lambda: call("GET", mpath)["proposal"], "里程碑方案", 10)
            planned = call("POST", mpath + "/analysis/confirm", {})
            call("POST", mpath + "/pause", {})
            for task in planned["tasks"][:2]:
                call("POST", mpath + f"/tasks/{task['id']}/milestone", {"milestone": True})
            wait_for(lambda: call("GET", mpath)["project"]["status"] == "paused", "项目普通暂停结算", 10)
            call("POST", mpath + "/resume", {})
            def gates_done():
                detail = call("GET", mpath)
                tasks = detail["tasks"]
                return detail if (sum(task["human_gate"] == "milestone" for task in tasks) == 2
                    and tasks[2]["stage"] == "completed" and detail["project"]["status"] == "blocked"
                    and not any(job["status"] == "running" for job in detail["jobs"])) else None
            settled = wait_for(gates_done, "独立里程碑不阻挡其他任务", 20)
            jobs = {job["id"]: job for job in settled["jobs"]}
            verifiers = [job for job in jobs.values() if job["role"] == "verifier"]
            assert verifiers
            for verifier in verifiers:
                execution = jobs[verifier["execution_job_id"]]
                assert execution["role"] == "executor", execution
                assert execution["task_id"] == verifier["task_id"], (execution, verifier)
            summary = next(p for p in call("GET", "/api/projects") if p["id"] == mid)
            assert len(summary["attention_items"]) == 2, summary
            assert all(item["task"] for item in summary["attention_items"]), summary
            assert settled["attention_items"] == summary["attention_items"], (settled, summary)
            print("交付一致性回归测试通过")


if __name__ == "__main__":
    main()
