#!/usr/bin/env bash
set -euo pipefail

expected_version="${1:-}"
# Callers pass tags, not bare versions ('v1.2.3'); strip the optional 'v'
# so the comparison below is not a guaranteed false mismatch.
expected_version="${expected_version#v}"
if [[ -z "${expected_version}" && "${GITHUB_REF:-}" == refs/tags/v* ]]; then
  expected_version="${GITHUB_REF#refs/tags/v}"
fi

if [[ -z "${expected_version}" && -n "${GITHUB_REF:-}" && "${GITHUB_REF}" != refs/tags/v* ]]; then
  # An explicit error beats the generic usage message: the ref, not the
  # invocation, is why no version could be derived.
  echo "GITHUB_REF '${GITHUB_REF}' is not a 'v'-prefixed tag; cannot determine version" >&2
  exit 1
fi

if [[ -z "${expected_version}" ]]; then
  echo "usage: $0 <version>" >&2
  exit 1
fi

python3 - "${expected_version}" <<'PY'
import json
import subprocess
import sys

expected = sys.argv[1]
try:
    metadata = json.loads(
        subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps"])
    )
except (OSError, subprocess.CalledProcessError) as exc:
    print(f"cargo metadata failed: {exc}", file=sys.stderr)
    sys.exit(1)
workspace_members = set(metadata["workspace_members"])
packages = [pkg for pkg in metadata["packages"] if pkg["id"] in workspace_members]
mismatches = [
    f"{pkg['name']}={pkg['version']}" for pkg in packages if pkg["version"] != expected
]

if not packages:
  print("No workspace packages found; is this run from the Cargo workspace root?", file=sys.stderr)
  sys.exit(1)

if mismatches:
    print(f"Tag version {expected} does not match all workspace crates:", file=sys.stderr)
    for item in mismatches:
        print(f"  - {item}", file=sys.stderr)
    sys.exit(1)

print(f"Verified {len(packages)} workspace packages at version {expected}")
PY
