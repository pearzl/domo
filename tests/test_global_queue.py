#!/usr/bin/env python3
"""跨项目 Agent 全局队列、人工调序、暂停资格与重启持久化。
mock Agent；不调用真实模型。
"""
from harness import Server, init_repo, temp_root, wait_for


def main():
    with temp_root("domo-global-queue-") as root:
        repo = root / "repo"
        init_repo(repo)
        with Server(root, env={"DOMO_MOCK_DELAY_MS": "40", "DOMO_MOCK_EXECUTOR_DELAY_MS": "10000"}) as server:
            call = server.call
            for agent in call("GET", "/api/agents"):
                call("PATCH", f"/api/agents/{agent['id']}", {"enabled": False})
            call("POST", "/api/agents", {"name": "planner", "kind": "mock", "roles": ["planner", "verifier"], "max_concurrency": 10})
            executor = call("POST", "/api/agents", {"name": "one executor", "kind": "mock", "roles": ["executor"], "max_concurrency": 1})
            ids = []
            for index in range(3):
                p = call("POST", "/api/projects", {"name": f"项目 {index}", "repo_path": str(repo), "goal": "[single] 编写结果", "confirm_mode": "auto"})["project"]
                ids.append(p["id"])
                if index == 0:
                    wait_for(lambda: any(j["role"] == "executor" and j["status"] == "running" for j in call("GET", f"/api/projects/{p['id']}")["jobs"]), "第一个执行者占用槽位", 15)
            def queued():
                items = [entry for entry in call("GET", "/api/queue")["queued"] if entry["agent_id"] == executor["id"]]
                return items if {ids[1], ids[2]} <= {entry["project_id"] for entry in items} else None
            entries = wait_for(queued, "跨项目入队", 15)
            call("PATCH", f"/api/agents/{executor['id']}", {"enabled": False})
            for pid in ids:
                call("POST", f"/api/projects/{pid}/pause", {"force": True})
            second = next(entry for entry in entries if entry["project_id"] == ids[2])
            reordered = call("PATCH", "/api/queue/" + second["id"], {"position": 0})
            expected = [item["id"] for item in reordered["queued"] if item["agent_id"] == executor["id"]]
            server.restart({"DOMO_MOCK_EXECUTOR_DELAY_MS": "200"})
            actual = [item["id"] for item in call("GET", "/api/queue")["queued"] if item["agent_id"] == executor["id"]]
            assert actual == expected, (actual, expected)
            # Resume both projects while holding capacity with the first one off.
            call("PATCH", f"/api/agents/{executor['id']}", {"enabled": False})
            call("POST", f"/api/projects/{ids[1]}/resume", {})
            call("POST", f"/api/projects/{ids[2]}/resume", {})
            call("PATCH", f"/api/agents/{executor['id']}", {"enabled": True})
            for pid in ids[1:]:
                wait_for(lambda: call("GET", f"/api/projects/{pid}")["project"]["report"], "排队项目完成", 30)
            jobs = [j for pid in ids[1:] for j in call("GET", f"/api/projects/{pid}")["jobs"] if j["role"] == "executor"]
            jobs.sort(key=lambda j: j["started_at"])
            assert jobs[0]["project_id"] == ids[2], jobs
            assert jobs[0]["finished_at"] <= jobs[1]["started_at"], jobs
            print("跨项目全局队列、人工顺序、暂停和重启持久化通过")


if __name__ == "__main__":
    main()
