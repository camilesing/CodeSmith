#!/usr/bin/env python3
"""Markdown link guard (round-9 TODO-61).

Every relative link in the repository's top-level markdown files and in
`docs/*.md` must resolve: the target file exists, and a `#fragment` names a
heading in it (GitHub's heading slugs) or an explicit `<a id="…">` anchor.
External links (`http(s)://`, `mailto:`) are not fetched; links inside
fenced code blocks and inline code are ignored. Subdirectories of `docs/`
are out of scope — the same scope `verify-docs-pairs.py` declares (legacy
upstream archives, generated plan artifacts).

`--root` points the guard at another tree (the self-tests in
`scripts/test-guards.py` use it).
"""

import argparse
import re
import sys
import unicodedata
from pathlib import Path

LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
IMAGE = re.compile(r"!\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
EXPLICIT_ANCHOR = re.compile(r"<a\s+(?:id|name)=\"([^\"]+)\"")


def files_in_scope(root: Path) -> list[Path]:
    files = sorted(p for p in root.glob("*.md") if p.is_file())
    files += sorted(p for p in (root / "docs").glob("*.md") if p.is_file())
    return files


def strip_code(text: str, inline: bool = True) -> str:
    """Blank fenced blocks (and inline code spans when `inline`), keeping
    line structure. Headings keep their inline code: GitHub's anchor
    includes the code text."""
    out = []
    fence = None
    for line in text.splitlines():
        marker = re.match(r"^\s*(```+|~~~+)", line)
        if marker:
            token = marker.group(1)[:3]
            if fence is None:
                fence = token
            elif token == fence:
                fence = None
            out.append("")
            continue
        if fence:
            out.append("")
        else:
            out.append(re.sub(r"`[^`]*`", "", line) if inline else line)
    return "\n".join(out)


def slug(heading: str) -> str:
    """GitHub's heading anchor: lowercase, punctuation dropped, spaces to `-`."""
    text = re.sub(r"`([^`]*)`", r"\1", heading)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = text.strip().lower()
    kept = []
    for ch in text:
        category = unicodedata.category(ch)
        if ch == " ":
            kept.append("-")
        elif ch in ("-", "_") or category[0] in ("L", "N") or category in ("Mn", "Pc"):
            kept.append(ch)
        # everything else (backticks' brackets, parentheses, CJK punctuation)
        # is dropped — GitHub keeps CJK letters (Lo) but drops ASCII and CJK
        # punctuation alike
    return "".join(kept)


def anchors_of(path: Path, cache: dict) -> set[str]:
    if path in cache:
        return cache[path]
    text = path.read_text(encoding="utf-8", errors="replace")
    found: set[str] = set(EXPLICIT_ANCHOR.findall(text))
    counts: dict[str, int] = {}
    for line in strip_code(text, inline=False).splitlines():
        match = HEADING.match(line)
        if not match:
            continue
        base = slug(match.group(2))
        seen = counts.get(base, 0)
        found.add(base if seen == 0 else f"{base}-{seen}")
        counts[base] = seen + 1
    cache[path] = found
    return found


def check(root: Path) -> list[str]:
    problems = []
    cache: dict = {}
    for source in files_in_scope(root):
        text = strip_code(source.read_text(encoding="utf-8", errors="replace"))
        for number, line in enumerate(text.splitlines(), start=1):
            for target in LINK.findall(line) + IMAGE.findall(line):
                if re.match(r"^[a-z][a-z0-9+.-]*:", target, re.IGNORECASE):
                    continue
                path_part, _, fragment = target.partition("#")
                path_part = path_part.split("?", 1)[0]
                destination = (source.parent / path_part).resolve() if path_part else source
                where = f"{source.relative_to(root)}:{number}"
                if not destination.exists():
                    problems.append(f"{where}: missing target {target}")
                    continue
                if fragment and destination.suffix == ".md":
                    if fragment.lower() not in {a.lower() for a in anchors_of(destination, cache)}:
                        problems.append(f"{where}: no heading for #{fragment} in {target}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root to check (default: this repository)",
    )
    args = parser.parse_args()
    root = args.root.resolve()
    problems = check(root)
    if problems:
        print(f"{len(problems)} broken markdown link(s):", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1
    print(f"markdown links ok: {len(files_in_scope(root))} file(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
