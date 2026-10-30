// Cross-platform DOMO smoke test.
//
// Covers:
//   1. the server builds and starts with a temporary data root;
//   2. the built-in mock planner creates a two-task DAG;
//   3. tasks run in Git worktrees and merge into the project branch;
//   4. the report is produced and can be accepted.
//
// Uses only Node.js built-in modules and the mock agent; no network model calls
// and no npm dependencies. Runtime data stays under the system temp directory.
// Exit code 0 means every check passed.
//
// Run with: node tests/test_smoke.mjs

import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const isWindows = process.platform === "win32";
const executable = (name) => (isWindows ? `${name}.exe` : name);

let child = null;
let stdout = "";
let stderr = "";
let base = "";

function assert(condition, message) {
  if (!condition) {
    throw new Error(`Assertion failed: ${message}`);
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => resolve(address.port));
    });
  });
}

function git(args, cwd) {
  const result = spawnSync(executable("git"), args, {
    cwd,
    encoding: "utf8",
    windowsHide: true,
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(`git ${args.join(" ")} failed: ${result.stderr.trim()}`);
  }
  return result.stdout;
}

async function api(method, path, body) {
  const response = await fetch(base + path, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`${method} ${path} -> ${response.status}: ${text}`);
  }
  return text ? JSON.parse(text) : null;
}

async function waitFor(check, label, timeoutMs = 90000) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    last = await check();
    if (last) {
      return last;
    }
    await sleep(200);
  }
  throw new Error(`Timed out waiting for ${label}`);
}

async function waitForHealth() {
  for (let attempt = 0; attempt < 150; attempt++) {
    if (child.exitCode !== null) {
      throw new Error(
        `server exited with code ${child.exitCode}\n--- stdout ---\n${stdout}\n--- stderr ---\n${stderr}`,
      );
    }
    try {
      const response = await fetch(`${base}/api/health`);
      if (response.ok) {
        return response.json();
      }
    } catch {
      // The listener is not ready yet.
    }
    await sleep(100);
  }
  throw new Error(
    `server did not become healthy\n--- stdout ---\n${stdout}\n--- stderr ---\n${stderr}`,
  );
}

async function stopServer() {
  if (!child || child.exitCode !== null) {
    return;
  }
  if (isWindows) {
    spawnSync(executable("taskkill"), ["/PID", String(child.pid), "/T", "/F"], {
      stdio: "ignore",
      windowsHide: true,
    });
  } else {
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch {
      try {
        child.kill("SIGKILL");
      } catch {
        // Already gone.
      }
    }
  }
  await new Promise((resolve) => {
    if (child.exitCode !== null) {
      resolve();
      return;
    }
    child.once("exit", resolve);
    setTimeout(resolve, 5000);
  });
}

async function main() {
  const tempRoot = await mkdtemp(join(tmpdir(), "domo-node-smoke-"));
  const repo = join(tempRoot, "repo");
  try {
    await mkdir(repo, { recursive: true });
    console.log("==> init test repository");
    git(["init", "-q", "-b", "main"], repo);
    git(
      ["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "--allow-empty", "-q", "-m", "init"],
      repo,
    );

    console.log("==> build server");
    const build = spawnSync(
      executable("cargo"),
      ["build", "--manifest-path", join(root, "server", "Cargo.toml")],
      { stdio: "inherit", windowsHide: true },
    );
    if (build.error) {
      throw build.error;
    }
    assert(build.status === 0, `cargo build exited with ${build.status}`);

    const port = await freePort();
    base = `http://127.0.0.1:${port}`;
    const server = join(
      root,
      "server",
      "target",
      "debug",
      isWindows ? "domo-server.exe" : "domo-server",
    );
    assert(existsSync(server), `server binary not found: ${server}`);

    console.log(`==> start server on port ${port}`);
    child = spawn(server, [], {
      cwd: root,
      env: {
        ...process.env,
        DOMO_DATA_ROOT: join(tempRoot, "data"),
        DOMO_WORKSPACE_ROOT: join(tempRoot, "ws"),
        DOMO_LOG_ROOT: join(tempRoot, "log"),
        DOMO_SERVER_PORT: String(port),
        DOMO_MOCK_DELAY_MS: "40",
      },
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
      detached: !isWindows,
    });
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });

    const health = await waitForHealth();
    assert(health.ok === true, "health check failed");
    if (existsSync(join(root, "web", "dist", "index.html"))) {
      const index = await fetch(`${base}/`);
      assert(index.status === 200, `web UI returned ${index.status}`);
    }

    console.log("==> register mock agent");
    const agents = await api("GET", "/api/agents");
    for (const agent of agents) {
      await api("PATCH", `/api/agents/${agent.id}`, { enabled: false });
    }
    await api("POST", "/api/agents", {
      name: "Node Smoke Mock",
      kind: "mock",
      roles: ["planner", "executor", "verifier"],
      max_concurrency: 2,
    });

    console.log("==> create project and start analysis");
    const created = await api("POST", "/api/projects", {
      name: "Node Smoke",
      repo_path: repo,
      base_branch: "main",
    });
    const projectId = created.project.id;
    await api("POST", `/api/projects/${projectId}/analysis/messages`, {
      message: "generate two dependent mock files",
      confirm_mode: "auto",
    });

    const detail = await waitFor(async () => {
      const current = await api("GET", `/api/projects/${projectId}`);
      return current.project.stage === "summarizing" && current.project.report ? current : null;
    }, "project report");

    assert(detail.tasks.length === 2, `expected two tasks, got ${detail.tasks.length}`);
    for (const task of detail.tasks) {
      assert(task.stage === "completed", `task ${task.key} is ${task.stage}`);
    }
    assert(detail.jobs.length > 0, "no jobs were recorded");
    assert(detail.events.length > 0, "no events were recorded");

    const branch = detail.project.branch;
    const t1 = git(["show", `${branch}:domo_mock/T1.txt`], repo);
    assert(t1.includes("T1"), "T1 artifact has unexpected content");
    const t2 = git(["show", `${branch}:domo_mock/T2.txt`], repo);
    assert(t2.includes("dep T1 was merged"), "T2 artifact has unexpected content");

    console.log("==> accept report");
    await api("POST", `/api/projects/${projectId}/report`, { action: "accept" });
    const completed = await api("GET", `/api/projects/${projectId}`);
    assert(completed.project.stage === "completed", `project stage is ${completed.project.stage}`);

    console.log("Node smoke test passed");
    console.log(`  project branch: ${branch}`);
    console.log(`  jobs: ${detail.jobs.length}`);
    console.log(`  events: ${detail.events.length}`);
  } finally {
    await stopServer();
    await rm(tempRoot, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  }
}

main().then(
  () => process.exit(0),
  (error) => {
    console.error(`FAILED: ${error.message}`);
    if (stdout.trim()) {
      console.error(`--- server stdout ---\n${stdout}`);
    }
    if (stderr.trim()) {
      console.error(`--- server stderr ---\n${stderr}`);
    }
    process.exit(1);
  },
);
