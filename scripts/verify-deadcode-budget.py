#!/usr/bin/env python3
"""Dead-code allowance ratchet.

AGENTS.md: "The standing `#[allow(dead_code)]` count is the running
receipt." This guard turns the receipt into a ratchet: the count of lines
under crates/ mentioning `allow(dead_code` may not exceed BUDGET. Deleting
dead code lowers the count — shrink BUDGET in the same change so the
receipt keeps ratcheting down.

Counted: lines in crates/**/*.rs containing the text `allow(dead_code`,
at most one per line. Every spelling counts — plain attributes, inner
`#![allow]`, `cfg_attr` conditionals, and prose mentions in comments: the
number is a debt receipt, not a lint census.
Exempt: nothing under crates/. Vendored and generated code elsewhere is
outside the scan root by construction.

Run from anywhere: `python3 scripts/verify-deadcode-budget.py`
(`--budget N` previews a different ceiling, e.g. before shrinking).
"""

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

BUDGET = 417
PATTERN = re.compile(r"allow\(dead_code")


def main() -> int:
    parser = argparse.ArgumentParser(description="Dead-code allowance ratchet.")
    parser.add_argument(
        "--root",
        type=Path,
        default=REPO_ROOT,
        help="repository root to scan (default: this script's repo)",
    )
    parser.add_argument(
        "--budget",
        type=int,
        default=BUDGET,
        help="ceiling to enforce (default: the committed BUDGET)",
    )
    args = parser.parse_args()

    per_file: dict[str, int] = {}
    for path in sorted((args.root / "crates").rglob("*.rs")):
        hits = sum(1 for line in path.read_text().splitlines() if PATTERN.search(line))
        if hits:
            per_file[str(path.relative_to(args.root))] = hits
    count = sum(per_file.values())

    if count > args.budget:
        print(f"dead_code: count={count} budget={args.budget} — over by {count - args.budget}")
        for name, hits in sorted(per_file.items(), key=lambda kv: -kv[1]):
            print(f"  {hits:4d}  {name}")
        print(
            "delete the dead code, or raise BUDGET in "
            "scripts/verify-deadcode-budget.py with a justification"
        )
        return 1
    note = ""
    if count < args.budget:
        note = f" — shrink BUDGET to {count}"
    print(f"dead_code: count={count} budget={args.budget}{note}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
