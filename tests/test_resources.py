#!/usr/bin/env python3
"""共享仓库与普通产物保护：不可变记录、替代/分歧、版本读取、作业引用，
输入输出分离、路径逃逸拒绝、外部编辑与中断检查、产物预览、归档保留。
标准库 + mock，无模型或外网。
"""
import sqlite3
from harness import Server, git, temp_root, wait_for


def main():
    with temp_root("domo-resources-") as root:
        delivery = root / "delivery"
        delivery.mkdir()
        (delivery / "source.txt").write_text("original input", encoding="utf-8")
        with Server(root, env={"DOMO_MOCK_DELAY_MS": "40"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call("POST", "/api/agents", {"name": "resources", "kind": "mock", "roles": ["planner", "executor", "verifier"]})
            p = call("POST", "/api/projects", {"name": "非软件研究报告", "workspace_type": "plain", "repo_path": str(delivery), "confirm_mode": "auto"})["project"]
            base = f"/api/projects/{p['id']}"

            def record(body, **extra):
                return {"topic": "研究口径", "body": body, "scope": [], "evidence": ["source.txt"], **extra}

            first = call("POST", base + "/shared", [record("原始事实")])
            first_id = first["records"][0]["id"]
            second = call("POST", base + "/shared", [record("修正事实", supersedes=[first_id], reason="复核原始资料；影响所有报告任务")])
            second_id = next(r["id"] for r in second["records"] if r["body"] == "修正事实")
            third = call("POST", base + "/shared", [record("另一种有证据的结论", conflicts=[second_id], reason="资料解释有分歧，需报告任务补充核验")])
            assert first_id in third["superseded_ids"], third
            assert len(third["records"]) == 3
            old = call("GET", base + "/shared?version=" + first["version"])
            assert len(old["records"]) == 1 and old["records"][0]["body"] == "原始事实", old
            assert (delivery / ".git").exists() is False
            assert git(["rev-parse", "--is-inside-work-tree"], third["repository"]).strip() == "true"
            call("POST", base + "/analysis/messages", {"message": "[single] 生成研究报告"})
            detail = wait_for(lambda: (d if (d := call("GET", base))["project"]["report"] else None), "报告", 30)
            job = next(j for j in detail["jobs"] if j["role"] == "executor")
            context_path = server.root / "ws" / (p["id"].split("_")[0] + "_" + p["id"].split("_")[1][:16]) / "jobs" / job["id"] / "context.json"
            import json
            context = json.loads(context_path.read_text(encoding="utf-8"))
            assert context["shared"]["version"] == third["version"]
            assert not any(r["id"] == first_id for r in context["shared"]["records"])
            with sqlite3.connect(server.root / "data" / "domo.db") as db:
                assert db.execute("SELECT COUNT(*) FROM job_shared_versions WHERE job_id = ?", (job["id"],)).fetchone()[0] == 1
            assets = call("GET", base + "/artifacts")
            assert any(a["path"] == "source.txt" and a["kind"] == "input" for a in assets["items"])
            output = next(a for a in assets["items"] if a["path"] == "domo_mock/T1.txt")
            call("POST", base + "/artifacts", {"path": output["path"], "kind": "delivery", "format": "text/plain", "verification": "逐段核对报告"})
            preview = call("GET", base + "/artifacts/" + output["id"])
            assert "T1" in preview["text"] and preview["versions"] == []
            for path in ["../outside", "/etc/passwd", ".git/config", "C:\\outside"]:
                try:
                    call("POST", base + "/artifacts", {"path": path, "kind": "input"})
                except AssertionError:
                    pass
                else:
                    raise AssertionError(f"unsafe path accepted: {path}")
            # An external edit is detected before dispatching another writer.
            (delivery / "source.txt").write_text("manually reviewed input", encoding="utf-8")
            task = call("POST", base + "/tasks", {"key": "REVIEW", "title": "复核报告", "description": "复核报告证据", "acceptance": ["复核完成"]})
            def blocked():
                detail = call("GET", base)
                return detail if any(t["id"] == task["id"] and t["status"] == "blocked" for t in detail["tasks"]) else None
            detail = wait_for(blocked, "外部变化阻塞", 20)
            assert call("GET", base + "/artifacts")["check"]["interrupted"] is True
            assert (delivery / "source.txt").read_text(encoding="utf-8") == "manually reviewed input"
            call("POST", base + "/workspace/inspect", {"reason": "已检查手工输入变化，保留当前资料并重新生成报告"})
            assert call("GET", base + "/artifacts")["check"]["interrupted"] is False
            call("POST", base + "/archive", {})
            assert call("GET", base + "/shared")["version"] == third["version"]
            assert (delivery / "domo_mock/T1.txt").is_file()
            assert call("GET", base + "/artifacts/" + output["id"])["text"]
            # Simulate shutdown after durable intent but before its Git commit.
            with sqlite3.connect(server.root / "data" / "domo.db") as db:
                db.execute("INSERT INTO shared_records(id, project_id, topic, body, scope, evidence, supersedes, conflicts, reason, created_at) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    ("info_recovery", p["id"], "恢复证据", "持久化后中断的记录", "[]", '["source.txt"]', "[]", "[]", "", "2026-01-01T00:00:00Z"))
            server.restart()
            recovered = call("GET", base + "/shared")
            assert any(record["id"] == "info_recovery" for record in recovered["records"])
            with sqlite3.connect(server.root / "data" / "domo.db") as db:
                assert db.execute("SELECT commit_id FROM shared_records WHERE id = 'info_recovery'").fetchone()[0]
            server.restart()
            assert len(call("GET", base + "/shared")["records"]) == 4
            suggestions = call("GET", "/api/agents/recommendations")
            assert suggestions["automatic_changes"] is False
            advice = next(item for item in suggestions["items"] if item["agent_name"] == "resources")
            assert advice["calls"] > 0 and advice["advice"], advice
            assert any("样本不足" in text for text in advice["advice"]), advice
            print("共享仓库、版本引用、产物保护、外部变化与归档保留通过")


if __name__ == "__main__":
    main()
