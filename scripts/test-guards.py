#!/usr/bin/env python3
"""Self-tests for the guard scripts (the doctrine: guards get tests too).

Runs each verify-* guard against throwaway fixture trees and pins the
accept/reject paths: clean trees pass, injected drift fails naming the
offender. The workspace-scanning guards (check-framework-deps.py,
capability-graph.py, event-matrix.py) are not covered here — they need a
fixture workspace; they stay manually injection-verified (see the TODO-1,
TODO-4 and round-9 TODO-61 records in the dev plan).

Run: python3 scripts/test-guards.py
"""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent


def run_guard(script: str, root: Path, *extra: str) -> subprocess.CompletedProcess:
    # A guard regressing into an infinite loop must fail this one test, not
    # hang the whole suite: TimeoutExpired surfaces as an error naming it.
    return subprocess.run(
        [sys.executable, str(SCRIPTS / script), "--root", str(root), *extra],
        capture_output=True,
        text=True,
        timeout=30,
    )


def write(root: Path, relpath: str, text: str = "") -> None:
    path = root / relpath
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def docs_pairs_allowlist() -> set[str]:
    # Derive the fixture from the guard's real ALLOWLIST so the two cannot
    # drift (a hand-copied set here would break every DocsPairsGuard test
    # the day the guard's allowlist grows).
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "verify_docs_pairs", SCRIPTS / "verify-docs-pairs.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return set(module.ALLOWLIST)


def write_allowlisted(root: Path) -> None:
    # The guard errors on allowlist entries with no matching doc, so every
    # fixture tree must carry the full allowlist (see verify-docs-pairs.py).
    for name in docs_pairs_allowlist():
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
            self.assertIn(f"{len(docs_pairs_allowlist())} allowlisted", result.stdout)

    def test_stale_allowlist_entry_fails_and_names_it(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            # Allowlist has CAPABILITY_GRAPH.md; the tree does not.
            write(root, "docs/GUIDE.md")
            write(root, "docs/GUIDE_cn.md")
            result = run_guard("verify-docs-pairs.py", root)
            self.assertEqual(result.returncode, 1)
            self.assertIn(
                "CAPABILITY_GRAPH.md: allowlisted but no such doc", result.stdout
            )

    def test_allowlisted_doc_gaining_pair_fails_and_names_it(self) -> None:
        # The docstring promises "landing a _cn pair removes an entry" —
        # the guard must say so instead of passing with dead config.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write(root, "docs/CAPABILITY_GRAPH.md")
            write(root, "docs/CAPABILITY_GRAPH_cn.md")
            write(root, "docs/GUIDE.md")
            write(root, "docs/GUIDE_cn.md")
            result = run_guard("verify-docs-pairs.py", root)
            self.assertEqual(result.returncode, 1)
            self.assertIn(
                "CAPABILITY_GRAPH.md: allowlisted but its _cn pair now exists",
                result.stdout,
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


class MdLinksGuard(unittest.TestCase):
    def test_paired_tree_passes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write(root, "docs/GUIDE.md", "# Guide\n\nSee [the appendix](APPENDIX.md#appendix).\n")
            write(root, "docs/APPENDIX.md", "# Appendix\n")
            result = run_guard("verify-md-links.py", root)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("2 file(s)", result.stdout)

    def test_missing_target_fails_and_names_it(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write(root, "README.md", "[missing](docs/GONE.md)\n")
            result = run_guard("verify-md-links.py", root)
            self.assertEqual(result.returncode, 1)
            self.assertIn("missing target docs/GONE.md", result.stderr)

    def test_unknown_fragment_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write(root, "docs/GUIDE.md", "[x](OTHER.md#nope)\n")
            write(root, "docs/OTHER.md", "# Real Heading\n")
            result = run_guard("verify-md-links.py", root)
            self.assertEqual(result.returncode, 1)
            self.assertIn("no heading for #nope", result.stderr)

    def test_code_blocks_and_external_links_ignored(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write(
                root,
                "docs/GUIDE.md",
                "```markdown\n[broke](NOPE.md)\n```\n"
                "`[inline](ALSO-NOPE.md)` and [site](https://example.com/x) stay.\n",
            )
            result = run_guard("verify-md-links.py", root)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
