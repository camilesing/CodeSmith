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
        "codesmith-cli",
        "print_agent_tool_catalog_metrics",
        "--",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]
    try:
        proc = subprocess.run(
            cmd, text=True, capture_output=True, check=False, timeout=600
        )
    except FileNotFoundError:
        sys.stderr.write("cargo not found on PATH\n")
        return 1
    except subprocess.TimeoutExpired:
        sys.stderr.write("cargo test timed out after 600 s\n")
        return 1
    sys.stdout.write(proc.stdout)

    # The Rust test prints the marker with `eprintln!` (stderr), so scan
    # stderr for it; scanning stdout could never find the marker. Anchor the
    # match at line start and require exactly one hit: an unanchored
    # substring could grab an unrelated diagnostic mentioning the marker.
    matches = [
        line[len(MARKER):]
        for line in proc.stderr.splitlines()
        if line.startswith(MARKER)
    ]
    if len(matches) != 1:
        sys.stderr.write(proc.stderr)
        sys.stderr.write("expected exactly one TOOL_CATALOG_METRICS marker\n")
        return proc.returncode or 1
    try:
        metrics = json.loads(matches[0])
    except json.JSONDecodeError:
        sys.stderr.write(proc.stderr)
        sys.stderr.write(f"malformed TOOL_CATALOG_METRICS payload: {matches[0]!r}\n")
        return proc.returncode or 1
    print(json.dumps(metrics, indent=2, sort_keys=True))
    sys.stderr.write(proc.stderr)
    return proc.returncode


if __name__ == "__main__":
    raise SystemExit(main())
