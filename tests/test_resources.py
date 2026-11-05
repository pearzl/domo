#!/usr/bin/env python3
"""共享仓库与显式成果索引：不可变记录、替代/分歧、版本读取、作业引用，
目录输入无需登记、路径逃逸拒绝、当前现场接力、产物预览、归档保留。
标准库 + mock，无模型或外网。
"""
import json
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
            assert call("GET", base + "/artifacts")["items"] == []

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
            context = json.loads(context_path.read_text(encoding="utf-8"))
            assert json.loads((context_path.parent / "artifacts.json").read_text(encoding="utf-8")) == context["artifacts"]
            assert context["shared"]["version"] == third["version"]
            assert not any(r["id"] == first_id for r in context["shared"]["records"])
            with sqlite3.connect(server.root / "data" / "domo.db") as db:
                assert db.execute("SELECT COUNT(*) FROM job_shared_versions WHERE job_id = ?", (job["id"],)).fetchone()[0] == 1
            assets = call("GET", base + "/artifacts")
            assert not any(a["path"] == "source.txt" for a in assets["items"])
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
            # Later tasks read the current directory, including external edits.
            call("POST", base + "/artifacts", {"path": "source.txt", "kind": "input"})
            (delivery / "source.txt").write_text("manually reviewed input", encoding="utf-8")
            task = call("POST", base + "/tasks", {"key": "REVIEW", "title": "复核报告", "description": "复核报告证据", "acceptance": ["复核完成"]})
            def completed():
                detail = call("GET", base)
                return detail if any(t["id"] == task["id"] and t["stage"] == "completed" for t in detail["tasks"]) and detail["project"]["report"] else None
            detail = wait_for(completed, "外部变化后基于当前目录完成", 20)
            assert (delivery / "source.txt").read_text(encoding="utf-8") == "manually reviewed input"
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

            # Only explicitly selected references enter the index. Large explicit
            # indexes stay available in the job snapshot without filling prompts.
            large = root / "large"
            large.mkdir()
            for index in range(150):
                (large / f"source-{index:03}.txt").write_text("input", encoding="utf-8")
            project = call("POST", "/api/projects", {"name": "large-input", "workspace_type": "plain",
                "repo_path": str(large), "confirm_mode": "auto"})["project"]
            large_base = f"/api/projects/{project['id']}"
            assert call("GET", large_base + "/artifacts")["items"] == []
            for index in range(150):
                call("POST", large_base + "/artifacts", {"path": f"source-{index:03}.txt", "kind": "input", "verification": "核对参考资料来源"})
            call("POST", large_base + "/analysis/messages", {"message": "[single] 生成报告"})
            result = wait_for(lambda: (d if (d := call("GET", large_base))["project"]["report"] else None), "大清单项目完成", 30)
            workspace = project["id"].split("_")[0] + "_" + project["id"].split("_")[1][:16]
            for job in result["jobs"]:
                job_dir = root / "ws" / workspace / "jobs" / job["id"]
                snapshot = json.loads((job_dir / "artifacts.json").read_text(encoding="utf-8"))
                assert len(snapshot["items"]) >= 150
                assert "permissions" not in snapshot and "check" not in snapshot
                prompt = (job_dir / "prompt.md").read_text(encoding="utf-8")
                assert len(prompt) < 30_000, (job["role"], len(prompt))
                assert '"snapshot_file"' in prompt and '"item_count"' in prompt
                assert "source-149.txt" not in prompt
            print("共享仓库、版本引用、显式成果、当前现场接力与归档保留通过")


if __name__ == "__main__":
    main()
