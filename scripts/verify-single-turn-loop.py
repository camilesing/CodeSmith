#!/usr/bin/env python3
"""Single turn-loop gate (general-agent alignment, iteration B — "one loop drives every agent").

Every agent turn — the main turn and every in-process sub-agent turn — runs
on the turn executor resolved from the `TurnExecutorRegistry`
(`crates/agent-runtime/src/engine/`). A second agent loop shows up in the
source as a model call (`.create_message(` / `.create_message_stream(`)
issued from inside a `loop` / `while` / `for` body; this guard lists every
such call site outside the allowlist below. One-shot calls (routers,
summarizers, probes) are not inside a loop and are never reported.

Heuristic (deliberately dumb, like the other guards): comments and string
literals are stripped, `#[cfg(test)]` modules and test files are skipped,
and a call is "in a loop" when an enclosing brace block of the same
function opens with a `loop` / `while` / `for` header. A closure or async
block spawned inside a loop counts — a fan-out that issues calls per
iteration still has to be explained here.

Default is dry-run: findings are printed, the exit code is 0. `--enforce`
exits 1 on any finding (and on an allowlist entry that no longer matches a
file, so the list cannot rot). `--root` points the scan at another tree
(the guard self-tests use fixture trees).
"""

import argparse
import re
import sys
from pathlib import Path

DEFAULT_ROOT = Path(__file__).resolve().parent.parent

# path (relative to the root) → why a model call inside a loop is legitimate
# there. Keep each reason one sentence; a new entry is a design decision.
ALLOWLIST = {
    "crates/agent-runtime/src/engine/turn/stream.rs": (
        "the shared turn loop's stream round (transparent retry) — the loop "
        "every agent runs on"
    ),
    "crates/agent/src/executor/mod.rs": (
        "the framework-core reference loop (`DefaultAgentExecutor`), consumed "
        "by embeds and the bridge tests, never by a host turn"
    ),
    "crates/agent/src/llm_client/retrying.rs": (
        "the transport retry wrapper: re-issues one request, not an agent loop"
    ),
    "crates/agent-runtime/src/compaction/compact.rs": (
        "the compaction summarizer's peel-and-retry: shrinks one summary "
        "request until it fits, never dispatches a tool"
    ),
    "crates/agent-runtime/src/rlm/turn.rs": (
        "the RLM algorithm (code → REPL → final answer over a Python kernel), "
        "a tool's protocol rather than a tool-dispatching agent loop"
    ),
}

CALL_RE = re.compile(r"\.create_message(?:_stream)?\s*\(")
LOOP_HEADER_RE = re.compile(r"(?:^|[^\w])(loop|while|for)\b[^;{}]*$")
FN_HEADER_RE = re.compile(r"(?:^|[^\w])fn\s+\w+[^;{}]*$")
TEST_MOD_RE = re.compile(r"#\[cfg\(test\)\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")


def strip_comments_and_strings(text: str) -> str:
    """Blank out comments and string literals, keeping offsets stable."""
    out = list(text)
    i, n = 0, len(text)

    def blank(start: int, end: int) -> None:
        for k in range(start, min(end, n)):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
        elif text.startswith("/*", i):
            j = text.find("*/", i + 2)
            j = n if j < 0 else j + 2
            blank(i, j)
            i = j
        elif (m := re.match(r'r(#*)"', text[i:])) and (i == 0 or not text[i - 1].isalnum()):
            closer = '"' + m.group(1)
            j = text.find(closer, i + m.end())
            j = n if j < 0 else j + len(closer)
            blank(i, j)
            i = j
        elif text[i] == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            blank(i, j + 1)
            i = j + 1
        elif text[i] == "'" and (m := re.match(r"'(\\.|[^\\'])'", text[i:])):
            blank(i, i + m.end())
            i += m.end()
        else:
            i += 1
    return "".join(out)


def blank_test_modules(text: str) -> str:
    """Blank out `#[cfg(test)] mod name { … }` bodies (brace-matched)."""
    out = list(text)
    for m in TEST_MOD_RE.finditer(text):
        depth, k = 0, m.end() - 1
        while k < len(text):
            if text[k] == "{":
                depth += 1
            elif text[k] == "}":
                depth -= 1
                if depth == 0:
                    break
            k += 1
        for j in range(m.start(), min(k + 1, len(text))):
            if out[j] != "\n":
                out[j] = " "
    return "".join(out)


def enclosing_loop(code: str, pos: int) -> bool:
    """Whether the call at `pos` sits inside a loop body of its function."""
    depth = 0
    k = pos
    while k > 0:
        k -= 1
        ch = code[k]
        if ch == "}":
            depth += 1
        elif ch == "{":
            if depth > 0:
                depth -= 1
                continue
            # An opener that encloses the call: classify its header.
            start = max(code.rfind(";", 0, k), code.rfind("{", 0, k), code.rfind("}", 0, k))
            header = code[start + 1 : k]
            if FN_HEADER_RE.search(header):
                return False
            if LOOP_HEADER_RE.search(header):
                return True
    return False


def scan(root: Path) -> list[tuple[str, int]]:
    findings = []
    for path in sorted((root / "crates").glob("*/src/**/*.rs")):
        rel = path.relative_to(root).as_posix()
        if "target" in path.parts or path.name == "tests.rs" or "/tests/" in rel:
            continue
        if rel in ALLOWLIST:
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        code = blank_test_modules(strip_comments_and_strings(text))
        for m in CALL_RE.finditer(code):
            if enclosing_loop(code, m.start()):
                findings.append((rel, code.count("\n", 0, m.start()) + 1))
    return findings


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", type=Path, default=DEFAULT_ROOT)
    ap.add_argument("--enforce", action="store_true", help="exit 1 on findings")
    args = ap.parse_args()
    root = args.root.resolve()

    stale = [rel for rel in ALLOWLIST if not (root / rel).is_file()]
    findings = scan(root)
    for rel, line in findings:
        print(f"{rel}:{line}: model call inside a loop outside the turn executor")
    for rel in stale:
        print(f"allowlist entry matches no file: {rel}")
    if findings or stale:
        mode = "error" if args.enforce else "dry-run"
        print(
            f"{mode}: {len(findings)} loop call site(s), {len(stale)} stale allowlist "
            "entr(y/ies) — one loop drives every agent (scripts/verify-single-turn-loop.py)"
        )
        return 1 if args.enforce else 0
    print(f"single turn loop OK ({len(ALLOWLIST)} allowlisted loop sites)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
