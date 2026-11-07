# CodeSmith Desktop

Tauri 2 shell over a vendored, rebranded client UI, bridged onto
`codesmith serve --http`. Opened by `--preset all` instead of the terminal
REPL.

Everything lives in [../docs/DESKTOP.md](../docs/DESKTOP.md)
(中文版 [../docs/DESKTOP_cn.md](../docs/DESKTOP_cn.md)): architecture, the
vendor/rebrand pipeline (`scripts/vendor_client.py`, `scripts/check_brand.py`),
build & run commands, and known limitations.

Quick start (the vendored tree is committed — start at `pnpm dev` unless
re-vendoring):

```sh
# Re-vendoring only: the upstream checkout must be BUILT first, with the full
# pipeline (per-package tsdown runs produce bundles with unresolved externals):
#   pnpm install && pnpm run build:lib:host && pnpm run build:lib:client
python3 scripts/vendor_client.py && node scripts/gen_boot_graph.mjs
cd ui && pnpm install && pnpm dev      # vite :1420
CODESMITH_DESKTOP_CODESMITH=<repo>/target/debug/codesmith \
  cargo run --manifest-path src-tauri/Cargo.toml
```
