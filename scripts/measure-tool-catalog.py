#!/usr/bin/env python3
"""Measure serialized tool catalog size before and after default deferral.

This delegates catalog construction to an ignored Rust test so the measurement
uses the same tool definitions, JSON serialization, and deferral policy as the
runtime. Token counts are deterministic estimates using ceil(serialized_bytes/4).
"""

from __future__ import annotations

import json
import subprocess
import sys


MARKER = "TOOL_CATALOG_METRICS "


def main() -> int:
    cmd = [
        "cargo",
        "test",
        "-p",
        "codesmith-tui",
        "print_agent_tool_catalog_metrics",
        "--",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]
    proc = subprocess.run(cmd, text=True, capture_output=True, check=False)
    sys.stdout.write(proc.stdout)

    # The Rust test prints the marker with `eprintln!` (stderr), so scan
    # stderr for it; scanning stdout could never find the marker.
    for line in proc.stderr.splitlines():
        if MARKER in line:
            metrics = json.loads(line.split(MARKER, 1)[1])
            print(json.dumps(metrics, indent=2, sort_keys=True))
            sys.stderr.write(proc.stderr)
            return proc.returncode

    sys.stderr.write(proc.stderr)
    sys.stderr.write("missing TOOL_CATALOG_METRICS marker\n")
    return proc.returncode or 1


if __name__ == "__main__":
    raise SystemExit(main())
