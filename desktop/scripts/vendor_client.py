#!/usr/bin/env python3
"""Vendor the upstream client closure into desktop/ui/vendor, rebranded.

Copies each closure package's package.json + lib/ (prebuilt by the upstream
`pnpm run build:lib:client`) + src/ + assets/ + locale/ from a local upstream
checkout, applies the exact-string VENDOR_PATCHES (product copy) to the
upstream bytes, then the mechanical REWRITES table (package scope, globals,
env names, CSS/DOM prefixes, prose) to every vendored text file, and syncs
the result into the output tree (stale files are pruned). Records the source
commit, the rewrite table and per-file hashes in VENDOR.json; re-running
against an unchanged checkout writes nothing (idempotent).

Usage:
  python3 desktop/scripts/vendor_client.py \
      [--upstream /Users/xinyiqi/Dev/TypeScript/deepseek-harness]

The checkout must already be built (`pnpm run build:lib:client`) — this
script never builds upstream code, it only copies artifacts.

Known limitations: the rewrite is textual. It stays coherent because every
rule is applied to every vendored text file in one pass (an identifier shared
across bundles — a module-table specifier, a CSS variable, a global — is
renamed everywhere at once), and the lowercase prefix keeps its length
(`dsh` -> `cds`), so hard-coded offsets such as `key.slice(21)` after
`'dsh.sidebar-right.v1.'` stay valid. The length-changing rules (scope,
globals, prose) were audited for numeric offsets at the recorded commit; a
new upstream offset that depends on a brand string's length would not be
caught by this script. Binary files and LICENSE texts are copied verbatim.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from collections import Counter
from pathlib import Path

UPSTREAM_SCOPE = "@deepseek-ai/"
VENDOR_SCOPE = "@codesmith/"

# Seed libraries the boot kernel's static module table imports through the
# shell bundle (packages/client/web/src/seed.ts getStaticModules) — declared
# upstream as devDependencies, so the closure needs them as explicit roots.
SEED_LIBRARY_ENTRIES = [
    "@deepseek-ai/dsh-client-store",
    "@deepseek-ai/dsh-client-ui-slots",
    "@deepseek-ai/dsh-client-ui-primitives",
    "@deepseek-ai/dsh-client-ui-dockkit",
]

# Required client entries (packages/client/web/src/boot-client.ts
# `requiredClientEntryNames`) plus the boot kernel app entry.
REQUIRED_ENTRIES = [
    "@deepseek-ai/dsh-client-web",  # boot kernel: static-modules seed + AppWebEntry
    "@deepseek-ai/dsh-client-modules",
    "@deepseek-ai/dsh-api-session-controller",
    "@deepseek-ai/dsh-client-ui-renderer",
    "@deepseek-ai/dsh-client-ui-layout",
    "@deepseek-ai/dsh-client-ui-session",
    "@deepseek-ai/dsh-client-ui-chat",
    "@deepseek-ai/dsh-client-ui-sidebar",
]

# CodeSmith-chosen additions: upstream UI whose interaction CodeSmith's
# runtime API can drive (approvals, user questions, tool rows, model
# selection, skills, secret masking, workflow runs, the settings shell, the
# shortcuts reference, the trajectory view, the schedule page over
# /v1/automations, the attachment presentation — the composer's attachment
# rail and drop overlay, over the bridged upload pipeline — the jobs popover
# over /v1/tasks, the changes-review tab over the runtime's per-call
# file-tool diffs, the sidebar files tree + document preview over the
# session workspace, and the Models settings page over config.toml
# providers + the keyring). Upstream pages with no CodeSmith equivalent
# (plugins, account, goal, plan preview, sidebar terminal/browser) stay
# unvendored so they never appear.
EXTRA_UI_ENTRIES = [
    "@deepseek-ai/dsh-client-ui-approval",
    "@deepseek-ai/dsh-client-ui-user-questions",
    "@deepseek-ai/dsh-client-ui-tool",
    "@deepseek-ai/dsh-client-ui-model-selection",
    "@deepseek-ai/dsh-client-ui-skill",
    "@deepseek-ai/dsh-client-ui-secret-mask",
    "@deepseek-ai/dsh-client-ui-workflow-run",
    "@deepseek-ai/dsh-client-ui-settings-general",
    "@deepseek-ai/dsh-client-ui-settings-models",
    "@deepseek-ai/dsh-client-ui-shortcuts",
    "@deepseek-ai/dsh-client-ui-trajectory",
    "@deepseek-ai/dsh-client-ui-schedule",
    "@deepseek-ai/dsh-client-ui-attachment",
    "@deepseek-ai/dsh-client-ui-jobs",
    "@deepseek-ai/dsh-api-job-controller",
    "@deepseek-ai/dsh-client-ui-deliverables",
    "@deepseek-ai/dsh-client-ui-sidebar-files",
    "@deepseek-ai/dsh-client-ui-sidebar-documentpreview",
    "@deepseek-ai/dsh-api-workspace-files",
]

# Scan roots for scoped packages inside the upstream checkout.
SCAN_ROOTS = ("packages/client", "packages/api", "packages/typert", "packages/util", "vendor")

# Per-package payload directories/files copied (tests excluded). READMEs are
# upstream docs, not runtime or license material, so they are not vendored.
COPY_NAMES = ("package.json", "lib", "src", "assets", "locale", "LICENSE", "LICENSE.md")

# Kept byte-for-byte: license texts must not be rewritten.
VERBATIM_NAMES = ("LICENSE", "LICENSE.md")

# Suffixes the rewrite table applies to; every other file is copied verbatim.
TEXT_SUFFIXES = {".js", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".json", ".css",
                 ".html", ".svg", ".md", ".txt", ".yaml", ".yml"}

# npm publish metadata pointing at the upstream repository; provenance lives
# in VENDOR.json instead.
PUBLISH_FIELDS = ("repository", "publishConfig", "homepage", "bugs")

# Brand patches applied to the upstream bytes before the rewrite table (both
# the source and the built bundle): CodeSmith product copy replaces upstream
# strings. A missing occurrence fails loud so an upstream rename cannot
# silently revert the patch.
VENDOR_PATCHES: list[dict[str, str]] = [
    {
        "package": "@deepseek-ai/dsh-client-ui-conversation",
        "files": ["src/client/locales.ts", "lib/client.js"],
        "find": "探索未至之境",
        "replace": "开启新的探索",
    },
    {
        "package": "@deepseek-ai/dsh-client-ui-conversation",
        "files": ["src/client/locales.ts", "lib/client.js"],
        "find": "可能是其他正在运行的 DSH 导致的（如其他 dsh web、桌面端），请退出其他正在运行的 DSH 后重试",
        "replace": "可能是其他正在运行的 CodeSmith 导致的（如其他桌面端），请退出后重试",
    },
    {
        "package": "@deepseek-ai/dsh-client-ui-conversation",
        "files": ["src/client/locales.ts", "lib/client.js"],
        "find": "possibly by another running DSH instance (such as dsh web or the desktop app). Quit other running DSH instances and try again",
        "replace": "possibly by another running CodeSmith instance (such as the desktop app). Quit other running CodeSmith instances and try again",
    },
    {
        "package": "@deepseek-ai/dsh-client-locale",
        "files": ["src/locales/zh.ts", "lib/client.js"],
        "find": "DSH 本地构建",
        "replace": "CodeS",
    },
    {
        "package": "@deepseek-ai/dsh-client-locale",
        "files": ["src/locales/en.ts", "lib/client.js"],
        "find": "DSH Local Build",
        "replace": "CodeS",
    },
    {
        "package": "@deepseek-ai/dsh-client-web",
        "files": ["src/boot-page.ts"],
        "find": "css.wordmark, 'HARNESS'",
        "replace": "css.wordmark, 'CodeS'",
    },
    {
        "package": "@deepseek-ai/dsh-client-web",
        "files": ["lib/index.js"],
        "find": 'css.wordmark, "HARNESS"',
        "replace": 'css.wordmark, "CodeS"',
    },
]


def build_rewrites(checkout: Path) -> list[tuple[str, str]]:
    """The ordered (regex, replacement) table — most specific forms first."""
    return [
        # Build-machine paths baked into bundles as module region ids.
        (re.escape(str(checkout)), "/upstream"),
        # Package scope and the bundler identifiers derived from it.
        (r"@deepseek-ai/dsh-", VENDOR_SCOPE),
        (re.escape(UPSTREAM_SCOPE), VENDOR_SCOPE),
        (r"_deepseek_ai_dsh_", "_codesmith_"),
        (r"_deepseek_ai_", "_codesmith_"),
        # Product names and prose.
        (r"DeepSeek Harness", "CodeSmith"),
        (r"deepseek-harness", "codesmith"),
        (r"deepseek-ai", "codesmith"),
        (r"DeepSeek", "CodeSmith"),
        (r"Deepseek", "Codesmith"),
        (r"DEEPSEEK", "CODESMITH"),
        (r"deepseek", "codesmith"),
        # Hump case the word-boundary rules above miss: locale keys and
        # identifiers (`deepSeekEndpointHint`, `deepSeekAccount`). Both a key
        # and its lookups live in the one pass, so renames stay coherent.
        (r"deepSeek", "codesmith"),
        # Globals (`__DSH_BOOT__`) and env names (`DSH_HOME`) merge into the
        # CODESMITH_ namespace; remaining uppercase/camel forms are prose or
        # identifiers (`isDSHRemoteError`, `parseDshClient`).
        (r"DSH_", "CODESMITH_"),
        (r"DSH", "CodeSmith"),
        (r"Dsh", "CodeSmith"),
        # Lowercase prefix (`--dsh-*` CSS vars, `dsh.` storage keys,
        # `dsh-resource://`, `data-dsh-*`, `\0dsh-css:`): length-preserving.
        # A preceding letter means an English word (hanDSHake, spreaDSHeet).
        (r"(?<![A-Za-z])dsh(?=[-._A-Za-z0-9])", "cds"),
        # The standalone word (`"dsh": {client}` manifest key, `pkg.dsh`,
        # "dsh web" prose).
        (r"(?<![A-Za-z0-9_$])dsh(?![A-Za-z0-9_$])", "codesmith"),
    ]


# Brand tokens that must not survive the rewrite (validated on every output).
BRAND_RESIDUE = re.compile(r"(?i:deepseek)|DSH|Dsh|(?<![A-Za-z])dsh")


class Rewriter:
    def __init__(self, rewrites: list[tuple[str, str]]) -> None:
        self.rules = [(re.compile(pattern), pattern, replacement)
                      for pattern, replacement in rewrites]

    def text(self, text: str, counts: Counter | None = None) -> str:
        for regex, pattern, replacement in self.rules:
            text, n = regex.subn(replacement, text)
            if n and counts is not None:
                counts[pattern] += n
        return text

    def name(self, upstream: str) -> str:
        name = self.text(upstream)
        if not name.startswith(VENDOR_SCOPE) or BRAND_RESIDUE.search(name):
            raise SystemExit(f"vendor: cannot rebrand package name {upstream!r} (got {name!r})")
        return name


def scan_packages(checkout: Path) -> dict[str, Path]:
    """Map upstream package name -> checkout directory."""
    found: dict[str, Path] = {}
    for root in SCAN_ROOTS:
        base = checkout / root
        if not base.is_dir():
            continue
        for pkg_json in base.glob("*/package.json"):
            try:
                name = json.loads(pkg_json.read_text(encoding="utf-8")).get("name", "")
            except json.JSONDecodeError:
                continue
            if name.startswith(UPSTREAM_SCOPE):
                found[name] = pkg_json.parent
    return found


def closure_of(packages: dict[str, Path], entries: list[str]) -> tuple[list[str], dict[str, list[str]]]:
    """Expand entries over scoped deps, peerDeps and the client inject lists.

    Returns (ordered closure, edges). Deps outside `packages` (npm registry
    packages or the shared modules table) are not followed; they are either
    bundled into each lib/client.browser.js by the upstream build or provided
    host-side through the static modules seed.
    """
    edges: dict[str, list[str]] = {}
    seen: set[str] = set()
    order: list[str] = []
    stack = list(reversed(entries))
    while stack:
        name = stack.pop()
        if name in seen or name not in packages:
            continue
        seen.add(name)
        order.append(name)
        pkg = json.loads((packages[name] / "package.json").read_text(encoding="utf-8"))
        deps: set[str] = set()
        # Library rows (no client plugin face) are consumed through the shell
        # bundle's own imports, so their build-time devDependencies are
        # runtime edges here and must be followed; plugin rows never are (the
        # classic bundles carry or resolve them through the module table).
        follow_dev = "client" not in (pkg.get("dsh") or {})
        keys = ("dependencies", "peerDependencies", "optionalDependencies")
        if follow_dev:
            keys = (*keys, "devDependencies")
        for key in keys:
            deps.update(d for d in pkg.get(key, {}) if d.startswith(UPSTREAM_SCOPE))
        inject = pkg.get("dsh", {}).get("client", {}).get("inject", [])
        deps.update(inject)
        edges[name] = sorted(deps)
        stack.extend(reversed(sorted(deps)))
    return order, edges


def collect_files(src: Path) -> dict[str, bytes]:
    """Upstream payload of one package (package.json handled separately)."""
    skip_dirs = {"node_modules", "tests", "dist"}
    files: dict[str, bytes] = {}
    for copy_name in COPY_NAMES:
        path = src / copy_name
        if copy_name == "package.json" or not path.exists():
            continue
        if path.is_file():
            files[copy_name] = path.read_bytes()
            continue
        for dirpath, dirnames, filenames in os.walk(path):
            dirnames[:] = sorted(d for d in dirnames if d not in skip_dirs)
            for filename in sorted(filenames):
                if filename.endswith((".map", ".tsbuildinfo")):
                    continue
                f = Path(dirpath) / filename
                files[str(f.relative_to(src))] = f.read_bytes()
    return files


def vendored_package_json(src: Path, vendored: set[str]) -> tuple[bytes, list[str]]:
    """package.json minus unvendored scoped deps and npm publish metadata."""
    pkg = json.loads((src / "package.json").read_text(encoding="utf-8"))
    pruned: list[str] = []
    # Prune scoped deps that are not vendored: they are build-time / type-only
    # edges upstream (the runtime classic bundles carry what they need);
    # leaving them in would make this subtree uninstallable.
    for key in ("dependencies", "peerDependencies", "optionalDependencies", "devDependencies"):
        deps = pkg.get(key)
        if not deps:
            continue
        for dep in [d for d in deps if d.startswith(UPSTREAM_SCOPE) and d not in vendored]:
            del deps[dep]
            pruned.append(f"{key}:{dep}")
    for field in PUBLISH_FIELDS:
        pkg.pop(field, None)
    return json.dumps(pkg, indent=2, ensure_ascii=False).encode() + b"\n", pruned


def apply_patches(name: str, files: dict[str, bytes], pkg_dir: Path) -> list[str]:
    patched: list[str] = []
    for patch in VENDOR_PATCHES:
        if patch["package"] != name:
            continue
        for rel in patch["files"]:
            if rel not in files:
                continue
            data = files[rel].decode("utf-8")
            if patch["find"] not in data:
                raise SystemExit(
                    f"vendor patch: {patch['find']!r} not found in {pkg_dir / rel} — "
                    "upstream changed; update VENDOR_PATCHES")
            files[rel] = data.replace(patch["find"], patch["replace"]).encode("utf-8")
            patched.append(rel)
    return patched


def rebrand_files(files: dict[str, bytes], rewriter: Rewriter, counts: Counter, pkg_dir: Path) -> None:
    branded: dict[str, bytes] = {}
    for rel, data in files.items():
        if Path(rel).name in VERBATIM_NAMES or Path(rel).suffix not in TEXT_SUFFIXES:
            continue
        text = rewriter.text(data.decode("utf-8"), counts)
        residue = BRAND_RESIDUE.search(text)
        if residue:
            start = max(0, residue.start() - 40)
            raise SystemExit(
                f"vendor: rewrite table left brand text in {pkg_dir / rel}: "
                f"…{text[start:residue.end() + 40]!r}… — extend build_rewrites")
        branded[rel] = text.encode("utf-8")
    # A branded file name (upstream `DeepSeekModelsEditor.tsx`) is rewritten by
    # the SAME table that rewrites the import specifiers naming it, so the
    # renamed file and its renamed importers stay in lockstep.
    renamed = {rewriter.text(rel): data for rel, data in branded.items()}
    for rel, data in renamed.items():
        if BRAND_RESIDUE.search(rel):
            raise SystemExit(f"vendor: rewrite table left brand text in a file path: {pkg_dir / rel}")
    files.update(renamed)
    for rel in branded.keys() - renamed.keys():
        del files[rel]


def sync_tree(desired: dict[Path, bytes], managed: list[Path], legacy: list[Path]) -> tuple[int, int]:
    """Write changed files, prune stale ones under `managed`, drop `legacy` trees.

    pnpm-owned `node_modules` directories inside package dirs are left alone.
    """
    written = removed = 0
    for path, data in desired.items():
        if path.is_file() and path.read_bytes() == data:
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        written += 1
    for root in managed:
        if not root.is_dir():
            continue
        for dirpath, dirnames, filenames in os.walk(root, topdown=False):
            if "node_modules" in Path(dirpath).relative_to(root).parts:
                continue
            for filename in filenames:
                f = Path(dirpath) / filename
                if f not in desired:
                    f.unlink()
                    removed += 1
            if Path(dirpath) != root and not any(Path(dirpath).iterdir()):
                Path(dirpath).rmdir()
    for root in legacy:
        if root.is_dir():
            removed += sum(len(fs) for _, _, fs in os.walk(root))
            shutil.rmtree(root)
    return written, removed


def notices_text(checkout: Path, commit: str) -> str:
    license_path = checkout / "LICENSE"
    if not license_path.is_file():
        raise SystemExit(f"vendor: upstream LICENSE missing at {license_path}")
    return (
        "# Third-party notices\n\n"
        "The packages under this directory are vendored from DeepSeek Harness\n"
        f"(source checkout `{checkout}`, commit `{commit}`), MIT licensed.\n"
        "They were copied from the upstream build output and mechanically\n"
        "rebranded by `desktop/scripts/vendor_client.py` (package scope,\n"
        "globals, CSS/DOM prefixes and product copy; the rewrite table is\n"
        "recorded in VENDOR.json). Packages that ship their own LICENSE file\n"
        "keep it verbatim.\n\n"
        "## DeepSeek Harness license\n\n"
        "```\n" + license_path.read_text(encoding="utf-8").rstrip("\n") + "\n```\n"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--upstream", "--dsh", dest="upstream", default=os.environ.get(
        "CODESMITH_DESKTOP_UPSTREAM", "/Users/xinyiqi/Dev/TypeScript/deepseek-harness"),
        help="upstream client checkout (env CODESMITH_DESKTOP_UPSTREAM)")
    parser.add_argument("--out", default=str(Path(__file__).resolve().parent.parent
                                             / "ui" / "vendor"))
    args = parser.parse_args()
    checkout = Path(args.upstream).resolve()
    out = Path(args.out).resolve()
    if not checkout.is_dir():
        print(f"vendor: upstream checkout not found: {checkout}", file=sys.stderr)
        return 1

    commit = subprocess.run(["git", "-C", str(checkout), "rev-parse", "HEAD"],
                            capture_output=True, text=True, check=True).stdout.strip()

    packages = scan_packages(checkout)
    missing_entries = [e for e in [*REQUIRED_ENTRIES, *EXTRA_UI_ENTRIES] if e not in packages]
    if missing_entries:
        print(f"vendor: entries missing from checkout: {missing_entries}", file=sys.stderr)
        return 1
    order, edges = closure_of(packages, [*SEED_LIBRARY_ENTRIES, *REQUIRED_ENTRIES, *EXTRA_UI_ENTRIES])

    unbuilt = [n for n in order if not (packages[n] / "lib").is_dir()]
    if unbuilt:
        print(f"vendor: packages have no lib/ (run `pnpm run build:lib:client` in the "
              f"checkout first): {unbuilt}", file=sys.stderr)
        return 1

    rewrites = build_rewrites(checkout)
    rewriter = Rewriter(rewrites)
    names = {name: rewriter.name(name) for name in order}
    if len(set(names.values())) != len(names):
        raise SystemExit(f"vendor: rebranded package names collide: {sorted(names.values())}")

    vendored = set(order)
    desired: dict[Path, bytes] = {}
    records: dict[str, dict] = {}
    for name in order:
        src = packages[name]
        files = collect_files(src)
        files["package.json"], pruned = vendored_package_json(src, vendored)
        patched = apply_patches(name, files, src)
        counts: Counter = Counter()
        rebrand_files(files, rewriter, counts, src)
        dst = out / names[name]
        for rel, data in files.items():
            desired[dst / rel] = data
        pkg = json.loads((src / "package.json").read_text(encoding="utf-8"))
        records[names[name]] = {
            "upstream": name,
            "dir": str(src.relative_to(checkout)),
            "entry": "client.browser.js" if (src / "lib" / "client.browser.js").exists() else "client.js",
            "plugin": "client" in (pkg.get("dsh") or {}),
            "files": {rel: hashlib.sha256(data).hexdigest() for rel, data in sorted(files.items())},
            "deps": [rewriter.name(dep) for dep in edges[name]],
            "pruned_deps": sorted(pruned),
            "patched_files": sorted(patched),
            "rewritten": sum(counts.values()),
        }

    manifest = {
        "source": str(checkout),
        "source_commit": commit,
        "rewrites": [list(rule) for rule in rewrites],
        "packages": records,
    }
    desired[out / "VENDOR.json"] = (json.dumps(manifest, indent=1, sort_keys=True, ensure_ascii=False)
                                    + "\n").encode("utf-8")
    desired[out / "THIRD_PARTY_NOTICES.md"] = notices_text(checkout, commit).encode("utf-8")

    written, removed = sync_tree(desired,
                                 managed=[out / VENDOR_SCOPE.rstrip("/")],
                                 legacy=[out / UPSTREAM_SCOPE.rstrip("/")])
    if written == 0 and removed == 0:
        print(f"vendor: up to date at {commit[:12]} ({len(order)} packages)")
    else:
        print(f"vendor: {len(order)} packages at {commit[:12]} -> {out} "
              f"({written} written, {removed} removed)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
