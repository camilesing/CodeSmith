#!/usr/bin/env python3
"""Workspace line-coverage measurement without external cargo subcommands.

Builds every workspace test target with `-C instrument-coverage`, runs the
suite, merges the .profraw files with the toolchain's own llvm-profdata/
llvm-cov (the `llvm-tools` component), and reports line coverage over
`crates/*/src` — the number a cargo-llvm-cov/codecov-style gate would see.

Usage:
    python3 scripts/coverage-report.py            # build + test + report
    python3 scripts/coverage-report.py --report-only   # reuse profraws

Requires the `llvm-tools` component on the active toolchain
(`rustup component add llvm-tools`). The toolchain is whatever `rustc` on
PATH resolves to in the repo (rustup honours rust-toolchain.toml), so run
this from the workspace root.

Known limitations (deliberate, not gaps):
- Doctests are not instrumented (`--lib --bins --tests` only); their lines
  do not appear in either side of the ratio.
- `#[cfg(test)]` code inside `crates/*/src` counts as coverable lines, the
  same denominator cargo-llvm-cov reports.
- Dependencies, build scripts, and OUT_DIR sources are excluded — the
  number is workspace product code only.
- Output is the per-crate table on stdout; no HTML/lcov artifact is
  produced (add `llvm-cov export --format=lcov` here when a gate needs one).
"""

import argparse
import glob
import json
import os
import re

import subprocess
import sys
import tempfile
from collections import defaultdict

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGET_DIR = os.path.join(REPO, "target-cov")

# Coverage instrumentation needs no debug info (line data lives in the
# coverage mapping, not DWARF) and no incremental units; both keep the
# separate instrumented target dir roughly a third of a full debug build.
MEASURE_ENV = {
    "RUSTFLAGS": "-C instrument-coverage",
    "CARGO_PROFILE_DEV_DEBUG": "0",
    "CARGO_PROFILE_TEST_DEBUG": "0",
    "CARGO_PROFILE_DEV_INCREMENTAL": "false",
}


def llvm_tools_bin():
    sysroot = subprocess.run(
        ["rustc", "--print", "sysroot"], capture_output=True, text=True, check=True
    ).stdout.strip()
    for candidate in glob.glob(os.path.join(sysroot, "lib", "rustlib", "*", "bin")):
        if os.path.exists(os.path.join(candidate, "llvm-profdata")):
            return candidate
    sys.exit(
        "llvm-profdata not found under the active toolchain — install it with "
        "`rustup component add llvm-tools` (or use a rustup-managed toolchain)."
    )


def measure():
    env = dict(os.environ, **MEASURE_ENV)
    profraw_dir = tempfile.mkdtemp(prefix="codesmith-cov-")
    env["LLVM_PROFILE_FILE"] = os.path.join(profraw_dir, "run-%p-%m.profraw")
    # --target-dir as a FLAG, not CARGO_TARGET_DIR: the env var would leak
    # into spawned fixture `cargo` runs inside tests (tool-impls' test_runner
    # fixtures), whose shared target dir then races sibling fixtures.
    # --no-fail-fast keeps later crates' coverage collectable when one fails.
    cmd = [
        "cargo", "test", "--workspace", "--all-features", "--locked",
        "--lib", "--bins", "--tests", "--no-fail-fast",
        f"--target-dir={TARGET_DIR}",
    ]
    print(f"==> {' '.join(cmd)}", file=sys.stderr)
    log_path = os.path.join(profraw_dir, "test-output.log")
    with open(log_path, "w") as log:
        completed = subprocess.run(cmd, cwd=REPO, env=env, stdout=log, stderr=subprocess.STDOUT)
    return profraw_dir, completed.returncode


def coverage_objects(profraw_dir):
    """Exactly the test binaries cargo ran, parsed from the test output.

    Enumerating the target dir instead picks up superseded binaries from
    earlier builds: their coverage mappings carry OLD line numbers for the
    same sources and llvm-cov unions the regions, inflating the denominator
    and marking shifted lines uncovered. Only objects that actually executed
    produced profile data, so the run log is the source of truth.
    """
    log_path = os.path.join(profraw_dir, "test-output.log")
    if os.path.exists(log_path):
        text = open(log_path, errors="replace").read()
        names = set(re.findall(r"target-cov/debug/[^\s)\]]+", text))
        objs = {os.path.join(REPO, name) for name in names}
        objs = {p for p in objs if os.path.isfile(p)}
        if objs:
            return sorted(objs)
    # Fallback (log-less --report-only): newest executable per crate stem.
    by_stem = {}
    for path in glob.glob(os.path.join(TARGET_DIR, "debug", "deps", "*")):
        if not os.path.isfile(path) or not os.access(path, os.X_OK):
            continue
        stem = re.sub(r"-[0-9a-f]{16}$", "", os.path.basename(path))
        mtime = os.path.getmtime(path)
        if stem not in by_stem or mtime > by_stem[stem][0]:
            by_stem[stem] = (mtime, path)
    return sorted(p for _, p in by_stem.values())


def report(profraw_dir):
    tools = llvm_tools_bin()
    profraws = glob.glob(os.path.join(profraw_dir, "*.profraw"))
    if not profraws:
        sys.exit(f"no .profraw files in {profraw_dir} — run without --report-only first")
    profdata = os.path.join(profraw_dir, "merged.profdata")
    subprocess.run(
        [os.path.join(tools, "llvm-profdata"), "merge", "-sparse", *profraws, "-o", profdata],
        check=True,
    )
    cmd = [
        os.path.join(tools, "llvm-cov"), "export",
        f"--instr-profile={profdata}", "--summary-only", "--format=text",
    ]
    for obj in coverage_objects(profraw_dir):
        cmd += ["--object", obj]
    data = json.loads(subprocess.run(cmd, capture_output=True, text=True, check=True).stdout)

    crate_re = re.compile(re.escape(REPO) + r"/crates/([^/]+)/src/.*\.rs$")
    per_crate = defaultdict(lambda: {"lc": 0, "lf": 0, "fc": 0, "ff": 0, "rc": 0, "rf": 0})
    for entry in data["data"][0]["files"]:
        match = crate_re.match(entry["filename"])
        if not match:
            continue
        bucket = per_crate[match.group(1)]
        for key, summary_key in (
            ("lc", "lines"), ("fc", "functions"), ("rc", "regions")
        ):
            bucket[key] += entry["summary"][summary_key]["covered"]
            bucket[key.replace("c", "f")] += entry["summary"][summary_key]["count"]

    print(f"{'crate':<24} {'line':>7} {'func':>7} {'region':>7}")
    totals = {k: sum(v[k] for v in per_crate.values()) for k in per_crate[next(iter(per_crate))]}
    print(
        f"{'WORKSPACE':<24} "
        f"{100*totals['lc']/totals['lf']:6.1f}% "
        f"{100*totals['fc']/totals['ff']:6.1f}% "
        f"{100*totals['rc']/totals['rf']:6.1f}%"
    )
    print("(metrics over crates/*/src; region is the Rust-ecosystem stand-in for branch coverage)")
    print("-" * 56)
    def ratio(v, c, t):
        return 100 * v[c] / v[t] if v[t] else 0.0
    for name, v in sorted(per_crate.items(), key=lambda kv: ratio(kv[1], "fc", "ff")):
        print(
            f"{name:<24} {ratio(v,'lc','lf'):6.1f}% {ratio(v,'fc','ff'):6.1f}% {ratio(v,'rc','rf'):6.1f}%"
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--report-only", action="store_true", help="skip the test run; reuse existing profraws"
    )
    args = parser.parse_args()
    if args.report_only:
        # Reuse the most recent measurement directory (mkdtemp suffixes are
        # random, so lexicographic order is not recency).
        dirs = glob.glob(os.path.join(tempfile.gettempdir(), "codesmith-cov-*"))
        if not dirs:
            sys.exit("no previous measurement found — run without --report-only")
        report(max(dirs, key=os.path.getmtime))
        return
    profraw_dir, rc = measure()
    report(profraw_dir)
    if rc != 0:
        print(f"note: cargo test exited {rc}; coverage above is partial", file=sys.stderr)
        sys.exit(rc)


if __name__ == "__main__":
    main()
