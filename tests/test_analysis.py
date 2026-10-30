#!/usr/bin/env python3
"""项目分析阶段冒烟测试。

覆盖范围：
  1. 项目创建后等待输入目标，不派发作业；
  2. 人工确认模式下，规划者多轮提问、给出包含任务清单的方案，人工确认后才创建任务并进入执行；
  3. 自动确认模式下，规划者可以直接跳过人工确认开始执行；
  4. 直接完成模式下，人工确认后才修改仓库，并生成等待验收的报告；
  5. 自动确认的直接完成不等待人工确认；
  6. 人工确认模式忽略规划者的跳过请求；
  7. 开启 mock 分段输出后，运行中的作业输出持续增长，结束后停止；
  8. 作业运行输出可通过接口读取。

约定：使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
"""

import subprocess
import time

from harness import (
    Server,
    ensure_server_built,
    git,
    init_repo,
    temp_root,
    wait_for,
)


def main():
    with temp_root("domo-analysis-") as temp:
        repo = temp / "repo"
        init_repo(repo)
        ensure_server_built()

        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "40",
                "DOMO_MOCK_STREAM": "1",
                "DOMO_MOCK_STREAM_GOAL": "[stream]",
                "DOMO_MOCK_STREAM_INTERVAL_MS": "150",
                "DOMO_MOCK_STREAM_LINES": "12",
            },
        ) as server:

            def call(method, path, data=None):
                return server.call(method, path, data)

            def wait_project(project_id, predicate, label, timeout=60):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            def create_project(name):
                return call(
                    "POST",
                    "/api/projects",
                    {"name": name, "repo_path": str(repo), "base_branch": "main"},
                )["project"]

            def submit(project_id, message, mode="manual"):
                return call(
                    "POST",
                    f"/api/projects/{project_id}/analysis/messages",
                    {"message": message, "confirm_mode": mode},
                )

            def wait_proposal(project_id, kind):
                return wait_project(
                    project_id,
                    lambda detail: detail.get("proposal")
                    and detail["proposal"]["kind"] == kind
                    and detail["project"]["stage"] == "analysis"
                    and detail["project"]["status"] == "blocked",
                    f"{kind} 项目方案",
                )

            def wait_report(project_id):
                return wait_project(
                    project_id,
                    lambda detail: detail["project"]["stage"] == "summarizing"
                    and bool(detail["project"]["report"]),
                    "项目报告",
                )

            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Analysis Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 2,
                },
            )

            # 1. 创建后没有目标，也没有作业。
            empty = create_project("Empty")
            assert empty["goal"] == "", empty
            empty_detail = call("GET", f"/api/projects/{empty['id']}")
            assert empty_detail["project"]["stage"] == "analysis", empty_detail["project"]
            assert empty_detail["project"]["status"] == "blocked", empty_detail["project"]
            assert empty_detail["jobs"] == [], empty_detail["jobs"]

            # 2. 人工确认：多轮提问 -> 包含任务的方案 -> 确认 -> 创建任务并执行。
            questions = create_project("Questions")
            submit(questions["id"], "[ask] 先讨论再规划")
            asked = wait_project(
                questions["id"],
                lambda detail: any(
                    message["role"] == "planner" for message in detail["messages"]
                )
                and detail["proposal"] is None
                and detail["project"]["stage"] == "analysis",
                "规划者提问",
            )
            assert "哪个使用场景" in asked["messages"][-1]["content"], asked["messages"]
            submit(questions["id"], "没有偏好，请直接规划。")
            proposal = wait_proposal(questions["id"], "plan")
            assert proposal["project"]["status"] == "blocked", proposal["project"]
            assert proposal["proposal"]["tasks"], proposal["proposal"]
            assert proposal["tasks"] == [], proposal["tasks"]
            call("POST", f"/api/projects/{questions['id']}/analysis/confirm", {})
            planned = wait_project(
                questions["id"],
                lambda detail: detail["project"]["stage"] in ("executing", "summarizing"),
                "确认方案后进入执行",
            )
            assert planned["project"]["stage"] in ("executing", "summarizing"), planned["project"]
            assert len(planned["tasks"]) == 2, planned["tasks"]
            assert any(job["role"] == "analysis" for job in planned["jobs"]), planned["jobs"]
            call("POST", f"/api/projects/{questions['id']}/archive", {})

            # 3. 自动确认：规划者直接进入执行，不等待人工确认。
            auto = create_project("AutoPlan")
            submit(auto["id"], "[skip] 自动确认普通目标", "auto")
            auto_detail = wait_project(
                auto["id"],
                lambda detail: detail["project"]["stage"] in ("executing", "summarizing"),
                "自动确认后进入执行",
            )
            assert auto_detail["project"]["confirm_mode"] == "auto", auto_detail["project"]
            assert any(job["role"] == "analysis" for job in auto_detail["jobs"]), auto_detail["jobs"]
            call("POST", f"/api/projects/{auto['id']}/archive", {})

            # 4. 人工确认的直接完成：确认前不修改仓库，确认后提交并生成报告。
            direct = create_project("Direct")
            submit(direct["id"], "[direct] 直接完成一个简单目标")
            proposal = wait_proposal(direct["id"], "direct")
            branch = proposal["project"]["branch"]
            result = subprocess.run(
                ["git", "-C", str(repo), "show", f"{branch}:domo_mock/direct.txt"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            assert result.returncode != 0, "人工确认前仓库不应出现产物"
            call("POST", f"/api/projects/{direct['id']}/analysis/confirm", {})
            direct_done = wait_report(direct["id"])
            assert direct_done["project"]["completion_mode"] == "direct", direct_done["project"]
            assert direct_done["tasks"] == [], direct_done["tasks"]
            assert any(job["role"] == "direct" for job in direct_done["jobs"]), direct_done["jobs"]
            content = git(["show", f"{branch}:domo_mock/direct.txt"], repo)
            assert "mock direct completion" in content, content

            # 4.1 报告被拒绝后重新直接完成。
            call(
                "POST",
                f"/api/projects/{direct['id']}/report",
                {"action": "reject", "feedback": "请重新执行直接完成。"},
            )
            redone = wait_project(
                direct["id"],
                lambda detail: detail["project"]["report"]
                and sum(1 for job in detail["jobs"] if job["role"] == "direct") >= 2,
                "直接完成重做",
            )
            assert redone["project"]["completion_mode"] == "direct", redone["project"]
            call("POST", f"/api/projects/{direct['id']}/report", {"action": "accept"})

            # 5. 自动确认的直接完成：不需要人工确认。
            auto_direct = create_project("AutoDirect")
            submit(auto_direct["id"], "[direct] 自动直接完成", "auto")
            auto_direct_done = wait_report(auto_direct["id"])
            assert any(
                job["role"] == "direct" for job in auto_direct_done["jobs"]
            ), auto_direct_done["jobs"]
            assert auto_direct_done["project"]["confirm_mode"] == "auto", auto_direct_done["project"]

            # 6. 人工确认模式忽略规划者的跳过请求。
            forced = create_project("ForcedManual")
            submit(forced["id"], "[skipforce] 人工确认不能被跳过")
            forced_ready = wait_proposal(forced["id"], "plan")
            assert forced_ready["proposal"]["skip_confirmation"] is True, forced_ready["proposal"]
            assert forced_ready["project"]["stage"] == "analysis", forced_ready["project"]
            assert forced_ready["project"]["status"] == "blocked", forced_ready["project"]
            call("POST", f"/api/projects/{forced['id']}/analysis/confirm", {})
            wait_project(
                forced["id"],
                lambda detail: detail["project"]["stage"] in ("executing", "summarizing"),
                "人工确认后进入执行",
            )

            # 7. 分段输出：运行期间 stdout 持续增长，结束后 running 变为 false。
            stream = create_project("Stream")
            submit(stream["id"], "[stream] 观察实时输出")
            stream_detail = wait_project(
                stream["id"],
                lambda detail: any(
                    job["role"] == "analysis" and job["status"] == "running"
                    for job in detail["jobs"]
                ),
                "分段输出作业运行",
            )
            stream_job = next(
                job
                for job in stream_detail["jobs"]
                if job["role"] == "analysis" and job["status"] == "running"
            )
            first = call("GET", f"/api/jobs/{stream_job['id']}/output")
            assert first["running"] is True, first
            time.sleep(0.5)
            second = call("GET", f"/api/jobs/{stream_job['id']}/output")
            assert second["running"] is True, second
            assert len(second["stdout"]) > len(first["stdout"]), (first, second)
            wait_proposal(stream["id"], "plan")
            final = call("GET", f"/api/jobs/{stream_job['id']}/output")
            assert final["running"] is False, final
            assert "mock job" in final["stdout"], final
            assert "segment" in final["stdout"], final
            call("POST", f"/api/projects/{stream['id']}/archive", {})

            # 8. 作业输出可通过接口读取。
            output = call("GET", f"/api/jobs/{forced_ready['jobs'][0]['id']}/output")
            assert "mock job" in output["stdout"], output
            assert output["running"] is False, output

            print("项目分析阶段测试通过")


if __name__ == "__main__":
    main()
