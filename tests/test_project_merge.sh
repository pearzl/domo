#!/usr/bin/env bash
# DOMO 项目分支 rebase 合入测试。
#
# 覆盖范围：
#   1. 未完成项目调用合入接口返回 400；
#   2. 完成项目合入后项目分支 rebase 到 base，base 快进到项目分支提交，
#      历史保持线性且没有 merge commit；
#   3. 重复合入返回“已是最新”，不新增提交、不改变引用；
#   4. base 在 worktree 检出且有未提交改动时拒绝合入，两侧引用不变；
#   5. rebase 冲突时中止并回滚，两侧引用不变，错误信息列出冲突文件；
#   6. 目标分支已包含项目分支提交时（包括在 DOMO 之外完成的合入），
#      详情返回 merged_into_base；
#   7. 合入后追加任务会清空合入记录，新工作完成后重新提供合入入口。
#
# 约定：使用内置 mock Agent 与本地临时仓库，不访问外网；临时数据全部放在
# ${TMPDIR:-/tmp}。退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-project-merge.$$"
PORT="${DOMO_MERGE_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)}"
SERVER_PID=""

cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

for repo in ok dirty conflict loose; do
  mkdir -p "$TMP_ROOT/$repo"
  git -C "$TMP_ROOT/$repo" init -q -b main
  printf 'base %s\n' "$repo" >"$TMP_ROOT/$repo/README.md"
  git -C "$TMP_ROOT/$repo" add README.md
  git -C "$TMP_ROOT/$repo" -c user.name=test -c user.email=test@example.com \
    commit -q -m "init"
done
# loose 仓库的 base 分支没有被任何 worktree 检出，合入走引用更新。
git -C "$TMP_ROOT/loose" branch release main

echo "==> 构建后端"
cargo build --manifest-path "$ROOT/server/Cargo.toml" >/dev/null

echo "==> 启动 DOMO (port $PORT)"
DOMO_DATA_ROOT="$TMP_ROOT/data" \
DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
DOMO_LOG_ROOT="$TMP_ROOT/log" \
DOMO_SERVER_PORT="$PORT" \
DOMO_MOCK_DELAY_MS=40 \
"$ROOT/server/target/debug/domo-server" >"$TMP_ROOT/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 100); do
  if curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null; then
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "服务启动失败" >&2
    tail -80 "$TMP_ROOT/server.log" >&2
    exit 1
  fi
  sleep 0.1
done

curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null

python3 - "$PORT" "$TMP_ROOT/ok" "$TMP_ROOT/dirty" "$TMP_ROOT/conflict" "$TMP_ROOT/loose" <<'PY'
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

base_url = f"http://127.0.0.1:{sys.argv[1]}"
repo_ok, repo_dirty, repo_conflict = sys.argv[2], sys.argv[3], sys.argv[4]
repo_loose = sys.argv[5]


def call_raw(method, path, data=None):
    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(
        base_url + path,
        data=body,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"{method} {path} -> {error.code}: {error.read().decode()}") from error


def expect_bad(method, path, data=None):
    try:
        call_raw(method, path, data)
    except RuntimeError as error:
        text = str(error)
        assert "400" in text, text
        return text
    raise AssertionError(f"{method} {path} 应当返回 400")


def wait_for(project_id, predicate, label, timeout=90):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = call_raw("GET", f"/api/projects/{project_id}")
        if predicate(last):
            return last
        time.sleep(0.2)
    raise AssertionError(f"等待 {label} 超时：{last['project'] if last else None}")


def git(repo, *args):
    return subprocess.run(
        ["git", "-C", repo, *args],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def make_project(repo, name, base="main"):
    project = call_raw(
        "POST",
        "/api/projects",
        {"name": name, "repo_path": repo, "base_branch": base},
    )["project"]
    call_raw(
        "POST",
        f"/api/projects/{project['id']}/analysis/messages",
        {"message": "[direct] 生成合并测试文件", "confirm_mode": "auto"},
    )
    return project


def complete_project(repo, name, base="main"):
    project = make_project(repo, name, base)
    project_id = project["id"]
    wait_for(
        project_id,
        lambda d: d["project"]["stage"] == "summarizing"
        and bool(d["project"]["report"]),
        "项目报告",
    )
    call_raw(
        "POST",
        f"/api/projects/{project_id}/report",
        {"action": "accept", "feedback": None},
    )
    detail = wait_for(
        project_id,
        lambda d: d["project"]["stage"] == "completed",
        "项目完成",
    )
    return detail


def complete_task_project(repo, name):
    project = call_raw(
        "POST",
        "/api/projects",
        {"name": name, "repo_path": repo, "base_branch": "main"},
    )["project"]
    project_id = project["id"]
    call_raw(
        "POST",
        f"/api/projects/{project_id}/analysis/messages",
        {"message": "[single] 外部合入检测", "confirm_mode": "auto"},
    )
    wait_for(
        project_id,
        lambda d: d["project"]["stage"] == "summarizing"
        and bool(d["project"]["report"]),
        "项目报告",
    )
    call_raw(
        "POST",
        f"/api/projects/{project_id}/report",
        {"action": "accept", "feedback": None},
    )
    return wait_for(
        project_id,
        lambda d: d["project"]["stage"] == "completed",
        "项目完成",
    )


def branch_heads(detail):
    branch = detail["project"]["branch"]
    workspace = os.path.join(detail["project"]["workspace"], "repo")
    return branch, git(workspace, "rev-parse", branch), workspace


agents = call_raw("GET", "/api/agents")
for agent in agents:
    call_raw("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
mock = call_raw(
    "POST",
    "/api/agents",
    {
        "name": "Merge Mock",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 3,
    },
)

# ---- 未完成项目拒绝 ----------------------------------------------------------
print("--> 未完成项目拒绝")
pending = make_project(repo_ok, "Pending")
bad = expect_bad("POST", f"/api/projects/{pending['id']}/merge")
assert "未完成" in bad, bad

# ---- 成功合入 + 重复合入 -----------------------------------------------------
print("--> 成功合入与重复合入")
detail = complete_project(repo_ok, "MergeOk")
project_id = detail["project"]["id"]
branch, project_head, worktree = branch_heads(detail)
base_before = git(repo_ok, "rev-parse", "main")
assert project_head != base_before, (project_head, base_before)
assert detail["merged_into_base"] is False, detail

result = call_raw("POST", f"/api/projects/{project_id}/merge")
assert result["status"] == "merged", result
assert result["merged_commit"] == project_head, result
assert result["merged_at"], result
assert "已合入" in result["message"], result

base_after = git(repo_ok, "rev-parse", "main")
project_after = git(repo_ok, "rev-parse", branch)
assert base_after == project_head == project_after, (base_after, project_head, project_after)
assert git(repo_ok, "rev-list", "--merges", "--count", "main") == "0"
assert git(repo_ok, "merge-base", "--is-ancestor", base_before, "main") == ""
assert len(git(repo_ok, "log", "-1", "--format=%P", "main").split()) == 1

detail = call_raw("GET", f"/api/projects/{project_id}")
assert detail["project"]["merged_at"], detail["project"]
assert detail["project"]["merged_commit"] == project_head, detail["project"]
assert detail["merged_commit"] == project_head, detail
assert detail["merged_into_base"] is True, detail
assert any(event["kind"] == "project_merged" for event in detail["events"]), detail["events"]

commit_count = git(repo_ok, "rev-list", "--count", "main")
merged_at = detail["project"]["merged_at"]
repeat = call_raw("POST", f"/api/projects/{project_id}/merge")
assert repeat["status"] == "up_to_date", repeat
assert "已是最新" in repeat["message"], repeat
assert git(repo_ok, "rev-parse", "main") == base_after
assert git(repo_ok, "rev-parse", branch) == project_after
assert git(repo_ok, "rev-list", "--count", "main") == commit_count
detail = call_raw("GET", f"/api/projects/{project_id}")
assert detail["project"]["merged_at"] == merged_at, detail["project"]
assert detail["merged_into_base"] is True, detail

# ---- 合入后追加任务：旧合入记录失效，重新提供合入入口 ------------------------
print("--> 合入后追加任务")
call_raw(
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
reopened = wait_for(
    project_id,
    lambda d: d["project"]["stage"] == "summarizing" and bool(d["project"]["report"]),
    "追加任务后的报告",
)
assert reopened["project"]["merged_at"] is None, reopened["project"]
assert reopened["merged_into_base"] is False, reopened
call_raw(
    "POST",
    f"/api/projects/{project_id}/report",
    {"action": "accept", "feedback": None},
)
completed_again = wait_for(
    project_id,
    lambda d: d["project"]["stage"] == "completed",
    "追加任务后完成",
)
assert completed_again["project"]["merged_at"] is None, completed_again["project"]
assert completed_again["merged_into_base"] is False, completed_again
again = call_raw("POST", f"/api/projects/{project_id}/merge")
assert again["status"] == "merged", again
after_again = call_raw("GET", f"/api/projects/{project_id}")
assert after_again["merged_into_base"] is True, after_again

# ---- 外部合入后不再显示合入入口 ----------------------------------------------
print("--> 外部合入检测")
manual = complete_task_project(repo_ok, "ManualMerged")
manual_id = manual["project"]["id"]
manual_branch, manual_head, _ = branch_heads(manual)
assert manual["merged_into_base"] is False, manual
assert not manual["project"]["merged_at"], manual["project"]
git(repo_ok, "merge", "--ff-only", manual_branch)
manual_after = call_raw("GET", f"/api/projects/{manual_id}")
assert manual_after["merged_into_base"] is True, manual_after
assert not manual_after["project"]["merged_at"], manual_after["project"]
manual_result = call_raw("POST", f"/api/projects/{manual_id}/merge")
assert manual_result["status"] == "up_to_date", manual_result
assert git(repo_ok, "rev-parse", "main") == manual_head

# ---- base 未被 worktree 检出时更新引用 --------------------------------------
print("--> base 未检出时更新引用")
detail = complete_project(repo_loose, "MergeLoose", base="release")
loose_id = detail["project"]["id"]
loose_branch, loose_project_head, _ = branch_heads(detail)
assert git(repo_loose, "rev-parse", "--abbrev-ref", "HEAD") == "main"
loose_main_before = git(repo_loose, "rev-parse", "main")
loose_base_before = git(repo_loose, "rev-parse", "release")
assert loose_project_head != loose_base_before

loose = call_raw("POST", f"/api/projects/{loose_id}/merge")
assert loose["status"] == "merged", loose
assert git(repo_loose, "rev-parse", "release") == loose_project_head
assert git(repo_loose, "rev-parse", loose_branch) == loose_project_head
assert git(repo_loose, "rev-parse", "main") == loose_main_before
assert git(repo_loose, "rev-list", "--merges", "--count", "release") == "0"

# ---- base worktree 有未提交改动时拒绝 ----------------------------------------
print("--> 脏 worktree 拒绝合入")
detail = complete_project(repo_dirty, "MergeDirty")
dirty_id = detail["project"]["id"]
dirty_branch, dirty_project_head, _ = branch_heads(detail)
dirty_base_before = git(repo_dirty, "rev-parse", "main")
assert dirty_project_head != dirty_base_before

with open(os.path.join(repo_dirty, "dirty.txt"), "w", encoding="utf-8") as handle:
    handle.write("uncommitted\n")
bad = expect_bad("POST", f"/api/projects/{dirty_id}/merge")
assert "未提交改动" in bad, bad
assert git(repo_dirty, "rev-parse", "main") == dirty_base_before
assert git(repo_dirty, "rev-parse", dirty_branch) == dirty_project_head
os.remove(os.path.join(repo_dirty, "dirty.txt"))

# ---- 冲突时中止并回滚 --------------------------------------------------------
print("--> 冲突回滚")
detail = complete_project(repo_conflict, "MergeConflict")
conflict_id = detail["project"]["id"]
conflict_branch, conflict_project_head, conflict_worktree = branch_heads(detail)

os.makedirs(os.path.join(repo_conflict, "domo_mock"), exist_ok=True)
with open(os.path.join(repo_conflict, "domo_mock", "direct.txt"), "w", encoding="utf-8") as handle:
    handle.write("base conflict\n")
git(repo_conflict, "add", "domo_mock/direct.txt")
git(
    repo_conflict,
    "-c",
    "user.name=test",
    "-c",
    "user.email=test@example.com",
    "commit",
    "-q",
    "-m",
    "conflicting base change",
)
conflict_base_before = git(repo_conflict, "rev-parse", "main")

bad = expect_bad("POST", f"/api/projects/{conflict_id}/merge")
assert "冲突" in bad, bad
assert "domo_mock/direct.txt" in bad, bad

assert git(repo_conflict, "rev-parse", "main") == conflict_base_before
assert git(repo_conflict, "rev-parse", conflict_branch) == conflict_project_head
rebase_check = subprocess.run(
    ["git", "-C", conflict_worktree, "rebase", "--show-current-patch"],
    capture_output=True,
    text=True,
)
assert rebase_check.returncode != 0, "rebase 未中止"
assert git(repo_conflict, "rev-list", "--merges", "--count", "main") == "0"

print("全部合入测试通过")
PY

echo "==> 项目合入测试通过"
