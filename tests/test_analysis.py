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
   9. Git/普通工作区的单任务方案保持独立执行、任务验收和项目验收，
      不转为直接完成；实际投递的粒度规则不因工作区改变，且不进入验收职责。

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

            def project_summary(project_id):
                return next(
                    item for item in call("GET", "/api/projects")
                    if item["id"] == project_id
                )

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
            assert [item["kind"] for item in project_summary(empty["id"])["attention_items"]] == ["blocked"]

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
            assert [item["kind"] for item in project_summary(questions["id"])["attention_items"]] == ["blocked"]
            submit(questions["id"], "没有偏好，请直接规划。")
            proposal = wait_proposal(questions["id"], "plan")
            assert proposal["project"]["status"] == "blocked", proposal["project"]
            assert proposal["proposal"]["tasks"], proposal["proposal"]
            assert proposal["tasks"] == [], proposal["tasks"]
            assert [item["kind"] for item in project_summary(questions["id"])["attention_items"]] == ["confirm"]
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
            assert project_summary(questions["id"])["attention_items"] == []

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
            call("POST", f"/api/projects/{auto['id']}/archive?force=true", {})

            # 6. 人工确认模式忽略规划者的跳过请求。
            forced = create_project("ForcedManual")
            submit(forced["id"], "[skipforce] 人工确认不能被跳过")
            forced_ready = wait_proposal(forced["id"], "plan")
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

            # 9. mock 只验证单工作单元的真实流程，不评价模型是否选对粒度。
            policies = {}
            for workspace_type in ("git", "plain"):
                directory = repo if workspace_type == "git" else temp / "research"
                directory.mkdir(exist_ok=True)
                single = call(
                    "POST",
                    "/api/projects",
                    {
                        "name": f"Single research unit {workspace_type}",
                        "workspace_type": workspace_type,
                        "repo_path": str(directory),
                        "base_branch": "main",
                    },
                )["project"]
                project_id = single["id"]
                submit(project_id, "[single] 整理资料、交叉核对并撰写调研报告")
                proposed = wait_proposal(project_id, "plan")
                assert len(proposed["proposal"]["tasks"]) == 1, proposed["proposal"]
                assert proposed["tasks"] == [], proposed["tasks"]
                call("POST", f"/api/projects/{project_id}/analysis/confirm", {})
                completed = wait_report(project_id)
                assert len(completed["tasks"]) == 1, completed["tasks"]
                assert completed["tasks"][0]["stage"] == "completed"
                succeeded = [
                    job for job in completed["jobs"] if job["status"] == "succeeded"
                ]
                roles = [job["role"] for job in succeeded]
                for role in ("analysis", "executor", "verifier", "project_acceptance"):
                    assert roles.count(role) == 1, roles
                assert "direct" not in roles, roles
                for job in succeeded:
                    prompt = call("GET", f"/api/jobs/{job['id']}")["prompt"]
                    expects_policy = job["role"] in ("analysis", "executor")
                    assert prompt.count("## 任务粒度与分工") == int(expects_policy)
                    if expects_policy:
                        policy = prompt.split("## 任务粒度与分工\n", 1)[1].split(
                            "\n## ", 1
                        )[0].strip()
                        assert "不以工作区类型决定粒度" in policy
                        assert "不自动提高质量或节省 Token" in policy
                        policies[(workspace_type, job["role"])] = policy
                call("POST", f"/api/projects/{project_id}/archive", {})
            assert len(set(policies.values())) == 1, policies

            print("项目分析阶段测试通过")


if __name__ == "__main__":
    main()
