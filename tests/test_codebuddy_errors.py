#!/usr/bin/env python3
"""CodeBuddy 结果事件错误测试。

场景：
  1. CodeBuddy 以退出码 0 结束，但在 result 事件报告 429 配额错误；
  2. DOMO 将该次运行判为限流失败，不追加协议继续提示；
  3. 作业错误与活动视图都保留 429 原因。

使用脚本化 CodeBuddy 假 CLI 与内置 mock Agent，不访问外网；临时数据全部
放在系统临时目录。退出码 0 表示全部通过。
"""

import json
import sys
import time

from harness import Server, init_repo, temp_root, wait_for, write_python_launcher


FAKE_CODEBUDDY = """\
import json
import os
import sys

sys.stdin.reconfigure(encoding="utf-8", errors="replace")
sys.stdout.reconfigure(encoding="utf-8")

prompt = sys.stdin.read()
with open(os.environ["FAKE_CODEBUDDY_LOG"], "a", encoding="utf-8") as log:
    log.write(
        json.dumps({"prompt_chars": len(prompt), "args": sys.argv[1:]}) + "\\n"
    )


def emit(event):
    print(json.dumps(event, ensure_ascii=False), flush=True)


session = "quota-session"
emit({"type": "system", "subtype": "init", "session_id": session, "model": "hy3"})
emit({
    "type": "assistant",
    "session_id": session,
    "message": {
        "role": "assistant",
        "content": [{"type": "text", "text": "429 您的使用量已超出频率限制，请稍后重试"}],
    },
})
emit({
    "type": "result",
    "subtype": "error_during_execution",
    "is_error": True,
    "session_id": session,
    "duration_ms": 12,
    "total_cost_usd": 0,
    "usage": {
        "input_tokens": 0,
        "output_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0,
    },
    "errors": ["429 您的使用量已超出频率限制，请稍后重试\\n更多信息"],
    "errors_info": [{
        "status": 429,
        "code": 6004,
        "category": "quota",
        "details": "429 您的使用量已超出频率限制，请稍后重试",
    }],
})
"""


def main():
    with temp_root("domo-codebuddy-quota-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        fake = temp / "fake_codebuddy.py"
        fake.write_text(FAKE_CODEBUDDY, encoding="utf-8")
        launcher = write_python_launcher(fake)
        call_log = temp / "codebuddy.jsonl"

        with Server(
            temp,
            env={
                "DOMO_WORKBUDDY_BIN": str(launcher),
                "FAKE_CODEBUDDY_LOG": str(call_log),
                "DOMO_MOCK_DELAY_MS": "10",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "10",
                "DOMO_RATE_LIMIT_COOLDOWN_SECS": "60",
            },
        ) as server:

            def call(method, path, data=None):
                return server.call(method, path, data)

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Mock Planner",
                    "kind": "mock",
                    "roles": ["planner", "verifier"],
                    "max_concurrency": 2,
                },
            )
            executor = call(
                "POST",
                "/api/agents",
                {
                    "name": "Quota Executor",
                    "kind": "codebuddy",
                    "roles": ["executor"],
                    "max_concurrency": 1,
                },
            )

            project = call(
                "POST",
                "/api/projects",
                {
                    "name": "CodeBuddy Quota",
                    "repo_path": str(repo),
                    "base_branch": "main",
                },
            )["project"]
            project_id = project["id"]

            pool = call("GET", f"/api/projects/{project_id}/agents")
            for entry in next(
                group for group in pool["roles"] if group["role"] == "executor"
            )["entries"]:
                if entry["agent_id"] != executor["id"] and entry["enabled"]:
                    call(
                        "PATCH",
                        f"/api/projects/{project_id}/agents/executor/{entry['agent_id']}",
                        {"enabled": False},
                    )
            call(
                "PATCH",
                f"/api/projects/{project_id}/agents/executor/{executor['id']}",
                {"position": 0},
            )

            call(
                "POST",
                f"/api/projects/{project_id}/analysis/messages",
                {"message": "[single] 验证 CodeBuddy 429 失败处理", "confirm_mode": "auto"},
            )

            def failed_executor():
                detail = call("GET", f"/api/projects/{project_id}")
                state = detail["project"]
                if state["status"] == "blocked" and state["stage"] == "analysis":
                    raise AssertionError(
                        f"项目分析阻塞：{state['blocked_reason']} {detail['messages']}"
                    )
                if detail.get("proposal") and state["stage"] == "analysis":
                    try:
                        call("POST", f"/api/projects/{project_id}/analysis/confirm", {})
                    except AssertionError as error:
                        if "请稍后再确认" not in str(error):
                            raise
                jobs = [
                    job
                    for job in detail["jobs"]
                    if job["role"] == "executor"
                    and job["agent_id"] == executor["id"]
                    and job["status"] == "failed"
                ]
                return (detail, jobs[0]) if jobs else None

            detail, job = wait_for(
                failed_executor, "CodeBuddy 429 作业失败", timeout=120
            )

            job_detail = call("GET", f"/api/jobs/{job['id']}")
            assert job_detail["failure_kind"] == "rate_limit", job_detail
            assert job_detail["failure_summary"]["kind"] == "rate_limit", job_detail
            assert "429" in (job_detail["error"] or ""), job_detail
            assert "频率限制" in (job_detail["error"] or ""), job_detail

            activity = call("GET", f"/api/jobs/{job['id']}/activity")
            assert activity["format"] == "codebuddy", activity
            assert any(
                item["kind"] == "line" and "429" in (item["text"] or "")
                for item in activity["items"]
            ), activity

            workspace = detail["project"]["workspace"]
            stdout_path = f"{workspace}/jobs/{job['id']}/stdout.log"
            with open(stdout_path, encoding="utf-8") as handle:
                stdout = handle.read()
            assert stdout.count('"type": "result"') == 1, stdout
            assert "--- 继续执行 ---" not in stdout, stdout

            entries = []
            if call_log.exists():
                with open(call_log, encoding="utf-8") as handle:
                    entries = [json.loads(line) for line in handle if line.strip()]
            assert len(entries) == 1, f"429 失败不应追加继续提示: {entries}"
            assert "-p" in entries[0]["args"], entries[0]

            print("CodeBuddy 429 失败适配测试通过")
            print(f"  项目 {project_id} 作业 {job['id']}")


if __name__ == "__main__":
    sys.exit(main())
