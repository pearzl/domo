"""Shared helpers for DOMO cross-platform tests.

Only the Python standard library is used. Tests run the real server binary and
Git, so the same test files work on Windows, Linux, and macOS.
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from contextlib import contextmanager
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SERVER_DIR = ROOT / "server"
SERVER_BIN = SERVER_DIR / "target" / "debug" / (
    "domo-server.exe" if os.name == "nt" else "domo-server"
)
IS_WINDOWS = os.name == "nt"

_built = False


def run(cmd, cwd=None, env=None, check=True, capture=True):
    result = subprocess.run(
        [str(part) for part in cmd],
        cwd=None if cwd is None else str(cwd),
        env=env,
        check=False,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        text=True,
        encoding="utf-8" if capture else None,
        errors="replace" if capture else None,
    )
    if check and result.returncode != 0:
        detail = ""
        if capture:
            detail = f"\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        raise AssertionError(f"command failed ({result.returncode}): {' '.join(map(str, cmd))}{detail}")
    return result


def git(args, cwd, check=True):
    return run(["git", *args], cwd=cwd, check=check).stdout


def ensure_server_built():
    global _built
    if _built:
        return
    if shutil.which("cargo") is None:
        raise AssertionError("cargo not found in PATH")
    run(
        ["cargo", "build", "--manifest-path", SERVER_DIR / "Cargo.toml"],
        cwd=ROOT,
        capture=False,
    )
    if not SERVER_BIN.exists():
        raise AssertionError(f"server binary not found: {SERVER_BIN}")
    _built = True


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


@contextmanager
def temp_root(prefix):
    path = Path(tempfile.mkdtemp(prefix=prefix))
    try:
        yield path
    finally:
        shutil.rmtree(path, ignore_errors=True)


def init_repo(path):
    path.mkdir(parents=True, exist_ok=True)
    git(["init", "-q", "-b", "main"], path)
    git(
        [
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ],
        path,
    )


def call(base, method, path, data=None):
    body = json.dumps(data).encode() if data is not None else None
    request = urllib.request.Request(
        base + path,
        data=body,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")
        raise AssertionError(f"{method} {path} -> {error.code}: {detail}") from error


def wait_for(check, label, timeout=120):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = check()
        if last:
            return last
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for {label}; last={last!r}")


def process_alive(pid):
    if IS_WINDOWS:
        import ctypes

        synchronize = 0x00100000
        handle = ctypes.windll.kernel32.OpenProcess(synchronize, False, int(pid))
        if not handle:
            return False
        ctypes.windll.kernel32.CloseHandle(handle)
        return True
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def write_python_launcher(script):
    """Makes a Python fixture directly executable for DOMO_*_BIN."""
    text = script.read_text(encoding="utf-8")
    if not text.startswith("#!"):
        script.write_text(f"#!{sys.executable}\n{text}", encoding="utf-8")
    if IS_WINDOWS:
        launcher = script.with_suffix(".cmd")
        launcher.write_text(
            f'@echo off\r\n"{sys.executable}" "%~dp0{script.name}" %*\r\n',
            encoding="utf-8",
        )
        return launcher
    script.chmod(0o755)
    return script


class Server:
    """A DOMO server process bound to one temp root."""

    def __init__(self, root, env=None, port=None):
        self.root = Path(root)
        self.port = port or free_port()
        self.base = f"http://127.0.0.1:{self.port}"
        self.env = {key: str(value) for key, value in (env or {}).items()}
        self.process = None
        self.out_path = self.root / "server.out.log"
        self.err_path = self.root / "server.err.log"
        self._out = None
        self._err = None

    def start(self, timeout=60):
        ensure_server_built()
        env = os.environ.copy()
        env.update(
            {
                "DOMO_DATA_ROOT": str(self.root / "data"),
                "DOMO_WORKSPACE_ROOT": str(self.root / "ws"),
                "DOMO_LOG_ROOT": str(self.root / "log"),
                "DOMO_SERVER_PORT": str(self.port),
            }
        )
        env.update(self.env)
        self._out = open(self.out_path, "a", encoding="utf-8")
        self._err = open(self.err_path, "a", encoding="utf-8")
        kwargs = {}
        if IS_WINDOWS:
            kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
        else:
            kwargs["start_new_session"] = True
        self.process = subprocess.Popen(
            [str(SERVER_BIN)],
            cwd=str(ROOT),
            env=env,
            stdout=self._out,
            stderr=self._err,
            **kwargs,
        )
        self._wait_health(timeout)
        return self

    def _wait_health(self, timeout):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.process.poll() is not None:
                raise AssertionError(
                    f"server exited with {self.process.returncode}\n{self.logs()}"
                )
            try:
                with urllib.request.urlopen(self.base + "/api/health", timeout=1) as response:
                    if response.status == 200:
                        return
            except (urllib.error.URLError, TimeoutError, ConnectionError):
                pass
            time.sleep(0.1)
        raise AssertionError(f"server did not become healthy\n{self.logs()}")

    def call(self, method, path, data=None):
        return call(self.base, method, path, data)

    def stop(self, timeout=10):
        if self.process is not None and self.process.poll() is None:
            if IS_WINDOWS:
                subprocess.run(
                    ["taskkill", "/PID", str(self.process.pid), "/T", "/F"],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    check=False,
                )
            else:
                try:
                    os.killpg(os.getpgid(self.process.pid), signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
            try:
                self.process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self._close_logs()

    def interrupt(self, timeout=20):
        """Sends Ctrl+C/Ctrl+Break and returns the exit code."""
        if self.process is None or self.process.poll() is not None:
            return self.process.returncode if self.process else None
        if IS_WINDOWS:
            import ctypes

            if not ctypes.windll.kernel32.GenerateConsoleCtrlEvent(1, self.process.pid):
                raise AssertionError("failed to send Ctrl+Break to the server")
        else:
            os.kill(self.process.pid, signal.SIGINT)
        try:
            return self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raise AssertionError("server did not exit after interrupt") from error

    def kill(self):
        """Terminates the process without a graceful shutdown."""
        if self.process is not None and self.process.poll() is None:
            if IS_WINDOWS:
                subprocess.run(
                    ["taskkill", "/PID", str(self.process.pid), "/T", "/F"],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    check=False,
                )
            else:
                try:
                    os.killpg(os.getpgid(self.process.pid), signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()

    def restart(self, env=None, timeout=60):
        self.stop()
        if env:
            self.env.update({key: str(value) for key, value in env.items()})
        return self.start(timeout)

    def logs(self):
        self._flush_logs()
        output = ""
        if self.out_path.exists():
            output += self.out_path.read_text(encoding="utf-8", errors="replace")
        if self.err_path.exists():
            output += self.err_path.read_text(encoding="utf-8", errors="replace")
        return output

    def _flush_logs(self):
        for handle in (self._out, self._err):
            if handle is not None and not handle.closed:
                handle.flush()

    def _close_logs(self):
        for handle in (self._out, self._err):
            if handle is not None and not handle.closed:
                handle.close()
        self._out = None
        self._err = None

    def __enter__(self):
        return self.start()

    def __exit__(self, exc_type, exc, traceback):
        self.stop()
        return False
