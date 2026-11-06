#!/usr/bin/env python3
"""Brand self-check for desktop/: no upstream brand text in code or vendored artifacts.

Runs the same match as

  grep -ril 'deepseek\\|dsh' desktop/ --include='*.rs' --include='*.ts' \\
      --include='*.tsx' --include='*.js' --include='*.json'

(plus .mjs/.cjs/.html/.css/.toml, which are code here too) and classifies
every hit. A brand hit is `deepseek` in any case, `DSH`, `Dsh`, or a
lowercase `dsh` not preceded by a letter. The case-insensitive grep also
matches English words that merely contain the letters (hanDSHake,
spreaDSHeet, uploaDSHeldText); those are reported as incidental, never
failed.

Exempt: third-party and build caches (node_modules, target), and the
provenance record ui/vendor/VENDOR.json (the upstream source path, upstream
package names and the rewrite table itself). ui/vendor/THIRD_PARTY_NOTICES.md
is not code and keeps the upstream copyright line (MIT).

Exit status: 0 when no brand hit remains, 1 otherwise.
"""

from __future__ import annotations

import os
import re
import sys
from collections import Counter
from pathlib import Path

DESKTOP = Path(__file__).resolve().parent.parent
SUFFIXES = {".rs", ".ts", ".tsx", ".js", ".json", ".mjs", ".cjs", ".html", ".css", ".toml"}
SKIP_DIRS = {"node_modules", "target"}
EXEMPT = {DESKTOP / "ui" / "vendor" / "VENDOR.json"}

ANY_HIT = re.compile(r"(?i)deepseek|dsh")
BRAND = re.compile(r"(?i:deepseek)|DSH|Dsh|(?<![A-Za-z])dsh")
WORD = re.compile(r"[A-Za-z0-9_$]+")


def main() -> int:
    brand_hits: list[str] = []
    incidental: Counter[str] = Counter()
    literal_files = 0
    for dirpath, dirnames, filenames in os.walk(DESKTOP):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)
        for filename in sorted(filenames):
            path = Path(dirpath) / filename
            if path.suffix not in SUFFIXES or path in EXEMPT:
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            if not ANY_HIT.search(text):
                continue
            literal_files += 1
            for match in ANY_HIT.finditer(text):
                if BRAND.match(text, match.start()):
                    line = text.count("\n", 0, match.start()) + 1
                    start = max(0, match.start() - 30)
                    snippet = text[start:match.end() + 30].replace("\n", " ")
                    brand_hits.append(f"{path.relative_to(DESKTOP.parent)}:{line}: …{snippet}…")
                    continue
                word = next((w.group(0) for w in WORD.finditer(text, max(0, match.start() - 40))
                             if w.start() <= match.start() < w.end()), match.group(0))
                incidental[word] += 1
    for hit in brand_hits[:20]:
        print(hit)
    if len(brand_hits) > 20:
        print(f"… {len(brand_hits) - 20} more")
    by_file = Counter(hit.split(":", 1)[0] for hit in brand_hits)
    for path, n in by_file.most_common():
        print(f"  {n:6d}  {path}")
    words = ", ".join(f"{word}×{n}" for word, n in incidental.most_common())
    print(f"brand check: {len(brand_hits)} brand hit(s); the literal grep lists {literal_files} "
          f"file(s), the rest are incidental words: {words or 'none'}")
    return 1 if brand_hits else 0


if __name__ == "__main__":
    sys.exit(main())
