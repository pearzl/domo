#!/usr/bin/env python3
"""DOMO 关闭流程测试。

场景：浏览器保持 SSE 长连接、执行者 Agent 子进程正在运行，向前端服务发送
SIGINT（Windows 上为 Ctrl+Break）。
验收标准：
  1. 服务在关闭宽限期内自行退出，不需要第二次信号或强制 kill；
  2. SSE 长连接随服务关闭而结束；
  3. 运行中的 Agent 子进程被终止，不留下孤儿进程。

使用内置 mock Agent 与本地 Python sleep 进程，不访问外网；临时数据全部放在
系统临时目录。
"""

import http.client
import sys
import threading
import time
from pathlib import Path

from harness import (
    Server,
    ensure_server_built,
    init_repo,
    process_alive,
    temp_root,
)

# 执行者 Agent：保持运行，直到服务在关闭时终止它。
SLEEPER = '''\
import sys
import time

time.sleep(300)
'''


class SseWatcher:
    """保持一条 SSE 长连接，并记录它何时被服务端关闭。"""

    def __init__(self, port):
        self.port = port
        self.status = None
        self.error = None
        self.closed = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=30)
        try:
            connection.request("GET", "/api/events/stream")
            response = connection.getresponse()
            self.status = response.status
            while response.read(1):
                pass
        except (OSError, http.client.HTTPException) as error:
            self.error = error
        finally:
            self.closed.set()
            try:
                connection.close()
            except OSError:
                pass

    def start(self):
        self._thread.start()
        return self

    def wait_open(self, timeout=10):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.status is not None:
                return self.status
            if self.closed.is_set():
                raise AssertionError(f"SSE 连接未建立: {self.error}")
            time.sleep(0.05)
        raise AssertionError("SSE 连接未建立")


def find_agent_pid(server, project_id, timeout=60):
    deadline = time.time() + timeout
    detail = None
    while time.time() < deadline:
        detail = server.call("GET", f"/api/projects/{project_id}")
        for job in detail["jobs"]:
            if job["role"] != "executor" or job["status"] != "running":
                continue
            pid_file = (
                Path(detail["project"]["workspace"]) / "jobs" / job["id"] / "pid"
            )
            if pid_file.exists():
                return int(pid_file.read_text(encoding="utf-8").split(":")[0])
        time.sleep(0.1)
    raise AssertionError(
        f"等待执行中作业超时：{detail['project']} "
        f"{[(t['key'], t['stage'], t['status']) for t in detail['tasks']]} "
        f"{[(j['role'], j['status'], j.get('error')) for j in detail['jobs']]}"
    )


def main():
    with temp_root("domo-shutdown-") as temp:
        repo = temp / "repo"
        init_repo(repo)

        sleeper = temp / "sleeper.py"
        sleeper.write_text(SLEEPER, encoding="utf-8")

        ensure_server_built()

        with Server(temp) as server:
            pending_goals = {}

            # 项目确认接口把创建时保存的目标作为分析消息提交。
            def call(method, path, data=None):
                if (
                    method == "POST"
                    and path == "/api/projects"
                    and isinstance(data, dict)
                    and "goal" in data
                ):
                    goal = data.get("goal")
                    payload = {key: value for key, value in data.items() if key != "goal"}
                    result = server.call("POST", path, payload)
                    pending_goals[result["project"]["id"]] = goal
                    return result
                if (
                    method == "POST"
                    and path.startswith("/api/projects/")
                    and path.endswith("/confirm")
                    and not path.endswith("/analysis/confirm")
                ):
                    project_id = path.split("/")[-2]
                    detail = server.call("GET", f"/api/projects/{project_id}")
                    if not detail["messages"] and project_id in pending_goals:
                        server.call(
                            "POST",
                            f"/api/projects/{project_id}/analysis/messages",
                            {
                                "message": pending_goals.pop(project_id),
                                "confirm_mode": "auto",
                            },
                        )
                    deadline = time.time() + 30
                    while time.time() < deadline:
                        detail = server.call("GET", f"/api/projects/{project_id}")
                        if detail["project"]["stage"] != "analysis":
                            return detail
                        if detail.get("proposal") and not detail["jobs"]:
                            return server.call(
                                "POST",
                                f"/api/projects/{project_id}/analysis/confirm",
                                {},
                            )
                        last = detail["messages"][-1] if detail["messages"] else None
                        if last and last["role"] == "planner" and not detail.get("proposal"):
                            return detail
                        time.sleep(0.1)
                    return server.call("GET", f"/api/projects/{project_id}")
                return server.call(method, path, data)

            print("==> 准备一个执行中的 CLI Agent 作业")
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})

            call(
                "POST",
                "/api/agents",
                {
                    "name": "Shutdown Planner",
                    "kind": "mock",
                    "roles": ["planner"],
                    "max_concurrency": 1,
                },
            )
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Shutdown Verifier",
                    "kind": "mock",
                    "roles": ["verifier"],
                    "max_concurrency": 1,
                },
            )
            call(
                "POST",
                "/api/agents",
                {
                    "name": "Shutdown Executor",
                    "kind": "cli",
                    "roles": ["executor"],
                    "max_concurrency": 1,
                    "command": [
                        sys.executable,
                        str(sleeper),
                        "{job_dir}",
                        "{cwd}",
                        str(temp),
                    ],
                },
            )

            project = call(
                "POST",
                "/api/projects",
                {
                    "name": "Shutdown",
                    "goal": "验证关闭流程",
                    "repo_path": str(repo),
                    "base_branch": "main",
                },
            )["project"]
            call("POST", f"/api/projects/{project['id']}/confirm", {})

            agent_pid = find_agent_pid(server, project["id"])

            print("==> 建立 SSE 长连接")
            watcher = SseWatcher(server.port).start()
            status = watcher.wait_open()
            assert status == 200, f"SSE 连接未建立: {status}"

            print("==> 发送中断信号")
            started = time.time()
            server.interrupt(timeout=15)
            elapsed_ms = int((time.time() - started) * 1000)
            assert elapsed_ms < 10000, f"服务退出耗时 {elapsed_ms}ms，超过关闭宽限期"

            print("==> 校验 SSE 连接与 Agent 子进程")
            assert watcher.closed.wait(10), "SSE 连接未随关闭结束"

            deadline = time.time() + 5
            while time.time() < deadline and process_alive(agent_pid):
                time.sleep(0.1)
            assert not process_alive(agent_pid), (
                f"Agent 子进程 {agent_pid} 仍在运行"
            )

            logs = server.logs()
            assert "DOMO 已关闭" in logs, "缺少正常关闭日志"
            assert "关闭超时" not in logs, "关闭走了超时强制退出路径"

            print("==> 通过：test_shutdown.py")


if __name__ == "__main__":
    main()
