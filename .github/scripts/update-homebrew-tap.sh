#!/usr/bin/env bash
# Update the Homebrew tap at camilesing/homebrew-codesmith after a release.
#
# Expected environment:
#   TAG       – git tag, e.g. "v0.8.31"
#   MANIFEST  – path to codesmith-artifacts-sha256.txt
#   TAP_REPO  – owner/repo of the Homebrew tap
#   TOKEN     – PAT with contents:write on TAP_REPO (required)

set -euo pipefail

: "${TAG:?}"
: "${MANIFEST:?}"
: "${TAP_REPO:?}"

if [ -z "${TOKEN:-}" ]; then
  echo "::error::No Homebrew tap token configured (TOKEN); refusing to update the tap." >&2
  exit 1
fi

# TAG is interpolated into the release URL, the Ruby formula body (which
# Homebrew evaluates on every end-user machine), and the commit message —
# refuse anything outside a plain vX.Y.Z[-suffix] shape.
[[ "${TAG}" =~ ^v[0-9]+(\.[0-9]+)+(-[0-9A-Za-z.]+)?$ ]] || die "Refusing unvalidated TAG: ${TAG}"
VERSION="${TAG#v}"

die() { echo "::error::${1}" >&2; exit 1; }

sha() {
  local file="${1:?}"
  local val
  val="$(awk -v f="${file}" '$2 == f {print $1; exit}' "${MANIFEST}")"
  if [ -z "${val}" ]; then
    die "Missing binary in checksum manifest: ${file}"
  fi
  echo "${val}"
}

# --- read checksums ---------------------------------------------------

# The single codesmith binary.
# Plain assignments first: `readonly VAR="$(…)"` masks the subshell's exit
# status (the builtin itself returns 0), so a missing manifest entry would
# not abort and an empty sha256 would get pushed to the tap.
SHA_COD_MACOS_ARM="$(sha codesmith-macos-arm64)"
SHA_COD_MACOS_X64="$(sha codesmith-macos-x64)"
SHA_COD_LINUX_ARM="$(sha codesmith-linux-arm64)"
SHA_COD_LINUX_X64="$(sha codesmith-linux-x64)"
readonly SHA_COD_MACOS_ARM SHA_COD_MACOS_X64 SHA_COD_LINUX_ARM SHA_COD_LINUX_X64

# --- temp dirs --------------------------------------------------------

FORMULA_FILE="$(mktemp)"
TAP_DIR="$(mktemp -d)"
trap 'rm -rf "${TAP_DIR}" "${FORMULA_FILE}"' EXIT

# --- generate formula --------------------------------------------------

readonly BASE_URL="https://github.com/camilesing/CodeSmith/releases/download/${TAG}"

cat > "${FORMULA_FILE}" << EOF
class Codesmith < Formula
  desc "Terminal-native coding agent for DeepSeek V4"
  homepage "https://github.com/camilesing/CodeSmith"
  version "${VERSION}"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "${BASE_URL}/codesmith-macos-arm64", using: :nounzip
      sha256 "${SHA_COD_MACOS_ARM}"
    else
      url "${BASE_URL}/codesmith-macos-x64", using: :nounzip
      sha256 "${SHA_COD_MACOS_X64}"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "${BASE_URL}/codesmith-linux-arm64", using: :nounzip
      sha256 "${SHA_COD_LINUX_ARM}"
    else
      url "${BASE_URL}/codesmith-linux-x64", using: :nounzip
      sha256 "${SHA_COD_LINUX_X64}"
    end
  end

  def install
    artifacts = Dir["codesmith-*"]
    raise "expected exactly one codesmith-* artifact in the staging directory, found: #{artifacts}" if artifacts.length != 1
    bin.install artifacts.first => "codesmith"
  end

  test do
    system "#{bin}/codesmith", "--version"
  end
end
EOF

# --- push to tap repo --------------------------------------------------
# Auth goes through an HTTP extra header instead of the URL so a git failure
# never echoes the PAT. The header is scoped to github.com only, and it is
# passed via git's environment config (GIT_CONFIG_*) rather than a `-c`
# argument: argv is visible to other local users via `ps` for the lifetime
# of the clone/push, and base64 is trivially reversible.
AUTH_HEADER="AUTHORIZATION: basic $(printf 'x-access-token:%s' "${TOKEN}" | base64)"
export GIT_CONFIG_COUNT=1
export GIT_CONFIG_KEY_0="http.https://github.com/.extraheader"
export GIT_CONFIG_VALUE_0="${AUTH_HEADER}"

git clone --depth 1 "https://github.com/${TAP_REPO}.git" "${TAP_DIR}"

mkdir -p "${TAP_DIR}/Formula"
cp "${FORMULA_FILE}" "${TAP_DIR}/Formula/codesmith.rb"
# A malformed heredoc (unbalanced blocks, broken interpolation) must fail the
# workflow here, before anything is committed or pushed to the tap.
ruby -c "${TAP_DIR}/Formula/codesmith.rb" >/dev/null || die "Generated formula is not valid Ruby"

cd "${TAP_DIR}"
git config user.name  "github-actions[bot]"
git config user.email "github-actions[bot]@users.noreply.github.com"

git add -A Formula/

if git diff --cached --quiet; then
  echo "Formula unchanged (already at ${VERSION}); nothing to push."
  exit 0
fi

git commit -m "chore: bump formula to ${VERSION}

Automated update from the release workflow."

TAP_BRANCH="$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's/^origin\///')"
TAP_BRANCH="${TAP_BRANCH:-main}"

if ! git push origin "HEAD:${TAP_BRANCH}"; then
  # A commit landed on the tap between the clone and this push: fetch the
  # moved branch tip and retry once on top of it instead of aborting the
  # release on a non-fast-forward.
  echo "::warning::push to ${TAP_BRANCH} was non-fast-forward; rebasing and retrying once"
  git fetch --depth 1 origin "${TAP_BRANCH}"
  git rebase "origin/${TAP_BRANCH}"
  git push origin "HEAD:${TAP_BRANCH}" || die "push to ${TAP_REPO}:${TAP_BRANCH} failed after rebase retry"
fi
echo "Pushed formula update to ${TAP_REPO} (v${VERSION})"
