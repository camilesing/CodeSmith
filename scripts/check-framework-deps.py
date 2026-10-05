#!/usr/bin/env python3
"""Framework-layer dependency-direction guard.

The workspace is split into a framework group and an implementation group.
Framework crates must not depend — in the build graph — on any workspace
crate outside the group: contracts and the engine live in the framework;
product surfaces and providers hang off it, never the other way around.
AGENTS.md states the rule; this script makes it physical (the plan doc
calls this "writing the rule into the cargo dependency graph").

Checked: normal and build-script path dependencies of framework crates —
what compiles into framework artifacts.
Exempt: dev-dependencies. They never enter a framework artifact consumed
by downstream crates; the standing example is `codesmith-extensions`' test
fixture (`extensions-fixture-dylib`).

Run from anywhere: `python3 scripts/check-framework-deps.py`.
Prints one line per violation and exits non-zero; exits 1 with a staleness
error if FRAMEWORK names crates that no longer exist.
"""

import json
import subprocess
import sys
from pathlib import Path

# The framework group. Keep in sync with the boundary note in
# docs/ARCHITECTURE.md (and its _cn pair); per-crate assignment reasons
# live there.
FRAMEWORK = {
    "codesmith-protocol",  # wire-frame vocabulary; leaf, no workspace deps
    "codesmith-tools",  # tool lifecycle + framework-side seam definitions
    "codesmith-agent",  # Extension/LlmClient/Tool contracts + registries
    "codesmith-config",  # config schema + precedence; framework needs it
    "codesmith-secrets",  # secret-store facade; config and engine need it
    "codesmith-extensions",  # extension runtime: loading, dispatch, reload
    "codesmith-agent-runtime",  # the kernel: one turn loop, engine, compaction
}

REPO_ROOT = Path(__file__).resolve().parent.parent


def main() -> int:
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            cwd=REPO_ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    packages = {p["name"]: p for p in meta["packages"]}
    unknown = FRAMEWORK - packages.keys()
    if unknown:
        print(
            "error: FRAMEWORK lists crates not in the workspace:",
            ", ".join(sorted(unknown)),
        )
        print("update scripts/check-framework-deps.py and docs/ARCHITECTURE.md")
        return 1

    violations = []
    for name in sorted(FRAMEWORK):
        for dep in packages[name]["dependencies"]:
            if dep.get("path") is None:
                continue
            kind = dep.get("kind") or "normal"
            if kind == "dev":
                continue  # exempt: never enters framework artifacts
            if dep["name"] in FRAMEWORK:
                continue
            violations.append(f"{name} -> {dep['name']} [{kind}]")

    if violations:
        print("error: framework crate depends on a non-framework workspace crate:")
        for v in violations:
            print(f"  {v}")
        print(
            "fix by dependency inversion: define the trait in a framework crate,\n"
            "have the implementation crate provide it and re-export. See the\n"
            "boundary note at the top of docs/ARCHITECTURE.md."
        )
        return 1

    print(f"framework dependency direction OK ({len(FRAMEWORK)} framework crates)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
