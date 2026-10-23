#!/usr/bin/env bash

# Crates published for each codesmith release, in dependency order.
# Every publishable codesmith-* workspace member belongs here: publish-crates.sh
# cross-checks this list against the workspace and refuses to run when one is
# missing. Order follows the internal dependency graph (dependencies first).
release_crates=(
  codesmith-secrets
  codesmith-protocol
  codesmith-mcp
  codesmith-release
  codesmith-config
  codesmith-execpolicy
  codesmith-hooks
  codesmith-tools
  codesmith-agent
  codesmith-state
  codesmith-extensions
  codesmith-agent-runtime
  codesmith-providers
  codesmith-core
  codesmith-app-server
  codesmith-index
  codesmith-tool-impls
  codesmith-cli
)

# Query one crates.io crate version. Prints "published", "absent" (404), or
# "unknown" (network error, timeout, 5xx) — a transient failure must never be
# read as "not published", or publish scripts re-publish and check scripts
# report false negatives.
crate_version_status() {
  local crate_name="$1"
  local crate_version="$2"
  local status
  status="$(curl -s -o /dev/null -w '%{http_code}' --max-time 30 \
    "https://crates.io/api/v1/crates/${crate_name}/${crate_version}")" || {
    echo "unknown"
    return 0
  }
  case "${status}" in
    200) echo "published" ;;
    404) echo "absent" ;;
    *) echo "unknown" ;;
  esac
}
