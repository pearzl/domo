#!/usr/bin/env python3
"""人工返工和指导创建新作业与会话，历史失败不能替代后续执行。

覆盖 Git/普通工作区、暂停/未暂停项目、任务重试/指导裁决入口。执行先失败、
后续执行成功但上级指导后仍被拒绝时，人工返工创建新执行作业，保留历史失败，
新验收关联实际执行作业。使用 mock 与标准库 CLI，不访问模型或外网。
"""
import sys

from harness import Server, init_repo, temp_root, wait_for


VERIFIER = r'''
import json, os, pathlib
job_dir = pathlib.Path(os.environ["DOMO_JOB_DIR"])
context = json.loads((job_dir / "context.json").read_text(encoding="utf-8"))
content = pathlib.Path("domo_mock/T1.txt").read_text(encoding="utf-8")
passed = "T1" in content and pathlib.Path(PASS_ROOT, context["project"]["id"]).is_file()
result = {"pass": passed, "reason": "报告核验完成", "issues": [] if passed else ["补充报告结论"],
    "evidence": {"contract_id": context["contract"]["id"], "artifact_version": context["artifact_version"],
        "artifacts": ["domo_mock/T1.txt"], "risks": [], "human_checks": [],
        "criteria": [{"criterion": item, "passed": passed,
            "checks": [{"method": "检查报告结论", "result": "满足要求" if passed else "结论不完整"}]}
            for item in context["task"]["acceptance"]]}}
if not passed:
    result.update(next_action="execute", guidance="补充报告结论")
(job_dir / "result.json").write_text(json.dumps(result), encoding="utf-8")
'''


def wait_detail(call, base, predicate, label):
    return wait_for(lambda: d if predicate(d := call("GET", base)) else None, label, 30)


def run_case(server, root, workspace, paused, action):
    call = server.call
    project = call("POST", "/api/projects", {"name": f"{workspace}-{paused}-{action}",
        "workspace_type": workspace,
        "repo_path": str(root / ("repo" if workspace == "git" else f"report-{paused}-{action}")),
        "goal": "[single] 生成研究报告", "confirm_mode": "auto"})["project"]
    base = f"/api/projects/{project['id']}"
    before = wait_detail(call, base, lambda d: bool(d["tasks"])
        and d["tasks"][0]["status"] == "blocked" and d["tasks"][0]["rejections"] == 4
        and all(j["status"] != "running" for j in d["jobs"]), "连续验收拒绝后阻塞")
    task_id = before["tasks"][0]["id"]
    executions = [j for j in before["jobs"] if j["role"] == "executor"]
    assert len(executions) == 5, executions
    assert len([j for j in before["jobs"] if j["role"] == "replan_review"]) == 1
    assert executions[0]["status"] == "failed", executions
    assert all(j["status"] == "succeeded" for j in executions[-4:]), executions
    old_failed = executions[0]["id"]
    if paused:
        call("POST", base + "/pause", {})
        wait_detail(call, base, lambda d: d["project"]["status"] == "paused", "项目暂停")
    (root / project["id"]).touch()
    if action == "retry":
        call("POST", base + f"/tasks/{task_id}/retry", {})
    else:
        call("POST", base + f"/tasks/{task_id}/arbitration", {"action": "guide", "guidance": "补齐结论后再提交"})
    if paused:
        recorded = call("GET", base)
        assert recorded["project"]["status"] == "paused", recorded["project"]
        assert len(recorded["jobs"]) == len(before["jobs"]), recorded["jobs"]
        # Deferred retry intent must survive a restart without reviving an old job.
        server.restart()
        assert call("GET", base)["project"]["status"] == "paused"
        call("POST", base + "/resume", {})
    after = wait_detail(call, base, lambda d: bool(d["project"]["report"]), "返工后交付")
    executions_after = [j for j in after["jobs"] if j["role"] == "executor"]
    assert len(executions_after) == len(executions) + 1, executions_after
    assert next(j for j in executions_after if j["id"] == old_failed)["status"] == "failed"
    assert call("GET", f"/api/jobs/{old_failed}/history")["runs"] == []
    new_execution = next(j for j in executions_after if j["id"] not in {old["id"] for old in executions})
    assert new_execution["status"] == "succeeded", new_execution
    verification = call("GET", base + f"/tasks/{task_id}/verifications")["verifications"][-1]
    assert verification["passed"] is True, verification
    assert verification["execution_job_id"] == new_execution["id"], verification
    assert verification["round"] == len(executions_after), verification
    if action == "guide":
        assert "补齐结论后再提交" in call("GET", f"/api/jobs/{new_execution['id']}")["prompt"]
    call("POST", base + "/archive", {})


def main():
    with temp_root("domo-retry-current-job-") as root:
        init_repo(root / "repo")
        verifier = root / "verifier.py"
        verifier.write_text(f"PASS_ROOT = {str(root)!r}\n" + VERIFIER, encoding="utf-8")
        with Server(root, env={"DOMO_MOCK_DELAY_MS": "20", "DOMO_MOCK_FAIL_ONCE": "1", "DOMO_MOCK_PLANNER_REPLAN_DECISION": "reject"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call("POST", "/api/agents", {"name": "writer", "kind": "mock", "roles": ["planner", "executor"]})
            call("POST", "/api/agents", {"name": "checker", "kind": "cli", "roles": ["verifier"],
                "command": [sys.executable, str(verifier)]})
            for workspace in ("git", "plain"):
                for paused in (False, True):
                    for action in ("retry", "guide"):
                        run_case(server, root, workspace, paused, action)
                        print(f"{workspace} paused={paused} action={action} passed", flush=True)


if __name__ == "__main__":
    main()
