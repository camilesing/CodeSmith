#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"
# shellcheck source=scripts/release/crates.sh
source "${script_dir}/crates.sh"

usage() {
  cat <<'EOF'
usage: scripts/release/check-published.sh [--allow-npm-binary-mismatch] [VERSION]

Verifies that a release version is visible on both npm and crates.io.
Defaults VERSION to the workspace version in Cargo.toml.

Use --allow-npm-binary-mismatch only for npm packaging-only releases where
the npm package intentionally points at an older GitHub binary release.
EOF
}

allow_npm_binary_mismatch=0
version=""

while (($# > 0)); do
  case "$1" in
    --allow-npm-binary-mismatch)
      allow_npm_binary_mismatch=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      # A typo like 'v1.2.3', a stray flag, or an oddly-expanded glob must
      # not silently probe the wrong version on npm/crates.io and report a
      # misleading 'not published'.
      if [[ -n "${version}" ]] || ! [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
        usage >&2
        exit 2
      fi
      version="$1"
      ;;
  esac
  shift
done

cd "${repo_root}"

if [[ -z "${version}" ]]; then
  # Authoritative source (same as the sibling release scripts): the awk
  # one-liner this replaced only matched `version = "…"` with exactly one
  # space and no trailing comment, silently yielding garbage or empty on
  # every other legal TOML spelling.
  version="$(python3 -c '
import json, subprocess
m = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps"]))
ws = set(m["workspace_members"])
versions = sorted({p["version"] for p in m["packages"] if p["id"] in ws})
print(versions[0] if versions else "")
' 2>/dev/null || true)"
fi

if [[ -z "${version}" ]]; then
  echo "Could not determine release version." >&2
  exit 1
fi

fail=0

echo "Checking published release ${version}..."

# Canonical post-rebrand npm package. Tool/network failures (missing npm
# binary, auth, DNS, timeout) must not read as "not published": capture
# stderr and branch on E404 vs anything else.
npm_manifest=""
if npm_out="$(npm view "codesmith@${version}" --json --fetch-timeout 30000 2>&1)"; then
  npm_manifest="${npm_out}"
  echo "npm codesmith@${version} is published."
elif printf '%s' "${npm_out}" | grep -q 'E404'; then
  echo "npm codesmith@${version} is not published." >&2
  fail=1
else
  echo "npm view codesmith@${version} failed:" >&2
  printf '%s\n' "${npm_out}" >&2
  fail=1
fi

# `codesmithBinaryVersion` is the internal version-pin field.
binary_field=""
npm_binary_version=""
if [[ -n "${npm_manifest}" ]]; then
  # Guarded: a missing `node` (127) or a null/array manifest (npm emits
  # `null` for some no-match cases) would abort the whole script under
  # `set -e` before the crates.io loop ran.
  value="$(printf '%s' "${npm_manifest}" | node -e '
    const fs = require("fs");
    const manifest = JSON.parse(fs.readFileSync(0, "utf8"));
    const entry = Array.isArray(manifest) ? manifest[manifest.length - 1] : manifest;
    console.log((entry && entry.codesmithBinaryVersion) || "");
  ' 2>/dev/null || true)"
  if [[ -n "${value}" ]]; then
    binary_field="codesmithBinaryVersion"
    npm_binary_version="${value}"
  fi
fi

if [[ -n "${binary_field}" ]]; then
  if [[ "${npm_binary_version}" == "${version}" ]]; then
    echo "npm ${binary_field}=${npm_binary_version}."
  elif [[ "${allow_npm_binary_mismatch}" == "1" ]]; then
    echo "npm ${binary_field}=${npm_binary_version} (allowed packaging-only mismatch)."
  else
    echo "npm ${binary_field}=${npm_binary_version}, expected ${version}." >&2
    fail=1
  fi
elif [[ "${allow_npm_binary_mismatch}" == "1" ]]; then
  echo "npm codesmithBinaryVersion is absent (allowed packaging-only mismatch)."
else
  echo "npm codesmithBinaryVersion is absent for codesmith@${version}." >&2
  fail=1
fi

for crate in "${release_crates[@]}"; do
  status="$(crate_version_status "${crate}" "${version}")"
  case "${status}" in
    published)
      echo "crates.io ${crate}@${version} is published."
      ;;
    absent)
      echo "crates.io ${crate}@${version} is not published." >&2
      fail=1
      ;;
    *)
      # Transient network/server failure: distinct message so an operator
      # re-checks instead of re-publishing.
      echo "crates.io ${crate}@${version} could not be queried (network or server error); not concluding it is unpublished." >&2
      fail=1
      ;;
  esac
done

if [[ "${fail}" == "0" ]]; then
  echo "Published release OK: npm codesmith@${version} and ${#release_crates[@]} crates are visible."
fi

exit "${fail}"
