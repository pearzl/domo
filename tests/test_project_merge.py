#!/usr/bin/env python3
"""DOMO 项目分支 rebase 合入测试。

覆盖范围：
  1. 未完成项目调用合入接口返回 400；
  2. 完成项目合入后项目分支 rebase 到 base，base 快进到项目分支提交，
     历史保持线性且没有 merge commit；
  3. 重复合入返回“已是最新”，提交数与引用保持不变；
  4. base 在 worktree 检出且有未提交改动时拒绝合入，两侧引用不变；
  5. rebase 冲突时中止并回滚，两侧引用不变，错误信息列出冲突文件；
  6. 目标分支已包含项目分支提交时（包括在 DOMO 之外完成的合入），
     详情返回 merged_into_base；
  7. 合入后追加任务会清空合入记录，新工作完成后重新提供合入入口。

约定：使用内置 mock Agent 与本地临时仓库，不访问外网；临时数据全部放在
系统临时目录。退出码 0 表示全部通过。
"""

import os
import subprocess

from harness import (
    Server,
    ensure_server_built,
    git,
    init_repo,
    temp_root,
    wait_for,
)


def main():
    with temp_root("domo-project-merge-") as temp:
        repos = {}
        for name in ("ok", "dirty", "conflict", "loose"):
            repo = temp / name
            init_repo(repo)
            (repo / "README.md").write_text(f"base {name}\n", encoding="utf-8")
            git(["add", "README.md"], repo)
            git(
                [
                    "-c",
                    "user.name=test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "-q",
                    "-m",
                    "init",
                ],
                repo,
            )
            repos[name] = repo

        repo_ok = repos["ok"]
        repo_dirty = repos["dirty"]
        repo_conflict = repos["conflict"]
        repo_loose = repos["loose"]

        # loose 仓库的 base 分支没有被任何 worktree 检出，合入走引用更新。
        git(["branch", "release", "main"], repo_loose)

        ensure_server_built()

        with Server(temp, env={"DOMO_MOCK_DELAY_MS": "40"}) as server:

            def expect_bad(method, path, data=None):
                try:
                    server.call(method, path, data)
                except AssertionError as error:
                    text = str(error)
                    assert "400" in text, text
                    return text
                raise AssertionError(f"{method} {path} 应当返回 400")

            def wait_project(project_id, predicate, label, timeout=90):
                def check():
                    detail = server.call("GET", f"/api/projects/{project_id}")
                    return detail if predicate(detail) else None

                return wait_for(check, label, timeout)

            def make_project(repo, name, base="main"):
                project = server.call(
                    "POST",
                    "/api/projects",
                    {
                        "name": name,
                        "repo_path": str(repo),
                        "base_branch": base,
                    },
                )["project"]
                server.call(
                    "POST",
                    f"/api/projects/{project['id']}/analysis/messages",
                    {"message": "[direct] 生成合并测试文件", "confirm_mode": "auto"},
                )
                return project

            def complete_project(repo, name, base="main"):
                project = make_project(repo, name, base)
                project_id = project["id"]
                wait_project(
                    project_id,
                    lambda d: d["project"]["stage"] == "summarizing"
                    and bool(d["project"]["report"]),
                    "项目报告",
                )
                server.call(
                    "POST",
                    f"/api/projects/{project_id}/report",
                    {"action": "accept", "feedback": None},
                )
                detail = wait_project(
                    project_id,
                    lambda d: d["project"]["stage"] == "completed",
                    "项目完成",
                )
                return detail

            def complete_task_project(repo, name):
                project = server.call(
                    "POST",
                    "/api/projects",
                    {
                        "name": name,
                        "repo_path": str(repo),
                        "base_branch": "main",
                    },
                )["project"]
                project_id = project["id"]
                server.call(
                    "POST",
                    f"/api/projects/{project_id}/analysis/messages",
                    {"message": "[single] 外部合入检测", "confirm_mode": "auto"},
                )
                wait_project(
                    project_id,
                    lambda d: d["project"]["stage"] == "summarizing"
                    and bool(d["project"]["report"]),
                    "项目报告",
                )
                server.call(
                    "POST",
                    f"/api/projects/{project_id}/report",
                    {"action": "accept", "feedback": None},
                )
                return wait_project(
                    project_id,
                    lambda d: d["project"]["stage"] == "completed",
                    "项目完成",
                )

            def branch_heads(detail):
                branch = detail["project"]["branch"]
                workspace = os.path.join(detail["project"]["workspace"], "repo")
                return (
                    branch,
                    git(["rev-parse", branch], workspace).strip(),
                    workspace,
                )

            agents = server.call("GET", "/api/agents")
            for agent in agents:
                server.call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            server.call(
                "POST",
                "/api/agents",
                {
                    "name": "Merge Mock",
                    "kind": "mock",
                    "roles": ["planner", "executor", "verifier"],
                    "max_concurrency": 3,
                },
            )

            # ---- 未完成项目拒绝 --------------------------------------------------
            print("--> 未完成项目拒绝")
            pending = make_project(repo_ok, "Pending")
            bad = expect_bad("POST", f"/api/projects/{pending['id']}/merge")
            assert "未完成" in bad, bad

            # ---- 成功合入 + 重复合入 ---------------------------------------------
            print("--> 成功合入与重复合入")
            detail = complete_project(repo_ok, "MergeOk")
            project_id = detail["project"]["id"]
            branch, project_head, worktree = branch_heads(detail)
            base_before = git(["rev-parse", "main"], repo_ok).strip()
            assert project_head != base_before, (project_head, base_before)
            assert detail["merged_into_base"] is False, detail

            result = server.call("POST", f"/api/projects/{project_id}/merge")
            assert result["status"] == "merged", result
            assert result["merged_commit"] == project_head, result
            assert result["merged_at"], result
            assert "已合入" in result["message"], result

            base_after = git(["rev-parse", "main"], repo_ok).strip()
            project_after = git(["rev-parse", branch], repo_ok).strip()
            assert base_after == project_head == project_after, (
                base_after,
                project_head,
                project_after,
            )
            assert git(["rev-list", "--merges", "--count", "main"], repo_ok).strip() == "0"
            assert (
                git(["merge-base", "--is-ancestor", base_before, "main"], repo_ok) == ""
            )
            assert len(git(["log", "-1", "--format=%P", "main"], repo_ok).split()) == 1

            detail = server.call("GET", f"/api/projects/{project_id}")
            assert detail["project"]["merged_at"], detail["project"]
            assert detail["project"]["merged_commit"] == project_head, detail["project"]
            assert detail["merged_commit"] == project_head, detail
            assert detail["merged_into_base"] is True, detail
            assert any(
                event["kind"] == "project_merged" for event in detail["events"]
            ), detail["events"]

            commit_count = git(["rev-list", "--count", "main"], repo_ok).strip()
            merged_at = detail["project"]["merged_at"]
            repeat = server.call("POST", f"/api/projects/{project_id}/merge")
            assert repeat["status"] == "up_to_date", repeat
            assert "已是最新" in repeat["message"], repeat
            assert git(["rev-parse", "main"], repo_ok).strip() == base_after
            assert git(["rev-parse", branch], repo_ok).strip() == project_after
            assert git(["rev-list", "--count", "main"], repo_ok).strip() == commit_count
            detail = server.call("GET", f"/api/projects/{project_id}")
            assert detail["project"]["merged_at"] == merged_at, detail["project"]
            assert detail["merged_into_base"] is True, detail

            # ---- 合入后追加任务：旧合入记录失效，重新提供合入入口 ----------------
            print("--> 合入后追加任务")
            server.call(
                "POST",
                f"/api/projects/{project_id}/tasks",
                {
                    "key": "M1",
                    "title": "追加任务",
                    "description": "在 domo_mock/M1.txt 写入追加内容。",
                    "acceptance": ["domo_mock/M1.txt 存在且包含 M1"],
                    "deps": [],
                    "milestone": False,
                },
            )
            reopened = wait_project(
                project_id,
                lambda d: d["project"]["stage"] == "summarizing"
                and bool(d["project"]["report"]),
                "追加任务后的报告",
            )
            assert reopened["project"]["merged_at"] is None, reopened["project"]
            assert reopened["merged_into_base"] is False, reopened
            server.call(
                "POST",
                f"/api/projects/{project_id}/report",
                {"action": "accept", "feedback": None},
            )
            completed_again = wait_project(
                project_id,
                lambda d: d["project"]["stage"] == "completed",
                "追加任务后完成",
            )
            assert completed_again["project"]["merged_at"] is None, completed_again["project"]
            assert completed_again["merged_into_base"] is False, completed_again
            again = server.call("POST", f"/api/projects/{project_id}/merge")
            assert again["status"] == "merged", again
            after_again = server.call("GET", f"/api/projects/{project_id}")
            assert after_again["merged_into_base"] is True, after_again

            # ---- 外部合入后隐藏合入入口 ------------------------------------------
            print("--> 外部合入检测")
            manual = complete_task_project(repo_ok, "ManualMerged")
            manual_id = manual["project"]["id"]
            manual_branch, manual_head, _ = branch_heads(manual)
            assert manual["merged_into_base"] is False, manual
            assert not manual["project"]["merged_at"], manual["project"]
            git(["merge", "--ff-only", manual_branch], repo_ok)
            manual_after = server.call("GET", f"/api/projects/{manual_id}")
            assert manual_after["merged_into_base"] is True, manual_after
            assert not manual_after["project"]["merged_at"], manual_after["project"]
            manual_result = server.call("POST", f"/api/projects/{manual_id}/merge")
            assert manual_result["status"] == "up_to_date", manual_result
            assert git(["rev-parse", "main"], repo_ok).strip() == manual_head

            # ---- base 未被 worktree 检出时更新引用 -------------------------------
            print("--> base 未检出时更新引用")
            detail = complete_project(repo_loose, "MergeLoose", base="release")
            loose_id = detail["project"]["id"]
            loose_branch, loose_project_head, _ = branch_heads(detail)
            assert (
                git(["rev-parse", "--abbrev-ref", "HEAD"], repo_loose).strip() == "main"
            )
            loose_main_before = git(["rev-parse", "main"], repo_loose).strip()
            loose_base_before = git(["rev-parse", "release"], repo_loose).strip()
            assert loose_project_head != loose_base_before

            loose = server.call("POST", f"/api/projects/{loose_id}/merge")
            assert loose["status"] == "merged", loose
            assert git(["rev-parse", "release"], repo_loose).strip() == loose_project_head
            assert git(["rev-parse", loose_branch], repo_loose).strip() == loose_project_head
            assert git(["rev-parse", "main"], repo_loose).strip() == loose_main_before
            assert (
                git(["rev-list", "--merges", "--count", "release"], repo_loose).strip()
                == "0"
            )

            # ---- base worktree 有未提交改动时拒绝 --------------------------------
            print("--> 脏 worktree 拒绝合入")
            detail = complete_project(repo_dirty, "MergeDirty")
            dirty_id = detail["project"]["id"]
            dirty_branch, dirty_project_head, _ = branch_heads(detail)
            dirty_base_before = git(["rev-parse", "main"], repo_dirty).strip()
            assert dirty_project_head != dirty_base_before

            dirty_path = os.path.join(repo_dirty, "dirty.txt")
            with open(dirty_path, "w", encoding="utf-8") as handle:
                handle.write("uncommitted\n")
            bad = expect_bad("POST", f"/api/projects/{dirty_id}/merge")
            assert "未提交改动" in bad, bad
            assert git(["rev-parse", "main"], repo_dirty).strip() == dirty_base_before
            assert git(["rev-parse", dirty_branch], repo_dirty).strip() == dirty_project_head
            os.remove(dirty_path)

            # ---- 冲突时中止并回滚 ------------------------------------------------
            print("--> 冲突回滚")
            detail = complete_project(repo_conflict, "MergeConflict")
            conflict_id = detail["project"]["id"]
            conflict_branch, conflict_project_head, conflict_worktree = branch_heads(detail)

            os.makedirs(os.path.join(repo_conflict, "domo_mock"), exist_ok=True)
            with open(
                os.path.join(repo_conflict, "domo_mock", "direct.txt"),
                "w",
                encoding="utf-8",
            ) as handle:
                handle.write("base conflict\n")
            git(["add", "domo_mock/direct.txt"], repo_conflict)
            git(
                [
                    "-c",
                    "user.name=test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "-q",
                    "-m",
                    "conflicting base change",
                ],
                repo_conflict,
            )
            conflict_base_before = git(["rev-parse", "main"], repo_conflict).strip()

            bad = expect_bad("POST", f"/api/projects/{conflict_id}/merge")
            assert "冲突" in bad, bad
            assert "domo_mock/direct.txt" in bad, bad

            assert git(["rev-parse", "main"], repo_conflict).strip() == conflict_base_before
            assert (
                git(["rev-parse", conflict_branch], repo_conflict).strip()
                == conflict_project_head
            )
            rebase_check = subprocess.run(
                ["git", "-C", str(conflict_worktree), "rebase", "--show-current-patch"],
                capture_output=True,
                text=True,
            )
            assert rebase_check.returncode != 0, "rebase 未中止"
            assert (
                git(["rev-list", "--merges", "--count", "main"], repo_conflict).strip()
                == "0"
            )

            print("全部合入测试通过")


if __name__ == "__main__":
    main()
