#!/usr/bin/env python3
"""Self-tests for the session readers of scripts/journal_metrics.py.

The script mirrors `FileSessionStore::load_session` / `read_journal` when it
folds a snapshot + journal pair; these pin what a crash between a journal
append and the snapshot write leaves behind, against throwaway fixture files
(nothing under the real CODESMITH_HOME is read).

Run: python3 scripts/test-journal-metrics.py
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent


def load_script():
    # A hyphenated file name is not importable by name.
    spec = importlib.util.spec_from_file_location(
        "journal_metrics", SCRIPTS / "journal_metrics.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


jm = load_script()


def message(role: str, text: str) -> dict:
    return {"role": role, "content": [{"type": "text", "text": text}]}


def journal(*records: dict) -> bytes:
    # serde_json writes non-ASCII raw; so does the fixture.
    return b"".join(
        json.dumps(record, ensure_ascii=False).encode("utf-8") + b"\n" for record in records
    )


def texts(session: dict) -> list[str]:
    return [block["text"] for m in session["messages"] for block in m["content"]]


class TornTail(unittest.TestCase):
    """A journal whose last line a crash cut mid-write."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.path = Path(tmp.name) / "s1.journal.jsonl"

    def fold(self, blob: bytes) -> dict:
        self.path.write_bytes(blob)
        # The torn-line notice goes to stderr on purpose; keep the test log clean.
        with contextlib.redirect_stderr(io.StringIO()):
            return jm.fold_journal(self.path)

    def test_a_torn_final_line_is_dropped_like_the_product_drops_it(self) -> None:
        whole = journal(
            {"kind": "header", "session_id": "s1"},
            {"kind": "append", "messages": [message("user", "one")]},
            {"kind": "append", "messages": [message("assistant", "two")]},
        )
        # The third append was cut mid-write — inside a multi-byte character.
        third = journal({"kind": "append", "messages": [message("user", "三")]})
        cut = third.index("三".encode("utf-8")) + 1
        folded = self.fold(whole + third[:cut])
        self.assertEqual(texts(folded), ["one", "two"])
        self.assertEqual(folded["id"], "s1")

    def test_a_torn_ascii_tail_is_dropped_too(self) -> None:
        whole = journal({"kind": "header", "session_id": "s1"})
        folded = self.fold(whole + b'{"kind": "append", "messages": [{"role"')
        self.assertEqual(folded["messages"], [])

    def test_a_newline_terminated_bad_last_line_still_fails_loud(self) -> None:
        # Ended cleanly but unreadable: that is corruption, not a torn write.
        whole = journal({"kind": "header", "session_id": "s1"})
        with self.assertRaises(ValueError):
            self.fold(whole + b'{"kind": "append", "messages": [}\n')

    def test_a_malformed_middle_line_fails_loud(self) -> None:
        whole = journal({"kind": "header", "session_id": "s1"})
        with self.assertRaises(ValueError):
            self.fold(whole + b'{"kind": "append"\n' + journal({"kind": "request"}))


if __name__ == "__main__":
    unittest.main()
