#!/usr/bin/env python3
"""持续工作资源：Git/普通工作区中执行、验收、返工与跨任务复用相同输入。

真实 CLI 替身按当前 context 定位资源，不读历史作业；资源不进入源码交付，
重启和归档保留。缺条件请求沿现有决策等待，重启后不自动重试，指导后接续。
不依赖真实模型、SDK 或外网，不将提示词断言作为真实 Agent 决策能力证明。
"""
import json
import sys
from pathlib import Path

from harness import Server, git, init_repo, temp_root, wait_for


SCRIPT = r'''
import json, os, pathlib, subprocess, sys
prompt = sys.stdin.read()
job = pathlib.Path(os.environ["DOMO_JOB_DIR"])
context = json.loads((job / "context.json").read_text(encoding="utf-8"))
role = context["role"]
key = context["task"]["key"]
project = pathlib.Path(context["work_resources"]["project"])
task = pathlib.Path(context["work_resources"]["task"])
assert project.is_dir() and task.is_dir()
assert not project.is_relative_to(pathlib.Path.cwd())
assert not task.is_relative_to(pathlib.Path.cwd())
assert not task.is_relative_to(job)
tool = project / "report_input.py"
counter = task / "attempt.txt"
if role == "executor":
    assert "按范围准备持续使用" in prompt
    attempt = int(counter.read_text()) + 1 if counter.exists() else 1
    counter.write_text(str(attempt))
    if key == "T1":
        if attempt == 1:
            assert not tool.exists()
            tool.write_text('print("report-data-v1")\n', encoding="utf-8")
            (task / "input.txt").write_text("current task input", encoding="utf-8")
        else:
            assert (task / "input.txt").read_text(encoding="utf-8") == "current task input"
    else:
        assert attempt == 1
        assert not (task / "input.txt").exists()
    if key != "T1" or attempt > 1:
        record = next(r for r in context["shared"]["records"] if r["topic"] == "报告核对工具")
        assert json.loads(record["body"])["tool"] == str(tool)
    records = []
    if key == "T1" and attempt == 1:
        records = [{"topic": "报告核对工具", "body": json.dumps({"tool": str(tool), "version": "v1"}),
                    "scope": [], "evidence": [str(tool)]},
                   {"topic": "任务核对输入", "body": str(task / "input.txt"),
                    "scope": [key], "evidence": [str(task / "input.txt")]}]
    if sys.argv[1] == "decision" and key == "T1" and attempt == 1:
        result = {"kind": "replan", "reason": "已准备并登记当前输入；仍需上级落实访问条件",
                  "shared_updates": records}
    else:
        output = pathlib.Path("domo_mock") / (key + ".txt")
        output.parent.mkdir(exist_ok=True)
        output.write_text(key, encoding="utf-8")
        result = {"kind": "done", "summary": "报告已生成，当前核对环境可复用",
                  "commit_files": [output.as_posix()], "shared_updates": records}
else:
    assert role == "verifier"
    assert "资源目录只读" in prompt
    assert "按范围准备持续使用" not in prompt
    attempt = int(counter.read_text())
    assert any(r["topic"] == "报告核对工具" for r in context["shared"]["records"])
    check = subprocess.run([sys.executable, str(tool)], check=True, capture_output=True, text=True)
    assert check.stdout.strip() == "report-data-v1"
    passed = not (sys.argv[1] == "rework" and key == "T1" and attempt == 1)
    result = {"pass": passed, "reason": "核对当前输入与报告", "issues": [] if passed else ["报告需要补充核对"],
              "evidence": {"contract_id": context["contract"]["id"],
                           "artifact_version": context["artifact_version"],
                           "artifacts": ["domo_mock/" + key + ".txt"], "risks": [], "human_checks": [],
                           "criteria": [{"criterion": c, "passed": passed,
                                         "checks": [{"method": "运行当前报告核对工具", "result": check.stdout.strip()}]}
                                        for c in context["task"]["acceptance"]]}}
    if not passed:
        result.update(next_action="execute", guidance="复用当前任务输入补充核对")
(job / "result.json").write_text(json.dumps(result, ensure_ascii=False), encoding="utf-8")
'''


def settled(server, base, predicate):
    def check():
        detail = server.call("GET", base)
        return detail if predicate(detail) and not any(j["status"] == "running" for j in detail["jobs"]) else None
    return wait_for(check, "资源接力流程结算", 45)


def scenario(root, workspace_type, mode):
    root.mkdir()
    delivery = root / "delivery"
    if workspace_type == "git":
        init_repo(delivery)
    else:
        delivery.mkdir()
    script = root / "agent.py"
    script.write_text(SCRIPT, encoding="utf-8")
    with Server(root, env={"DOMO_MOCK_DELAY_MS": "30", "DOMO_MOCK_PLANNER_REPLAN_DECISION": "wait"}) as server:
        call = server.call
        for agent in call("GET", "/api/agents"):
            call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
        call("POST", "/api/agents", {"name": "planner", "kind": "mock", "roles": ["planner"]})
        call("POST", "/api/agents", {"name": "resource consumer", "kind": "cli", "roles": ["executor", "verifier"],
                                    "command": [sys.executable, str(script), mode]})
        project = call("POST", "/api/projects", {"name": "报告核对", "workspace_type": workspace_type,
                       "repo_path": str(delivery), "confirm_mode": "auto"})["project"]
        base = f"/api/projects/{project['id']}"
        call("POST", base + "/analysis/messages", {"message": "整理并核对两份研究报告"})
        if mode == "decision":
            current = settled(server, base, lambda d: any(e["kind"] == "replan_review_waiting" for e in d["events"]))
            task = next(t for t in current["tasks"] if t["key"] == "T1")
            assert task["attempts"] == 0 and task["rejections"] == 0
            review = next(j for j in current["jobs"] if j["role"] == "replan_review")
            review_context = json.loads(Path(project["workspace"], "jobs", review["id"], "context.json").read_text(encoding="utf-8"))
            assert Path(review_context["work_resources"]["task"], "input.txt").is_file()
            assert any(r["topic"] == "报告核对工具" for r in review_context["shared"]["records"])
            count = len(current["jobs"])
            server.restart()
            assert len(call("GET", base)["jobs"]) == count
            call("POST", base + f"/tasks/{task['id']}/arbitration", {"action": "guide", "guidance": "访问条件已落实，复用当前资源继续核对"})
        current = settled(server, base, lambda d: bool(d["project"]["report"]))
        contexts = []
        for job in current["jobs"]:
            if job["role"] in {"executor", "verifier"}:
                context = json.loads(Path(project["workspace"], "jobs", job["id"], "context.json").read_text(encoding="utf-8"))
                contexts.append(context)
        first = [c for c in contexts if c["task"]["key"] == "T1"]
        assert len({c["work_resources"]["task"] for c in first}) == 1
        assert len([c for c in first if c["role"] == "executor"]) == 2
        assert len({c["work_resources"]["project"] for c in contexts}) == 1
        assert len({c["work_resources"]["task"] for c in contexts}) == 2
        resource_dir = Path(first[0]["work_resources"]["project"])
        task_dir = Path(first[0]["work_resources"]["task"])
        assert (task_dir / "attempt.txt").read_text() == "2"
        if workspace_type == "git":
            files = git(["ls-tree", "-r", "--name-only", project["branch"]], delivery).splitlines()
            assert files == ["domo_mock/T1.txt", "domo_mock/T2.txt"], files
        server.restart()
        call("POST", base + "/tasks", {"key": "FOLLOWUP", "title": "追加核对", "description": "复用项目核对工具", "acceptance": ["当前工具可用于追加报告"]})
        settled(server, base, lambda d: bool(d["project"]["report"]) and any(t["key"] == "FOLLOWUP" and t["stage"] == "completed" for t in d["tasks"]))
        call("POST", base + "/archive?force=true", {})
        assert (resource_dir / "report_input.py").is_file()
        assert (task_dir / "input.txt").read_text(encoding="utf-8") == "current task input"


def main():
    with temp_root("domo-work-resources-") as root:
        for workspace_type in ["plain", "git"]:
            for mode in ["rework", "decision"]:
                scenario(root / (workspace_type + "-" + mode), workspace_type, mode)
    print("持续资源同任务/跨任务接力、决策等待、重启及归档保留通过")


if __name__ == "__main__":
    main()
