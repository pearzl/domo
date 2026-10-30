#!/usr/bin/env python3
"""Run every DOMO Python test in a separate process.

Usage:
    python tests/run.py            # all tests
    python tests/run.py stall      # tests whose filename contains "stall"
"""

import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent


def main():
    pattern = sys.argv[1] if len(sys.argv) > 1 else ""
    tests = [path for path in sorted(HERE.glob("test_*.py")) if pattern in path.name]
    if not tests:
        print(f"no tests matched {pattern!r}", file=sys.stderr)
        return 1

    env = os.environ.copy()
    env["PYTHONUTF8"] = "1"
    env["PYTHONIOENCODING"] = "utf-8"

    failed = []
    for test in tests:
        print(f"==> {test.name}", flush=True)
        result = subprocess.run(
            [sys.executable, str(test)],
            cwd=str(ROOT),
            env=env,
        )
        if result.returncode != 0:
            failed.append(test.name)
            print(f"FAILED: {test.name}", flush=True)

    print()
    if failed:
        print(f"{len(failed)} test(s) failed:", file=sys.stderr)
        for name in failed:
            print(f"  - {name}", file=sys.stderr)
        return 1
    print(f"all {len(tests)} test(s) passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
