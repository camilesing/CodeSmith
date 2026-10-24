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
  # crates.io's API policy requires a User-Agent; bare curl commonly gets
  # 403 (which the status branching would then report as "unknown" for
  # every crate).
  status="$(curl -s -o /dev/null -w '%{http_code}' --max-time 30 \
    -H 'User-Agent: codesmith-release-checks (https://github.com/codesmith)' \
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
