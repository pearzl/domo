#!/usr/bin/env python3
"""DOMO 普通工作区（非 Git）全流程测试。

覆盖范围：
  1. 选择不存在的本地目录时自动创建，项目记录普通工作区类型；
  2. 普通目录交付不对产物使用 Git，内部共享仓库独立保存；
  3. 同一项目同一时刻只推进一个任务，任务没有分支与 worktree；
  4. 完成即交付，不产生合入待办，合入接口被拒绝；
  5. 归档保留产物与 jobs 审计记录，不产生 Git 归档引用。

使用内置 mock Agent，不访问外网；临时数据全部放在系统临时目录。
退出码 0 表示全部通过。
"""

from harness import Server, ensure_server_built, temp_root, wait_for


def main():
    with temp_root("domo-plain-workspace-") as temp:
        delivery = temp / "delivery"
        ensure_server_built()
        with Server(
            temp,
            env={
                "DOMO_MOCK_DELAY_MS": "40",
                "DOMO_MOCK_EXECUTOR_DELAY_MS": "220",
            },
        ) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Plain Workspace Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 4,
                },
            )

            def wait_project(project_id, predicate, label, timeout=120):
                def check():
                    detail = call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            def new_plain_project(name, goal):
                detail = call(
                    "POST",
                    "/api/projects",
                    {
                        "name": name,
                        "goal": goal,
                        "workspace_type": "plain",
                        "repo_path": str(delivery / name),
                        "confirm_mode": "auto",
                    },
                )
                project = detail["project"]
                assert project["workspace_type"] == "plain", project
                assert project["base_branch"] == "", project
                assert project["branch"] == "", project
                return project["id"], detail

            project_id, created = new_plain_project(
                "plain-parallel", "[parallel] 普通工作区并行任务"
            )
            assert (delivery / "plain-parallel").is_dir()
            assert created["project"]["status"] != "archived", created["project"]

            detail = wait_project(
                project_id,
                lambda item: item["project"]["stage"] == "summarizing"
                and bool(item["project"]["report"]),
                "普通工作区项目报告",
            )
            tasks = detail["tasks"]
            assert len(tasks) == 3, tasks
            assert all(task["branch"] is None and task["worktree"] is None for task in tasks), tasks
            for task in tasks:
                path = delivery / "plain-parallel" / "domo_mock" / f"{task['key']}.txt"
                assert path.is_file(), path

            executor_jobs = sorted(
                (job for job in detail["jobs"] if job["role"] == "executor"),
                key=lambda job: job["started_at"] or "",
            )
            assert len(executor_jobs) == 3, executor_jobs
            for previous, current in zip(executor_jobs, executor_jobs[1:]):
                assert previous["finished_at"] <= current["started_at"], (
                    previous,
                    current,
                )

            summary = next(
                item for item in call("GET", "/api/projects") if item["id"] == project_id
            )
            assert summary["workspace_type"] == "plain", summary
            assert [item["kind"] for item in summary["attention_items"]] == ["review"], summary

            call("POST", f"/api/projects/{project_id}/report", {"action": "accept"})
            detail = call("GET", f"/api/projects/{project_id}")
            assert detail["project"]["stage"] == "completed", detail["project"]
            assert detail["project"]["merged_at"] is None, detail["project"]
            assert detail["merged_into_base"] is False
            assert all(task["status"] != "canceled" for task in detail["tasks"])

            summary = next(
                item for item in call("GET", "/api/projects") if item["id"] == project_id
            )
            assert summary["attention_items"] == [], summary

            status, _ = raw_status(
                server.base, "POST", f"/api/projects/{project_id}/merge", {}
            )
            assert status == 400, status

            refs = call("GET", f"/api/projects/{project_id}/archive-refs")
            assert refs["project"] is None and refs["tasks"] == [], refs

            call("POST", f"/api/projects/{project_id}/archive", {})
            archived = call("GET", f"/api/projects/{project_id}")
            assert archived["project"]["status"] == "archived", archived["project"]
            assert (delivery / "plain-parallel" / "domo_mock" / "T1.txt").is_file()
            jobs_dir = server.root / "ws" / workspace_key(project_id) / "jobs"
            assert jobs_dir.is_dir(), jobs_dir
            assert any(jobs_dir.iterdir()), "归档后缺少 jobs 审计记录"
            assert any(
                event["kind"] == "project_archived" for event in archived["events"]
            )

            # 派生任务沿用普通任务规则：子任务顺序完成并独立验收。
            server.restart(
                {
                    "DOMO_MOCK_DERIVE_ONCE": "1",
                    "DOMO_MOCK_DERIVE_GOAL": "[derive-parent]",
                    "DOMO_MOCK_DERIVE_KEY": "T1",
                }
            )
            derive_id, _ = new_plain_project(
                "plain-derive", "[derive-parent] 普通工作区派生子任务"
            )
            detail = wait_project(
                derive_id,
                lambda item: item["project"]["stage"] == "summarizing"
                and bool(item["project"]["report"]),
                "普通工作区派生链完成",
            )
            parent = next(task for task in detail["tasks"] if task["key"] == "T1")
            children = [
                task for task in detail["tasks"] if task["parent_id"] == parent["id"]
            ]
            assert children and all(child["stage"] == "completed" for child in children), children
            for child in children:
                assert any(job["task_id"] == child["id"] and job["role"] == "verifier"
                           and job["status"] == "succeeded" for job in detail["jobs"]), child
            assert parent["stage"] == "completed", parent
            for child in children:
                path = delivery / "plain-derive" / "domo_mock" / f"{child['key']}.txt"
                assert path.is_file(), path

            print("普通工作区分析、串行执行、验收、交付与归档测试通过")


def raw_status(base, method, path, data=None):
    import json
    import urllib.error
    import urllib.request

    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(
        base + path,
        data=body,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            response.read()
            return response.status, None
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode("utf-8", errors="replace")


def workspace_key(project_id):
    prefix, suffix = project_id.split("_", 1)
    return f"{prefix}_{suffix[:16]}"


if __name__ == "__main__":
    main()
