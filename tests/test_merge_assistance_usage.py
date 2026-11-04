#!/usr/bin/env python3
"""合入协助的适配器结算：结果缺少用量时读取 OpenCode 主/子会话账本，
账本覆盖结果中的用量，
成功、业务失败和进程失败均记账，项目/Agent 聚合与后续预算检查包含费用。
使用假 OpenCode CLI 和本地 SQLite 账本，不调用真实模型或外网。
"""
from harness import Server, init_repo, temp_root, wait_for, write_python_launcher


OPENCODE_FAKE = r'''
import json, os, pathlib, sqlite3, sys
ledger = pathlib.Path(os.environ["MERGE_USAGE_LEDGER"])
if sys.argv[1:] == ["debug", "paths", "db"]:
    print(ledger)
    sys.exit(0)
sys.stdin.read()
session = sys.argv[sys.argv.index("--session") + 1]
mode = pathlib.Path(os.environ["MERGE_USAGE_MODE"]).read_text(encoding="utf-8")
with sqlite3.connect(ledger) as db:
    db.execute("""CREATE TABLE IF NOT EXISTS session_v2 (
        id TEXT PRIMARY KEY, parent_id TEXT, cost REAL,
        tokens_input INTEGER, tokens_output INTEGER, tokens_reasoning INTEGER,
        tokens_cache_read INTEGER, tokens_cache_write INTEGER)""")
    db.execute("INSERT INTO session_v2 VALUES (?, NULL, 0.75, 1000, 200, 30, 40, 10)", (session,))
    db.execute("INSERT INTO session_v2 VALUES (?, ?, 0.25, 100, 20, 3, 4, 1)", (session + "_child", session))
result = {"kind": "done" if mode == "success" else "failed",
          "summary": "ready", "reason": "fixture deliberate failure"}
if mode != "success":
    result["usage"] = {"cost": 99, "input_tokens": 9999, "output_tokens": 9999}
pathlib.Path(os.environ["DOMO_JOB_DIR"], "result.json").write_text(json.dumps(result), encoding="utf-8")
if mode == "process_failure":
    sys.stderr.write("fixture process failure\n")
    sys.exit(1)
'''


def main():
    with temp_root("domo-merge-usage-") as root:
        script = root / "opencode.py"
        script.write_text(OPENCODE_FAKE, encoding="utf-8")
        launcher = write_python_launcher(script)
        mode_file = root / "mode.txt"
        with Server(root, env={
            "DOMO_MOCK_DELAY_MS": "30",
            "DOMO_OPENCODE_BIN": str(launcher),
            "MERGE_USAGE_LEDGER": str(root / "ledger.db"),
            "MERGE_USAGE_MODE": str(mode_file),
        }) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call("POST", "/api/agents", {
                "name": "mock", "kind": "mock", "roles": ["planner", "executor", "verifier"],
            })
            selected = call("POST", "/api/agents", {
                "name": "ledger resolver", "kind": "opencode", "roles": ["executor"], "enabled": False,
            })
            for mode in ["success", "business_failure", "process_failure"]:
                repo = root / mode
                init_repo(repo)
                mode_file.write_text(mode, encoding="utf-8")
                project = call("POST", "/api/projects", {
                    "name": mode, "repo_path": str(repo),
                    "goal": "[direct] 完成交付", "confirm_mode": "auto",
                })["project"]
                base = f"/api/projects/{project['id']}"
                wait_for(lambda: call("GET", base)["project"]["report"], "直接完成", 20)
                call("POST", base + "/report", {"action": "accept"})
                call("PUT", base + "/limits", {"cost_budget": 0.5})
                request = {"agent_id": selected["id"], "risk_confirmed": True}
                call("POST", base + "/merge/assist", request)
                expected = "succeeded" if mode == "success" else "failed"
                wait_for(lambda: call("GET", base + "/merge/assist")["request"]["state"] == expected,
                         "合入协助结算", 20)
                detail = call("GET", base)
                job = next(job for job in detail["jobs"] if job["role"] == "merge_conflict")
                assert job["status"] == expected and job["usage_known"], job
                assert job["cost"] == 1.0, job
                # Normalized input includes cache reads/writes; output includes reasoning.
                assert job["input_tokens"] == 1155 and job["output_tokens"] == 253, job
                assert job["reasoning_tokens"] == 33, job
                assert job["cache_read_tokens"] == 44 and job["cache_write_tokens"] == 11, job
                assert detail["delivery"]["known_cost"] == 1.0, detail["delivery"]
                usage = call("GET", f"/api/usage?project_id={project['id']}&role=merge_conflict")
                assert usage["totals"]["cost"] == 1.0, usage
                assert usage["totals"]["known_metrics"]["cost"] == 1, usage
                if mode != "success":
                    try:
                        call("POST", base + "/merge/assist", request)
                    except AssertionError as error:
                        assert "409" in str(error) and "预算" in str(error), error
                    else:
                        raise AssertionError("已结算费用达到预算后仍允许重新发起")
            usage = call("GET", f"/api/usage?agent_id={selected['id']}&role=merge_conflict")
            assert usage["totals"]["cost"] == 3.0 and usage["totals"]["jobs"] == 3, usage
            print("合入协助 OpenCode 账本结算、主/子会话汇总、失败计费与预算检查通过")


if __name__ == "__main__":
    main()
