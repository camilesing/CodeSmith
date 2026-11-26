#!/usr/bin/env python3
"""Generic failure-pattern metrics over CodeSmith session records.

Reads the snapshots (`<id>.json`) and append-only journals
(`<id>.journal.jsonl`) under the sessions directory, folds each into a
transcript, and computes aggregate detector rates:

  loop_rate          sessions with >=3 consecutive same-name tool calls
  error_rate         tool results flagged is_error / all tool results
  recovery_rate      errors later retried on the same tool with success
  give_up_rate       abandonment phrases in assistant text messages
  clean_session_rate sessions where no detector above fired

Use `--json` to store a run (ratchet baseline), `--compare` to print a
before/after markdown report against a stored run.

Detector concepts ported from kayba-ai/recursive-improve (Apache-2.0);
error detection uses CodeSmith's first-class `is_error` flag instead of
output-text regexes.

Known limitations: `codesmith exec` runs and runtime-API threads keep no
journal; sub-agent transcripts are replay-level records and are not read
here; compaction and utility LLM calls are absent; sessions carry no
ground-truth success flag, so `clean_session_rate` measures "no observed
failure pattern", not success. The workspace filter applies to snapshot
metadata — journal-only sessions (no snapshot) match only when
`--workspace` is omitted or the id is given explicitly via `--session`.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

GIVE_UP_RE = re.compile(
    r"I'm unable to|I cannot (complete|fulfill|process|help with)"
    r"|I'?m (sorry|afraid).{0,30}(can't|cannot|unable)"
    r"|unfortunately.{0,30}(can't|cannot|unable)"
    r"|not (able|possible) to|beyond my (ability|capabilities)",
    re.IGNORECASE,
)

LOOP_RUN_THRESHOLD = 3
SMALL_SAMPLE = 5


def fail(msg: str) -> "None":
    print(f"journal_metrics: {msg}", file=sys.stderr)
    sys.exit(2)


# ── session sources ─────────────────────────────────────────────────────────


def blocks(message: dict) -> list:
    content = message.get("content", [])
    if isinstance(content, str):
        return [{"type": "text", "text": content}] if content else []
    return content if isinstance(content, list) else []


def load_snapshot(path: Path) -> dict:
    with open(path, encoding="utf-8") as fh:
        data = json.load(fh)
    meta = data.get("metadata", {})
    return {
        "id": meta.get("id", path.stem),
        "messages": data.get("messages", []),
        "metadata": meta,
        # The journal size this snapshot covers, and the failed attempts the
        # store persists alongside it (SavedSession fields).
        "journal_bytes": data.get("journal_bytes"),
        "failed_rounds": len(data.get("failed_attempts") or []),
    }


def load_pair(snapshot: Path, journal: Path) -> dict:
    """Snapshot + journal pair, mirroring `FileSessionStore::load_session`:
    the snapshot is a cache stamped with the journal size it covers, so a
    journal that has grown past the stamp (a crash between journal append
    and snapshot write) is folded for the transcript — the snapshot then
    contributes only its metadata. A snapshot that carries no stamp was
    written by a build without journals and stays authoritative, as in
    `recover_from_journal`: a downgrade-then-upgrade never rolls a session
    back to the journal's older transcript."""
    session = load_snapshot(snapshot)
    stamped = session.pop("journal_bytes", None)
    if stamped is not None and journal.stat().st_size > stamped:
        folded = fold_journal(journal)
        folded["id"] = session["id"]
        folded["metadata"] = session["metadata"]
        return folded
    return session


def fold_journal(path: Path) -> dict:
    """Fold a journal into a transcript; unknown kinds fail loud.

    A torn tail is read the way `read_journal` (crates/session/src/journal.rs)
    reads it: the bytes decode lossily (a crash mid-write can cut the last
    line inside a multi-byte character) and a final line that does not parse
    is dropped when the file does not end in a newline. Any other unreadable
    line still fails loud."""
    transcript: list[dict] = []
    session_id, requests, failed_rounds = path.stem, 0, 0
    torn_tail_allowed = not path.read_bytes().endswith(b"\n")
    with open(path, encoding="utf-8", errors="replace") as fh:
        lines = [(lineno, line) for lineno, line in enumerate(fh, 1) if line.strip()]
    for index, (lineno, raw) in enumerate(lines):
        line = raw.strip()
        try:
            rec = json.loads(line)
        except json.JSONDecodeError as exc:
            if torn_tail_allowed and index + 1 == len(lines):
                print(
                    f"journal_metrics: {path}:{lineno}: dropping a torn final journal line ({exc})",
                    file=sys.stderr,
                )
                continue
            raise ValueError(f"{path}:{lineno}: malformed JSON ({exc})") from exc
        kind = rec.get("kind")
        if kind == "header":
            session_id = rec.get("session_id", session_id)
        elif kind == "append":
            transcript.extend(rec.get("messages", []))
        elif kind == "rebuild":
            transcript = rec.get("messages", [])
        elif kind == "resume":
            pass  # transcript already equals the journal's fold
        elif kind == "request":
            requests += 1
        elif kind == "attempt":
            failed_rounds += 1
        else:
            raise ValueError(f"{path}:{lineno}: unknown journal kind {kind!r}")
    return {
        "id": session_id,
        "messages": transcript,
        "metadata": {},
        "requests": requests,
        "failed_rounds": failed_rounds,
    }


def collect_sessions(args: argparse.Namespace) -> list[dict]:
    sessions_dir = Path(args.sessions_dir)
    if not sessions_dir.is_dir():
        fail(f"sessions directory not found: {sessions_dir}")
    workspace = None
    workspace_variants: set[str] = set()
    if args.workspace:
        workspace = args.workspace
        # Accept both spellings: the engine may store either the literal or
        # the fully resolved path (e.g. macOS /tmp vs /private/tmp).
        workspace_variants = {args.workspace, str(Path(args.workspace).resolve())}

    picked: list[dict] = []
    skipped: list[str] = []
    if args.session:
        for sid in args.session:
            journal = sessions_dir / f"{sid}.journal.jsonl"
            snapshot = sessions_dir / f"{sid}.json"
            try:
                if snapshot.exists() and journal.exists():
                    picked.append(load_pair(snapshot, journal))
                elif snapshot.exists():
                    picked.append(load_snapshot(snapshot))
                elif journal.exists():
                    picked.append(fold_journal(journal))
                else:
                    fail(f"no session record for id {sid} in {sessions_dir}")
            except (ValueError, OSError) as exc:
                fail(str(exc))  # explicitly requested: hard failure
    else:
        # One record per session id. When both exist the pair is loaded
        # through `load_pair` (journal folded when it outgrew the snapshot
        # stamp; the snapshot still carries the metadata the workspace
        # filter needs). Foreign .json files in the dir (e.g. a baseline
        # written here) are ignored.
        journals: dict[str, Path] = {
            journalless_id(Path(p)): Path(p)
            for p in glob.glob(str(sessions_dir / "*.journal.jsonl"))
        }
        snapshots: dict[str, Path] = {}
        for spath in glob.glob(str(sessions_dir / "*.json")):
            path = Path(spath)
            if looks_like_snapshot(path):
                snapshots[journalless_id(path)] = path
        entries = sorted(
            journals.keys() | snapshots.keys(),
            key=lambda sid: (
                json_meta_field(snapshots.get(sid) or journals[sid], "updated_at")
                or datetime.fromtimestamp(
                    (snapshots.get(sid) or journals[sid]).stat().st_mtime, tz=timezone.utc
                ).isoformat()
            ),
            reverse=True,
        )
        for sid in entries:
            snapshot, journal = snapshots.get(sid), journals.get(sid)
            path = snapshot or journal
            try:
                if snapshot is not None and journal is not None:
                    session = load_pair(snapshot, journal)
                    if workspace and session["metadata"].get("workspace") not in workspace_variants:
                        continue
                elif snapshot is not None:
                    session = load_snapshot(snapshot)
                    if workspace and session["metadata"].get("workspace") not in workspace_variants:
                        continue
                else:
                    if workspace:
                        continue  # journal-only session: no workspace metadata
                    session = fold_journal(journal)
            except (ValueError, OSError, json.JSONDecodeError) as exc:
                print(f"journal_metrics: skipping unreadable record: {exc}", file=sys.stderr)
                skipped.append(str(path))
                continue
            picked.append(session)
            if len(picked) >= args.limit:
                break
    if skipped:
        print(f"journal_metrics: {len(skipped)} unreadable record(s) skipped", file=sys.stderr)
    if not picked:
        fail("no matching session records (see --workspace/--limit/--session)")
    return picked


def journalless_id(path: Path) -> str:
    return path.name.removesuffix(".journal.jsonl").removesuffix(".json")


def looks_like_snapshot(path: Path) -> bool:
    try:
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
    except (OSError, json.JSONDecodeError):
        return False
    return "schema_version" in data and isinstance(data.get("messages"), list)


def json_meta_field(path: Path, field: str) -> str:
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh).get("metadata", {}).get(field, "") or ""
    except (OSError, json.JSONDecodeError):
        return ""


# ── detectors ───────────────────────────────────────────────────────────────


def analyze(session: dict) -> dict:
    calls: list[str] = []  # tool names in call order
    results: list[tuple[str, bool]] = []  # (tool name, is_error) in result order
    id_to_name: dict[str, str] = {}
    assistant_text = 0
    give_up_hits = 0

    for message in session.get("messages", []):
        role = message.get("role")
        for block in blocks(message):
            btype = block.get("type")
            if role == "assistant" and btype == "tool_use":
                calls.append(block.get("name", "?"))
                id_to_name[block.get("id", "")] = block.get("name", "?")
            elif btype == "tool_result":
                results.append(
                    (id_to_name.get(block.get("tool_use_id", ""), "?"),
                     bool(block.get("is_error", False)))
                )
            elif role == "assistant" and btype == "text":
                assistant_text += 1
                if GIVE_UP_RE.search(block.get("text", "")):
                    give_up_hits += 1

    # longest run of equal consecutive tool names
    max_run, run = 0, 0
    for i, name in enumerate(calls):
        run = run + 1 if i and name == calls[i - 1] else 1
        max_run = max(max_run, run)

    errors = [name for name, is_error in results if is_error]
    recoveries = 0
    for i, (name, is_error) in enumerate(results):
        if not is_error:
            continue
        for later_name, later_error in results[i + 1:]:
            if later_name == name and not later_error:
                recoveries += 1
                break

    fired = []
    if calls and max_run >= LOOP_RUN_THRESHOLD:
        fired.append("loop")
    if errors:
        fired.append("error")
    if give_up_hits:
        fired.append("give_up")

    return {
        "id": session.get("id", "?"),
        "mode": session.get("metadata", {}).get("mode"),
        "model": session.get("metadata", {}).get("model"),
        "updated_at": session.get("metadata", {}).get("updated_at"),
        "messages": len(session.get("messages", [])),
        "tool_calls": len(calls),
        "tool_results": len(results),
        "tool_errors": len(errors),
        "recoveries": recoveries,
        "max_tool_run": max_run,
        "failed_rounds": session.get("failed_rounds", 0),
        "assistant_text_messages": assistant_text,
        "give_up_hits": give_up_hits,
        "fired": fired,
    }


def metric(num: int, den: int) -> dict:
    return {
        "numerator": num,
        "denominator": den,
        "value": round(num / den, 4) if den else 0.0,
        "directional_only": 0 < den < SMALL_SAMPLE or den == 0,
    }


def aggregate(rows: list[dict]) -> dict:
    tool_sessions = [r for r in rows if r["tool_calls"]]
    return {
        "loop_rate": metric(
            sum(1 for r in tool_sessions if "loop" in r["fired"]), len(tool_sessions)
        ),
        "error_rate": metric(
            sum(r["tool_errors"] for r in rows), sum(r["tool_results"] for r in rows)
        ),
        "recovery_rate": metric(
            sum(r["recoveries"] for r in rows), sum(r["tool_errors"] for r in rows)
        ),
        "give_up_rate": metric(
            sum(r["give_up_hits"] for r in rows),
            sum(r["assistant_text_messages"] for r in rows),
        ),
        "clean_session_rate": metric(
            sum(1 for r in rows if not r["fired"]), len(rows)
        ),
        "failed_rounds_total": metric(
            sum(r["failed_rounds"] for r in rows), len(rows)
        ),
    }


# ── reports ─────────────────────────────────────────────────────────────────


def summary_markdown(payload: dict) -> str:
    lines = [
        f"# Session metrics — {payload['n_sessions']} session(s)"
        f" ({payload['workspace'] or 'all workspaces'})",
        "",
        "| metric | value | n |",
        "|---|---|---|",
    ]
    for name, m in payload["metrics"].items():
        flag = " *(directional-only)*" if m["directional_only"] else ""
        lines.append(
            f"| {name} | {m['value']:.3f}{flag} | {m['numerator']}/{m['denominator']} |"
        )
    lines += ["", "## Sessions", "", "| id | mode | msgs | tools | errors | fired |", "|---|---|---|---|---|---|"]
    for r in payload["sessions"]:
        fired = ", ".join(r["fired"]) or "-"
        lines.append(
            f"| {r['id'][:8]} | {r['mode'] or '?'} | {r['messages']} | "
            f"{r['tool_calls']} | {r['tool_errors']} | {fired} |"
        )
    return "\n".join(lines)


def compare_markdown(before: dict, after: dict) -> str:
    lines = [
        f"# Metrics compare — before {before['n_sessions']} session(s),"
        f" after {after['n_sessions']}",
        "",
        "| metric | before | after | Δ |",
        "|---|---|---|---|",
    ]
    for name, new in after["metrics"].items():
        old = before.get("metrics", {}).get(name)
        if old is None:
            lines.append(f"| {name} | — | {new['value']:.3f} | new |")
            continue
        delta = new["value"] - old["value"]
        arrow = "▲" if delta > 0 else ("▼" if delta < 0 else "=")
        lines.append(
            f"| {name} | {old['value']:.3f} | {new['value']:.3f} | {arrow} {delta:+.3f} |"
        )
    return "\n".join(lines)


# ── entry ───────────────────────────────────────────────────────────────────


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--sessions-dir",
        default=os.environ.get("CODESMITH_HOME", os.path.expanduser("~/.codesmith")) + "/sessions",
        help="sessions directory (default: $CODESMITH_HOME/sessions)",
    )
    parser.add_argument(
        "--workspace", default=os.getcwd(),
        help="only sessions whose metadata.workspace equals this (default: cwd; empty string disables)",
    )
    parser.add_argument("--limit", type=int, default=20, help="most recent sessions to scan (default 20)")
    parser.add_argument("--session", action="append", help="explicit session id (repeatable)")
    parser.add_argument("--json", dest="json_path", help="write the full metrics JSON here")
    parser.add_argument("--compare", help="print a before/after report against this stored JSON")
    args = parser.parse_args()

    rows = [analyze(s) for s in collect_sessions(args)]
    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "sessions_dir": str(Path(args.sessions_dir)),
        "workspace": args.workspace or None,
        "n_sessions": len(rows),
        "sessions": rows,
        "metrics": aggregate(rows),
    }

    if args.json_path:
        Path(args.json_path).write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    if args.compare:
        try:
            with open(args.compare, encoding="utf-8") as fh:
                before = json.load(fh)
        except (OSError, json.JSONDecodeError) as exc:
            fail(f"cannot read baseline {args.compare}: {exc}")
        print(compare_markdown(before, payload))
    else:
        print(summary_markdown(payload))


if __name__ == "__main__":
    main()
