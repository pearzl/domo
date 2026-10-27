#!/usr/bin/env bash
# DOMO 默认团队配置测试。
#
# 覆盖范围：
#   1. 空库启动时默认团队按种子 Agent 初始化，三个角色都有条目；
#   2. GET/POST/PATCH/DELETE /api/team/default 的加入、上下游移（相邻交换、
#      编号连续）、启停与移除行为，以及角色/重复/未知条目的校验；
#   3. 全局 Agent 创建/更新不接收优先级字段，视图不再输出优先级；
#   4. 新建项目后项目池与默认团队快照一致（角色、顺序、启停）；
#   5. 调整默认团队后再次新建项目反映新配置，旧项目池保持不变；
#   6. 删除全局 Agent 时默认团队同步移除并重新编号。
#
# 使用内置 mock Agent，不访问外网；临时数据全部放在 ${TMPDIR:-/tmp}。
# 项目停留在目标确认阶段，不派发任何作业。退出码 0 表示全部通过。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/domo-default-team.$$"
PORT="${DOMO_DEFAULT_TEAM_PORT:-$(python3 - <<'PY'
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

mkdir -p "$TMP_ROOT/repo"
git -C "$TMP_ROOT/repo" init -q -b main
git -C "$TMP_ROOT/repo" -c user.name=test -c user.email=test@example.com \
  commit --allow-empty -q -m "init"

echo "==> 构建后端"
cargo build --manifest-path "$ROOT/server/Cargo.toml" >/dev/null

start_server() {
  DOMO_DATA_ROOT="$TMP_ROOT/data" \
  DOMO_WORKSPACE_ROOT="$TMP_ROOT/ws" \
  DOMO_LOG_ROOT="$TMP_ROOT/log" \
  DOMO_SERVER_PORT="$PORT" \
  "$ROOT/server/target/debug/domo-server" >>"$TMP_ROOT/server.log" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 100); do
    if curl -sf "http://127.0.0.1:$PORT/api/health" >/dev/null; then
      return 0
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "服务启动失败" >&2
      tail -120 "$TMP_ROOT/server.log" >&2
      exit 1
    fi
    sleep 0.1
  done
  echo "等待服务启动超时" >&2
  exit 1
}

echo "==> 启动 DOMO (port $PORT)"
start_server

python3 - "$PORT" "$TMP_ROOT/repo" "$TMP_ROOT/state.json" <<'PY'
import json
import sys
import time
import urllib.error
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
repo = sys.argv[2]
state_path = sys.argv[3]


_pending_goals = {}


def call(method, path, data=None):
    if method == "POST" and path == "/api/projects" and isinstance(data, dict) and "goal" in data:
        goal = data.get("goal")
        payload = {key: value for key, value in data.items() if key != "goal"}
        result = _call_raw("POST", path, payload)
        project_id = result["project"]["id"]
        _pending_goals[project_id] = goal
        return result
    if (
        method == "POST"
        and path.startswith("/api/projects/")
        and path.endswith("/confirm")
        and not path.endswith("/goal/confirm")
    ):
        project_id = path.split("/")[-2]
        detail = _call_raw("GET", f"/api/projects/{project_id}")
        if not detail["messages"] and project_id in _pending_goals:
            _call_raw(
                "POST",
                f"/api/projects/{project_id}/goal/messages",
                {
                    "message": _pending_goals.pop(project_id),
                    "goal_confirm_mode": "auto",
                },
            )
        message = (data or {}).get("message")
        if message:
            _call_raw(
                "POST",
                f"/api/projects/{project_id}/goal/messages",
                {"message": message, "goal_confirm_mode": "auto"},
            )
        deadline = time.time() + 30
        while time.time() < deadline:
            detail = _call_raw("GET", f"/api/projects/{project_id}")
            if detail["project"]["stage"] != "goal_confirm":
                return detail
            if detail.get("goal_proposal") and not detail["jobs"]:
                return _call_raw(
                    "POST",
                    f"/api/projects/{project_id}/goal/confirm",
                    {"message": None},
                )
            last = detail["messages"][-1] if detail["messages"] else None
            if last and last["role"] == "planner" and not detail.get("goal_proposal"):
                return detail
            time.sleep(0.1)
        return _call_raw("GET", f"/api/projects/{project_id}")
    return _call_raw(method, path, data)


def _call_raw(method, path, data=None):
    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(
        base + path,
        data=body,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        return {"__status__": error.code, "error": error.read().decode()}


def expect_status(method, path, data, status):
    result = call(method, path, data)
    code = result.get("__status__")
    assert code == status, (method, path, status, result)


def roles_of(group_name, payload):
    return {group["role"]: group["entries"] for group in payload["roles"]}


def ids(entries):
    return [entry["agent_id"] for entry in entries]


def priorities(entries):
    return [entry["priority"] for entry in entries]


# 1. 空库启动：默认团队按种子 Agent 初始化，三个角色各有一个启用的条目。
team = call("GET", "/api/team/default")
assert "__status__" not in team, team
assert team["project_id"] == "default", team
team_roles = roles_of("team", team)
assert set(team_roles) == {"planner", "executor", "verifier"}, team_roles
for role, entries in team_roles.items():
    assert len(entries) == 1, entries
    assert entries[0]["name"] == "OpenCode 默认团队", entries
    assert entries[0]["kind"] == "opencode", entries
    assert entries[0]["enabled"] is True, entries
    assert priorities(entries) == [0], entries

default_agent_id = team_roles["planner"][0]["agent_id"]
assert (
    default_agent_id
    == team_roles["executor"][0]["agent_id"]
    == team_roles["verifier"][0]["agent_id"]
), team_roles

# 2. 默认团队加入：新条目排在末尾，编号连续。
alpha = call(
    "POST",
    "/api/agents",
    {
        "name": "Alpha Mock",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 2,
    },
)
assert "__status__" not in alpha, alpha
assert "priority" not in alpha, "全局 Agent 不应再暴露优先级"
for role in ("planner", "executor", "verifier"):
    call("POST", "/api/team/default", {"role": role, "agent_id": alpha["id"]})

beta = call(
    "POST",
    "/api/agents",
    {
        "name": "Beta Mock",
        "kind": "mock",
        "roles": ["planner", "executor", "verifier"],
        "max_concurrency": 2,
    },
)
for role in ("planner", "executor", "verifier"):
    call("POST", "/api/team/default", {"role": role, "agent_id": beta["id"]})

team = call("GET", "/api/team/default")
team_roles = roles_of("team", team)
for role, entries in team_roles.items():
    assert ids(entries) == [default_agent_id, alpha["id"], beta["id"]], entries
    assert priorities(entries) == [0, 1, 2], entries
    assert all(entry["enabled"] for entry in entries), entries

# 加入校验：重复加入、Agent 不具备角色、未知角色都被拒绝。
expect_status("POST", "/api/team/default", {"role": "planner", "agent_id": alpha["id"]}, 400)
solo = call(
    "POST",
    "/api/agents",
    {"name": "Solo Executor", "kind": "mock", "roles": ["executor"]},
)
expect_status("POST", "/api/team/default", {"role": "planner", "agent_id": solo["id"]}, 400)
expect_status(
    "POST", "/api/team/default", {"role": "watcher", "agent_id": alpha["id"]}, 400
)
expect_status(
    "PATCH",
    f"/api/team/default/planner/{alpha['id']}",
    {"direction": "sideways"},
    400,
)
expect_status(
    "DELETE", f"/api/team/default/planner/agt_missing", None, 404
)

# 3. 上下移调整：相邻交换，编号保持连续。
call("PATCH", f"/api/team/default/planner/{beta['id']}", {"direction": "up"})
team = call("GET", "/api/team/default")
planner_entries = roles_of("team", team)["planner"]
assert ids(planner_entries) == [default_agent_id, beta["id"], alpha["id"]], planner_entries
assert priorities(planner_entries) == [0, 1, 2], planner_entries

call("PATCH", f"/api/team/default/planner/{beta['id']}", {"direction": "up"})
team = call("GET", "/api/team/default")
planner_entries = roles_of("team", team)["planner"]
assert ids(planner_entries) == [beta["id"], default_agent_id, alpha["id"]], planner_entries
assert priorities(planner_entries) == [0, 1, 2], planner_entries

# 4. 全局禁用默认 Agent，默认团队视图与项目池继承的默认条目同步停用。
call("PATCH", f"/api/agents/{default_agent_id}", {"enabled": False})
team = call("GET", "/api/team/default")
team_roles = roles_of("team", team)
planner_entries = team_roles["planner"]
assert planner_entries[1]["agent_id"] == default_agent_id, planner_entries
assert planner_entries[1]["enabled"] is False, planner_entries
assert (
    next(
        entry
        for entry in team_roles["executor"]
        if entry["agent_id"] == default_agent_id
    )["enabled"]
    is False
), team_roles["executor"]

# 5. 新建项目的项目池继承默认团队（角色、顺序、启停）。
project = call(
    "POST",
    "/api/projects",
    {"name": "Inherit", "goal": "项目池继承默认团队", "repo_path": repo, "base_branch": "main"},
)["project"]
pool = call("GET", f"/api/projects/{project['id']}/agents")
pool_roles = roles_of("pool", pool)
for role, entries in pool_roles.items():
    expected = team_roles[role]
    assert ids(entries) == ids(expected), (role, entries, expected)
    assert priorities(entries) == priorities(expected), (role, entries, expected)
    assert [entry["enabled"] for entry in entries] == [
        entry["enabled"] for entry in expected
    ], (role, entries, expected)

# 在默认团队中停用 Alpha 的执行角色，新项目继续反映；旧项目池保持。
call("PATCH", f"/api/team/default/executor/{alpha['id']}", {"enabled": False})
team = call("GET", "/api/team/default")
team_roles = roles_of("team", team)
alpha_executor = next(
    entry
    for entry in team_roles["executor"]
    if entry["agent_id"] == alpha["id"]
)
assert alpha_executor["enabled"] is False, team_roles["executor"]

project2 = call(
    "POST",
    "/api/projects",
    {"name": "Inherit2", "goal": "第二次继承反映默认团队调整", "repo_path": repo, "base_branch": "main"},
)["project"]
pool2 = call("GET", f"/api/projects/{project2['id']}/agents")
pool2_roles = roles_of("pool", pool2)
assert ids(pool2_roles["executor"]) == ids(team_roles["executor"]), pool2_roles["executor"]
assert (
    next(
        entry
        for entry in pool2_roles["executor"]
        if entry["agent_id"] == alpha["id"]
    )["enabled"]
    is False
), pool2_roles["executor"]

pool = call("GET", f"/api/projects/{project['id']}/agents")
assert ids(roles_of("pool", pool)["executor"]) == ids(pool2_roles["executor"]), pool
assert (
    next(
        entry
        for entry in roles_of("pool", pool)["executor"]
        if entry["agent_id"] == alpha["id"]
    )["enabled"]
    is True
), "旧项目池不应跟随默认团队变化"

# 6. 移除条目后重新编号，再新建项目的池同步收缩。
call("DELETE", f"/api/team/default/planner/{alpha['id']}")
team = call("GET", "/api/team/default")
planner_entries = roles_of("team", team)["planner"]
assert ids(planner_entries) == [beta["id"], default_agent_id], planner_entries
assert priorities(planner_entries) == [0, 1], planner_entries

project3 = call(
    "POST",
    "/api/projects",
    {"name": "Inherit3", "goal": "移除后再次继承", "repo_path": repo, "base_branch": "main"},
)["project"]
pool3 = call("GET", f"/api/projects/{project3['id']}/agents")
assert ids(roles_of("pool", pool3)["planner"]) == [beta["id"], default_agent_id], pool3

# 7. 删除全局 Agent 时默认团队同步移除并重新编号。
call("DELETE", f"/api/agents/{beta['id']}")
team = call("GET", "/api/team/default")
planner_entries = roles_of("team", team)["planner"]
assert ids(planner_entries) == [default_agent_id], planner_entries
assert priorities(planner_entries) == [0], planner_entries
assert ids(roles_of("team", team)["executor"]) == [
    default_agent_id,
    alpha["id"],
], team

# 旧项目的池保持快照语义，被删除的全局 Agent 条目标记为不可用。
pool = call("GET", f"/api/projects/{project3['id']}/agents")
beta_entry = next(
    entry
    for entry in roles_of("pool", pool)["planner"]
    if entry["agent_id"] == beta["id"]
)
assert beta_entry["available"] is False, beta_entry
assert "删除" in (beta_entry["unavailable_reason"] or ""), beta_entry

json.dump(
    {
        "default_agent_id": default_agent_id,
        "project3_id": project3["id"],
    },
    open(state_path, "w", encoding="utf-8"),
    ensure_ascii=False,
)
print("默认团队初始化、编辑与项目继承测试通过")
PY

echo "==> 重启服务，验证默认团队配置持久化"
kill "$SERVER_PID"
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
start_server

python3 - "$PORT" "$TMP_ROOT/state.json" <<'PY'
import json
import sys
import urllib.request

base = f"http://127.0.0.1:{sys.argv[1]}"
state = json.load(open(sys.argv[2], encoding="utf-8"))


def call(path):
    with urllib.request.urlopen(base + path) as response:
        return json.load(response)


team = call("/api/team/default")
planner = next(group for group in team["roles"] if group["role"] == "planner")
assert [entry["priority"] for entry in planner["entries"]] == list(
    range(len(planner["entries"]))
), planner
assert all(entry["agent_id"] == state["default_agent_id"] for entry in planner["entries"]), planner

pool = call(f"/api/projects/{state['project3_id']}/agents")
planner_pool = next(group for group in pool["roles"] if group["role"] == "planner")
assert [entry["priority"] for entry in planner_pool["entries"]] == list(
    range(len(planner_pool["entries"]))
), planner_pool

print("默认团队配置重启后保持")
PY

echo "==> 通过：$(basename "$0")"
