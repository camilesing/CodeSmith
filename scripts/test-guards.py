#!/usr/bin/env python3
"""Self-tests for the guard scripts (the doctrine: guards get tests too).

Runs each verify-* guard against throwaway fixture trees and pins the
accept/reject paths: clean trees pass, injected drift fails naming the
offender. The cargo-metadata guards (check-framework-deps.py,
capability-graph.py) are not covered here — they need a fixture
workspace; they stay manually injection-verified (see the TODO-1 and
TODO-4 records in the dev plan).

Run: python3 scripts/test-guards.py
"""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent


def run_guard(script: str, root: Path, *extra: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPTS / script), "--root", str(root), *extra],
        capture_output=True,
        text=True,
    )


def write(root: Path, relpath: str, text: str = "") -> None:
    path = root / relpath
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def write_allowlisted(root: Path) -> None:
    # The guard errors on allowlist entries with no matching doc, so every
    # fixture tree must carry the full allowlist (see verify-docs-pairs.py).
    for name in ("CAPABILITY_GRAPH.md", "CLI.md", "DESIGN_INTERNALS.md", "HARNESS.md"):
        write(root, f"docs/{name}")


class DocsPairsGuard(unittest.TestCase):
    def test_paired_tree_passes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_allowlisted(root)
            write(root, "docs/GUIDE.md")
            write(root, "docs/GUIDE_cn.md")
            result = run_guard("verify-docs-pairs.py", root)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("1 pair", result.stdout)

    def test_unpaired_doc_fails_and_names_it(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_allowlisted(root)
            write(root, "docs/GUIDE.md")
            write(root, "docs/GUIDE_cn.md")
            write(root, "docs/NEW.md")
            result = run_guard("verify-docs-pairs.py", root)
            self.assertEqual(result.returncode, 1)
            self.assertIn("NEW.md: no _cn pair", result.stdout)

    def test_allowlisted_doc_passes_without_pair(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_allowlisted(root)
            result = run_guard("verify-docs-pairs.py", root)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("4 allowlisted", result.stdout)

    def test_stale_allowlist_entry_fails_and_names_it(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name in ("CAPABILITY_GRAPH.md", "DESIGN_INTERNALS.md", "HARNESS.md"):
                write(root, f"docs/{name}")
            write(root, "docs/GUIDE.md")
            write(root, "docs/GUIDE_cn.md")
            result = run_guard("verify-docs-pairs.py", root)
            self.assertEqual(result.returncode, 1)
            self.assertIn("CLI.md: allowlisted but no such doc", result.stdout)

    def test_allowlisted_doc_gaining_pair_fails_and_names_it(self) -> None:
        # The docstring promises "landing a _cn pair removes an entry" —
        # the guard must say so instead of passing with dead config.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_allowlisted(root)
            write(root, "docs/GUIDE.md")
            write(root, "docs/GUIDE_cn.md")
            write(root, "docs/CLI_cn.md")
            result = run_guard("verify-docs-pairs.py", root)
            self.assertEqual(result.returncode, 1)
            self.assertIn(
                "CLI.md: allowlisted but its _cn pair now exists", result.stdout
            )

    def test_orphan_cn_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_allowlisted(root)
            write(root, "docs/GUIDE.md")
            write(root, "docs/GUIDE_cn.md")
            write(root, "docs/ORPHAN_cn.md")
            result = run_guard("verify-docs-pairs.py", root)
            self.assertEqual(result.returncode, 1)
            self.assertIn("ORPHAN_cn.md: _cn file without its base ORPHAN.md", result.stdout)

    def test_subdirectories_out_of_scope(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_allowlisted(root)
            write(root, "docs/legacy/README-upstream.md")
            result = run_guard("verify-docs-pairs.py", root)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class DeadcodeBudgetGuard(unittest.TestCase):
    SOURCE = """\
#![allow(dead_code)]
#[allow(dead_code)]
struct A;
#[cfg_attr(not(feature = "web"), allow(dead_code))]
fn b() {}
// prose mention of `#[allow(dead_code)]` counts too
"""

    def make_tree(self, tmp: str) -> Path:
        root = Path(tmp)
        write(root, "crates/fixture/src/lib.rs", self.SOURCE)
        return root

    def test_counting_rule_covers_all_spellings(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self.make_tree(tmp)
            result = run_guard("verify-deadcode-budget.py", root, "--budget", "4")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("count=4", result.stdout)

    def test_over_budget_fails_with_delta(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self.make_tree(tmp)
            result = run_guard("verify-deadcode-budget.py", root, "--budget", "3")
            self.assertEqual(result.returncode, 1)
            self.assertIn("count=4 budget=3", result.stdout)
            self.assertIn("crates/fixture/src/lib.rs", result.stdout)

    def test_under_budget_suggests_shrinking(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self.make_tree(tmp)
            result = run_guard("verify-deadcode-budget.py", root, "--budget", "9")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("shrink BUDGET to 4", result.stdout)


if __name__ == "__main__":
    unittest.main()
