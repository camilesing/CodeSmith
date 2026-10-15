#!/usr/bin/env bash
# Fails CI if version state is inconsistent across the workspace, npm
# wrapper, and Cargo.lock. Run on every push/PR so silent drift can't ship.
#
# Checks performed:
#   1. No `crates/*/Cargo.toml` carries a literal `version = "x.y.z"`; every
#      crate must inherit `version.workspace = true`.
#   2. `npm/codesmith/package.json` `version` matches the workspace
#      `version` in the root `Cargo.toml`.
#   3. Internal `codesmith-*` path dependency pins — root
#      `[workspace.dependencies]` entries and any remaining member-local
#      pins — match the workspace version.
#   4. The current release-note section exists in `CHANGELOG.md`.
#   5. The current release has a dated Keep a Changelog entry and compare link.
#   6. README contributor additions are mentioned in the current release entry.
#   7. `SECURITY.md` keeps the dedicated security contact.
#   8. `Cargo.lock` is in sync with the manifests (`cargo metadata --locked`
#      fails if not).
set -euo pipefail

cd "$(dirname "$0")/../.."

fail=0

# 1) Literal versions in crate manifests.
literals="$(grep -nE '^version = "' crates/*/Cargo.toml || true)"
if [[ -n "${literals}" ]]; then
  echo "::error::Crate manifests must use 'version.workspace = true', not literal versions:" >&2
  echo "${literals}" >&2
  fail=1
fi

# 2) Workspace ↔ npm package.json.
workspace_version="$(awk '/^\[workspace\.package\]/{in_ws=1; next} /^\[/{in_ws=0} in_ws && /^version = "/{sub(/^version = "/, ""); sub(/"$/, ""); print; exit}' Cargo.toml || true)"
npm_version="$(node -p "require('./npm/codesmith/package.json').version" 2>/dev/null || true)"
if [[ -z "${workspace_version}" ]]; then
  echo "::error::Could not determine workspace version from Cargo.toml." >&2
  fail=1
fi
if [[ -z "${npm_version}" ]]; then
  echo "::error::Could not determine npm version from npm/codesmith/package.json." >&2
  fail=1
fi
if [[ -n "${workspace_version}" && -n "${npm_version}" && "${workspace_version}" != "${npm_version}" ]]; then
  echo "::error::npm/codesmith/package.json version (${npm_version}) does not match workspace Cargo.toml (${workspace_version})." >&2
  fail=1
fi

# 3) Internal path dependency pins. Since the D6 hoist the versioned pins
#    live in the root [workspace.dependencies]; member manifests inherit via
#    `workspace = true`. Both surfaces are checked so a pin can never drift
#    from the workspace version (which would break `cargo publish` or
#    resolve against a stale crates.io release).
internal_dep_pin_re='codesmith-[a-z-]+[[:space:]]*=[[:space:]]*\{[^}]*version[[:space:]]*=[[:space:]]*"'
internal_dep_drift="$(
  {
    grep -nE "${internal_dep_pin_re}" Cargo.toml || true
    grep -nE "${internal_dep_pin_re}" crates/*/Cargo.toml || true
  } | grep -v "version[[:space:]]*=[[:space:]]*\"${workspace_version}\"" || true
)"
if [[ -n "${internal_dep_drift}" ]]; then
  echo "::error::Internal codesmith-* path dependency versions must match workspace version ${workspace_version}:" >&2
  echo "${internal_dep_drift}" >&2
  fail=1
fi

# 4) Current release-note shape.
current_section="$(
  awk -v version="${workspace_version}" '
    index($0, "## [" version "] - ") == 1 { in_section = 1; print; next }
    in_section && /^## \[/ { exit }
    in_section { print }
  ' CHANGELOG.md
)"
if [[ -z "${current_section}" ]]; then
  echo "::error::CHANGELOG.md must contain a section for ${workspace_version}." >&2
  fail=1
else
  if ! grep -qE "^## \\[${workspace_version}\\] - [0-9]{4}-[0-9]{2}-[0-9]{2}$" <<<"${current_section}"; then
    echo "::error::CHANGELOG.md section ${workspace_version} must use '## [${workspace_version}] - YYYY-MM-DD'." >&2
    fail=1
  fi
  if ! grep -qE "^### (Added|Changed|Deprecated|Removed|Fixed|Security)$" <<<"${current_section}"; then
    echo "::error::CHANGELOG.md section ${workspace_version} must contain at least one Keep a Changelog subsection." >&2
    fail=1
  fi
fi

compare_line="$(grep -E "^\\[${workspace_version}\\]: " CHANGELOG.md || true)"
if [[ -z "${compare_line}" ]]; then
  echo "::error::CHANGELOG.md must include a compare link for ${workspace_version}." >&2
  fail=1
fi

# 6) Contributor-credit cross-check for README additions on the release branch.
# This cannot prove every external PR author has been credited, but it does
# catch the common release-polish failure mode: adding a README contributor row
# without mentioning that credit/correction in the current release entry.
previous_tag=""
current_tag="v${workspace_version}"
if [[ "${compare_line}" =~ compare/(v[0-9]+\.[0-9]+\.[0-9]+)\.\.\.${current_tag} ]]; then
  previous_tag="${BASH_REMATCH[1]}"
fi
if [[ -n "${previous_tag}" ]]; then
  if ! git rev-parse -q --verify "refs/tags/${previous_tag}" >/dev/null; then
    if ! git fetch --quiet --depth=1 origin "refs/tags/${previous_tag}:refs/tags/${previous_tag}"; then
      echo "::error::Failed to fetch tag ${previous_tag} from origin; cannot verify README contributor credits." >&2
      fail=1
    fi
  fi
  if git rev-parse -q --verify "refs/tags/${previous_tag}" >/dev/null; then
    while IFS= read -r line; do
      [[ -z "${line}" ]] && continue
      handle="$(sed -E 's#^\+[-*] \*\*\[[^]]+\]\(https://github\.com/([^)/]+).*#\1#' <<<"${line}")"
      if [[ -n "${handle}" && "${handle}" != "${line}" ]]; then
        if ! grep -Fq "github.com/${handle}" <<<"${current_section}" && ! grep -Fq "@${handle}" <<<"${current_section}"; then
          echo "::error::README.md adds contributor @${handle}, but CHANGELOG.md ${workspace_version} does not mention that credit." >&2
          fail=1
        fi
      fi
    done < <(
      git diff "${previous_tag}..HEAD" -- README.md \
        | grep -E '^\+[-*] \*\*\[[^]]+\]\(https://github.com/[^)]+\)\*\*' || true
    )
  fi
fi

# 7) Security contact guard.
security_contact="https://github.com/camilesing/CodeSmith/security/advisories/new"
if ! grep -qF "${security_contact}" SECURITY.md; then
  echo "::error::SECURITY.md must list ${security_contact} as the security contact channel." >&2
  fail=1
fi
if grep -qE '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' SECURITY.md; then
  echo "::error::SECURITY.md must not use a personal fallback email; use the GitHub security advisory channel." >&2
  fail=1
fi

# 8) Cargo.lock in sync.
if ! metadata_err="$(cargo metadata --locked --format-version 1 --no-deps 2>&1 >/dev/null)"; then
  echo "::error::Cargo.lock is out of sync with the manifests. Run 'cargo update -p codesmith-cli' or 'cargo build' and commit the result." >&2
  echo "${metadata_err}" >&2
  fail=1
fi

if [[ "${fail}" -eq 0 ]]; then
  echo "Version state OK: workspace=${workspace_version}, npm=${npm_version}, lockfile in sync."
fi

exit "${fail}"
