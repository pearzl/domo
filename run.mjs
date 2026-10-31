#!/usr/bin/env node
// Rebuild the web bundle and start the server.
//
// Usage:
//   node run.mjs                 # release build, then serve on http://127.0.0.1:8787
//   node run.mjs --debug         # debug build with faster compilation
//   node run.mjs -- <server args>  # arguments after -- are passed to the server
//
// Requires Node.js 20+ and Rust 1.91+.

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const webDir = join(root, "web");
const serverDir = join(root, "server");

const rawArgs = process.argv.slice(2);
const separator = rawArgs.indexOf("--");
const ownArgs = separator === -1 ? rawArgs : rawArgs.slice(0, separator);
const serverArgs = separator === -1 ? [] : rawArgs.slice(separator + 1);

let debug = false;
for (const arg of ownArgs) {
  if (arg === "--debug") {
    debug = true;
  } else if (arg === "--help" || arg === "-h") {
    console.log(`Usage:
  node run.mjs                 # release build
  node run.mjs --debug         # debug build
  node run.mjs -- <server args>  # forward arguments to the server`);
    process.exit(0);
  } else {
    console.error(`error: unknown option: ${arg}`);
    process.exit(2);
  }
}

if (isWsl()) {
  console.warn(
    "warning: WSL detected; this starts the Linux backend, which cannot use Windows agent CLIs. " +
      "Run with Windows Node.js to start the Windows backend.",
  );
}

function isWsl() {
  if (process.platform !== "linux") {
    return false;
  }
  if (process.env.WSL_DISTRO_NAME) {
    return true;
  }
  try {
    return /microsoft/i.test(readFileSync("/proc/version", "utf8"));
  } catch {
    return false;
  }
}

function run(command, args, { cwd, shell = false } = {}) {
  const result = spawnSync(command, args, {
    cwd,
    shell,
    stdio: "inherit",
  });
  if (result.error) {
    console.error(`error: failed to run ${command}: ${result.error.message}`);
    process.exit(1);
  }
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const npmShell = process.platform === "win32";

run(npm, ["install", "--no-audit", "--no-fund"], { cwd: webDir, shell: npmShell });
run(npm, ["run", "build"], { cwd: webDir, shell: npmShell });

const cargoArgs = ["run"];
if (!debug) {
  cargoArgs.push("--release");
}
if (serverArgs.length > 0) {
  cargoArgs.push("--", ...serverArgs);
}
run("cargo", cargoArgs, { cwd: serverDir });
