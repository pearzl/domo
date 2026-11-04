#!/usr/bin/env python3
"""人工指定项目合入冲突作业：风险确认、角色/启停绕过、预算、失败保留、
隔离处理后直接线性快进；不创建任务、验收者或改变完成阶段。
脚本化 CLI + mock，不调用真实模型。
"""
import sys
from harness import Server, git, init_repo, temp_root, wait_for

SCRIPT = r'''
import json, os, pathlib, subprocess, sys, time
job = pathlib.Path(os.environ["DOMO_JOB_DIR"])
if os.environ.get("MERGE_FIXTURE_WAIT") == "1":
    time.sleep(30)
if os.environ.get("MERGE_FIXTURE_FAIL") == "1":
    result = {"kind":"failed", "reason":"fixture deliberate failure"}
else:
    files = subprocess.check_output(["git", "diff", "--name-only", "--diff-filter=U"], text=True).splitlines()
    for name in files:
        pathlib.Path(name).write_text("base retained\nproject retained\n", encoding="utf-8")
    result = {"kind":"done", "summary":"resolved conflicts", "usage":{"cost":0.25, "input_tokens":10, "output_tokens":10}}
(job / "result.json").write_text(json.dumps(result), encoding="utf-8")
'''


def main():
    with temp_root("domo-merge-assist-") as root:
        repo = root / "repo"
        init_repo(repo)
        (repo / "domo_mock").mkdir()
        file = repo / "domo_mock/direct.txt"
        file.write_text("original\n", encoding="utf-8")
        git(["add", "."], repo)
        git(["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-m", "input"], repo)
        script = root / "resolve.py"
        script.write_text(SCRIPT, encoding="utf-8")
        with Server(root, env={"DOMO_MOCK_DELAY_MS": "30", "MERGE_FIXTURE_FAIL": "1"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call("POST", "/api/agents", {"name": "mock", "kind": "mock", "roles": ["planner", "executor", "verifier"]})
            selected = call("POST", "/api/agents", {"name": "manual resolver", "kind": "cli", "roles": ["executor"], "enabled": False, "command": [sys.executable, str(script)]})
            p = call("POST", "/api/projects", {"name": "merge", "repo_path": str(repo), "goal": "[direct] 完成交付", "confirm_mode": "auto"})["project"]
            base = f"/api/projects/{p['id']}"
            wait_for(lambda: call("GET", base)["project"]["report"], "直接完成", 20)
            artifacts = call("GET", base + "/artifacts")["items"]
            artifact = next(item for item in artifacts if item["path"] == "domo_mock/direct.txt")
            preview = call("GET", base + "/artifacts/" + artifact["id"])
            assert preview["commit"] in preview["versions"] and "mock direct" in preview["text"], preview
            assert call("GET", base + "/artifacts/" + artifact["id"] + "?commit=" + preview["commit"])["text"] == preview["text"]
            call("POST", base + "/report", {"action": "accept"})
            file.write_text("base retained\n", encoding="utf-8")
            git(["add", "."], repo)
            git(["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-m", "base update"], repo)
            old_base = git(["rev-parse", "main"], repo).strip()
            old_tip = git(["rev-parse", p["branch"]], repo).strip()
            try:
                call("POST", base + "/merge", {})
            except AssertionError as error:
                assert "direct.txt" in str(error)
            else:
                raise AssertionError("expected rebase conflict")
            for body in [{"agent_id": selected["id"], "risk_confirmed": False}]:
                try:
                    call("POST", base + "/merge/assist", body)
                except AssertionError:
                    pass
                else:
                    raise AssertionError("risk not confirmed")
            call("POST", base + "/merge/assist", {"agent_id": selected["id"], "risk_confirmed": True})
            failed = wait_for(lambda: (r if (r := call("GET", base + "/merge/assist"))["request"]["state"] == "failed" else None), "冲突处理失败保留", 20)
            assert "deliberate" in failed["request"]["error"], failed
            assert git(["rev-parse", "main"], repo).strip() == old_base
            assert git(["rev-parse", p["branch"]], repo).strip() == old_tip
            assert call("GET", base)["project"]["stage"] == "completed"
            dirty = call("GET", base + "/archive")["dirty"]
            assert any("resolution" in item["path"] for item in dirty), dirty
            server.restart({"MERGE_FIXTURE_FAIL": "0", "MERGE_FIXTURE_WAIT": "1"})
            call("POST", base + "/merge/assist", {"agent_id": selected["id"], "risk_confirmed": True})
            wait_for(lambda: any(j["role"] == "merge_conflict" and j["status"] == "running" and j["started_at"] for j in call("GET", base)["jobs"]), "可停止的合入作业", 20)
            call("DELETE", base + "/merge/assist")
            wait_for(lambda: call("GET", base + "/merge/assist")["request"]["state"] == "canceled", "停止保留现场", 10)
            assert git(["rev-parse", "main"], repo).strip() == old_base
            assert call("GET", base)["project"]["stage"] == "completed"
            server.restart({"MERGE_FIXTURE_FAIL": "0", "MERGE_FIXTURE_WAIT": "0"})
            call("POST", base + "/merge/assist", {"agent_id": selected["id"], "risk_confirmed": True})
            result = wait_for(lambda: (r if (r := call("GET", base + "/merge/assist"))["request"]["state"] == "succeeded" else None), "处理后直接合入", 30)
            detail = call("GET", base)
            assert not detail["tasks"], detail["tasks"]
            assert detail["project"]["stage"] == "completed" and detail["project"]["status"] is None
            merge_jobs = [j for j in detail["jobs"] if j["role"] == "merge_conflict"]
            assert len(merge_jobs) == 3 and merge_jobs[-1]["agent_id"] == selected["id"], merge_jobs
            assert not any(j["role"] == "verifier" for j in detail["jobs"])
            assert git(["rev-parse", "main"], repo).strip() == detail["project"]["merged_commit"]
            assert git(["rev-list", "--merges", f"{old_base}..main"], repo).strip() == ""
            assert "project retained" in file.read_text(encoding="utf-8")
            print("人工合入冲突处理：失败保留、再次处理、独立项目作业与线性合入通过")


if __name__ == "__main__":
    main()
