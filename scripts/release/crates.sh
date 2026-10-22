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
