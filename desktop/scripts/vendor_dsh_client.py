#!/usr/bin/env python3
"""Vendor the DSH client closure into desktop/ui/vendor.

Copies each closure package's package.json + lib/ (prebuilt by DSH's own
`pnpm run build:lib:client`) + src/ + assets/ + locale/ from a local DSH
checkout. Records the source commit and per-file hashes in VENDOR.json;
re-running against an unchanged tree rewrites nothing (idempotent).

Usage:
  python3 desktop/scripts/vendor_dsh_client.py \
      [--dsh /Users/camile/Work/TypeScript/deepseek-harness]

The DSH checkout must already be built (`pnpm run build:lib:client`) — this
script never builds upstream code, it only copies artifacts.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

# Required client entries (packages/client/web/src/boot-client.ts
# `requiredClientEntryNames`) plus the boot kernel app entry.
# Seed libraries the boot kernel's static module table imports through the
# shell bundle (packages/client/web/src/seed.ts getStaticModules) — declared
# upstream as devDependencies, so the closure needs them as explicit roots.
SEED_LIBRARY_ENTRIES = [
    "@deepseek-ai/dsh-client-store",
    "@deepseek-ai/dsh-client-ui-slots",
    "@deepseek-ai/dsh-client-ui-primitives",
    "@deepseek-ai/dsh-client-ui-dockkit",
]

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

# Scan roots for @deepseek-ai packages inside the DSH checkout.
SCAN_ROOTS = ("packages/client", "packages/api", "packages/typert", "packages/util", "vendor")

# Per-package payload directories/files copied verbatim (tests excluded).
COPY_NAMES = ("package.json", "lib", "src", "assets", "locale", "LICENSE", "LICENSE.md",
              "README.md", "README.zh.md")


def scan_packages(dsh: Path) -> dict[str, Path]:
    """Map @deepseek-ai package name -> checkout directory."""
    found: dict[str, Path] = {}
    for root in SCAN_ROOTS:
        base = dsh / root
        if not base.is_dir():
            continue
        for pkg_json in base.glob("*/package.json"):
            try:
                name = json.loads(pkg_json.read_text(encoding="utf-8")).get("name", "")
            except json.JSONDecodeError:
                continue
            if name.startswith("@deepseek-ai/"):
                found[name] = pkg_json.parent
    return found


def closure_of(packages: dict[str, Path], entries: list[str]) -> tuple[list[str], dict[str, list[str]]]:
    """Expand entries over @deepseek-ai deps, peerDeps and dsh.client.inject.

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
        # Library rows (no `dsh.client` plugin face) are consumed through the
        # shell bundle's own imports, so their build-time devDependencies are
        # runtime edges here and must be followed; plugin rows never are (the
        # classic bundles carry or resolve them through the module table).
        follow_dev = "client" not in (pkg.get("dsh") or {})
        keys = ("dependencies", "peerDependencies", "optionalDependencies")
        if follow_dev:
            keys = (*keys, "devDependencies")
        for key in keys:
            deps.update(d for d in pkg.get(key, {}) if d.startswith("@deepseek-ai/"))
        inject = pkg.get("dsh", {}).get("client", {}).get("inject", [])
        deps.update(inject)
        edges[name] = sorted(deps)
        stack.extend(reversed(sorted(deps)))
    return order, edges


def copy_tree_contents(src: Path, dst: Path, copied: dict[str, str]) -> None:
    skip_parts = {"node_modules", "tests", "dist", ".tsbuildinfo"}
    for item in src.iterdir():
        if item.name in skip_parts:
            continue
        target = dst / item.name
        if item.is_dir():
            for f in sorted(item.rglob("*")):
                if not f.is_file():
                    continue
                if f.suffix == ".map" or f.name.endswith(".tsbuildinfo"):
                    continue
                rel = f.relative_to(src)
                out = dst / rel
                out.parent.mkdir(parents=True, exist_ok=True)
                data = f.read_bytes()
                out.write_bytes(data)
                copied[str(rel)] = hashlib.sha256(data).hexdigest()
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            data = item.read_bytes()
            target.write_bytes(data)
            copied[item.name] = hashlib.sha256(data).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dsh", default=os.environ.get(
        "DSH_HOME", "/Users/camile/Work/TypeScript/deepseek-harness"))
    parser.add_argument("--out", default=str(Path(__file__).resolve().parent.parent
                                             / "ui" / "vendor"))
    args = parser.parse_args()
    dsh = Path(args.dsh).resolve()
    out = Path(args.out).resolve()
    if not dsh.is_dir():
        print(f"vendor: DSH checkout not found: {dsh}", file=sys.stderr)
        return 1

    commit = subprocess.run(["git", "-C", str(dsh), "rev-parse", "HEAD"],
                            capture_output=True, text=True, check=True).stdout.strip()

    packages = scan_packages(dsh)
    missing_entries = [e for e in REQUIRED_ENTRIES if e not in packages]
    if missing_entries:
        print(f"vendor: entries missing from checkout: {missing_entries}", file=sys.stderr)
        return 1
    order, edges = closure_of(packages, [*SEED_LIBRARY_ENTRIES, *REQUIRED_ENTRIES])

    unbuilt = [n for n in order if not (packages[n] / "lib").is_dir()]
    if unbuilt:
        print(f"vendor: packages have no lib/ (run `pnpm run build:lib:client` in the "
              f"checkout first): {unbuilt}", file=sys.stderr)
        return 1

    manifest_path = out / "VENDOR.json"
    old_manifest = None
    if manifest_path.exists():
        try:
            old_manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            old_manifest = None

    vendored = set(order)
    records: dict[str, dict] = {}
    for name in order:
        src = packages[name]
        dst = out / name
        files: dict[str, str] = {}
        for copy_name in COPY_NAMES:
            if copy_name == "package.json" or not (src / copy_name).exists():
                continue
            if (src / copy_name).is_dir():
                copy_tree_contents(src / copy_name, dst / copy_name, files)
            else:
                data = (src / copy_name).read_bytes()
                dst.mkdir(parents=True, exist_ok=True)
                (dst / copy_name).write_bytes(data)
                files[copy_name] = hashlib.sha256(data).hexdigest()
        # Prune @deepseek-ai deps that are not vendored: they are build-time /
        # type-only edges upstream (the runtime classic bundles carry what they
        # need); leaving them in would make this subtree uninstallable.
        pkg = json.loads((src / "package.json").read_text(encoding="utf-8"))
        pruned: list[str] = []
        for key in ("dependencies", "peerDependencies", "optionalDependencies", "devDependencies"):
            deps = pkg.get(key)
            if not deps:
                continue
            for dep in [d for d in deps if d.startswith("@deepseek-ai/") and d not in vendored]:
                del deps[dep]
                pruned.append(f"{key}:{dep}")
        data = json.dumps(pkg, indent=2, ensure_ascii=False).encode() + b"\n"
        (dst / "package.json").write_bytes(data)
        files["package.json"] = hashlib.sha256(data).hexdigest()
        entry = "client.browser.js" if (src / "lib" / "client.browser.js").exists() else "client.js"
        records[name] = {
            "dir": str(src.relative_to(dsh)),
            "entry": entry,
            "plugin": "client" in (pkg.get("dsh") or {}),
            "files": files,
            "deps": edges[name],
            "pruned_deps": sorted(pruned),
        }

    manifest = {
        "source": str(dsh),
        "source_commit": commit,
        "packages": records,
    }
    if old_manifest == manifest:
        print(f"vendor: up to date at {commit[:12]} ({len(order)} packages)")
        return 0

    notices = out / "THIRD_PARTY_NOTICES.md"
    notices.parent.mkdir(parents=True, exist_ok=True)
    notices.write_text(
        "# Third-party notices\n\n"
        "The packages under this directory are vendored from the DeepSeek Harness\n"
        "(`" + str(dsh) + "`, commit `" + commit + "`), MIT licensed, Copyright\n"
        "Hangzhou DeepSeek Artificial Intelligence Co., Ltd. Original license\n"
        "texts are retained in each package's LICENSE / package.json.\n",
        encoding="utf-8")
    manifest_path.write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n",
                             encoding="utf-8")
    print(f"vendor: wrote {len(order)} packages at {commit[:12]} -> {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
