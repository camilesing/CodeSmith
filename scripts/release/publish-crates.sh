#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/release/crates.sh
source "${script_dir}/crates.sh"

mode="${1:-dry-run}"
case "${mode}" in
  dry-run|publish) ;;
  *)
    echo "usage: $0 [dry-run|publish]" >&2
    exit 1
    ;;
esac

packages=("${release_crates[@]}")

workspace_version=""
workspace_codesmith_packages=()
workspace_package_dep_flags=()
# Workspace dependency edges as parallel arrays (from, to): associative
# arrays are bash-4+, and this script must also run on macOS's bash 3.2.
dep_from=()
dep_to=()

while IFS=$'\t' read -r kind name value; do
  case "${kind}" in
    version)
      workspace_version="${name}"
      ;;
    crate)
      workspace_codesmith_packages+=("${name}")
      workspace_package_dep_flags+=("${value}")
      ;;
    dep)
      dep_from+=("${name}")
      dep_to+=("${value}")
      ;;
  esac
done < <(
  python3 - <<'PY'
import json
import subprocess

try:
    metadata = json.loads(
        subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps"])
    )
except (OSError, subprocess.CalledProcessError) as exc:
    raise SystemExit(f"cargo metadata failed: {exc}")
workspace_members = set(metadata["workspace_members"])
workspace_packages = [
    pkg for pkg in metadata["packages"] if pkg["id"] in workspace_members
]
workspace_by_name = {pkg["name"]: pkg for pkg in workspace_packages}

versions = sorted({pkg["version"] for pkg in workspace_packages})
if not versions:
    raise SystemExit("workspace has no packages")
if len(versions) != 1:
    raise SystemExit(f"workspace packages have mixed versions: {', '.join(versions)}")
print(f"version\t{versions[0]}\t")

for pkg in sorted(workspace_packages, key=lambda item: item["name"]):
    if not pkg["name"].startswith("codesmith-"):
        continue
    has_workspace_dep = any(
        dep.get("path") and dep["name"] in workspace_by_name
        for dep in pkg["dependencies"]
    )
    print(f"crate\t{pkg['name']}\t{1 if has_workspace_dep else 0}")
    for dep in pkg["dependencies"]:
        # Dev-dependencies don't need to exist on the registry at publish
        # time; only normal and build dependencies constrain the order.
        if dep.get("path") and dep.get("kind") != "dev" and dep["name"] in workspace_by_name:
            print(f"dep\t{pkg['name']}\t{dep['name']}")
PY
)

if [[ -z "${workspace_version}" ]]; then
  echo "Could not determine workspace version." >&2
  exit 1
fi

missing_packages=()
for workspace_package in "${workspace_codesmith_packages[@]}"; do
  found=0
  for package in "${packages[@]}"; do
    if [[ "${package}" == "${workspace_package}" ]]; then
      found=1
      break
    fi
  done
  if [[ "${found}" == "0" ]]; then
    missing_packages+=("${workspace_package}")
  fi
done

extra_packages=()
for package in "${packages[@]}"; do
  found=0
  for workspace_package in "${workspace_codesmith_packages[@]}"; do
    if [[ "${package}" == "${workspace_package}" ]]; then
      found=1
      break
    fi
  done
  if [[ "${found}" == "0" ]]; then
    extra_packages+=("${package}")
  fi
done

if (( ${#missing_packages[@]} > 0 || ${#extra_packages[@]} > 0 )); then
  if (( ${#missing_packages[@]} > 0 )); then
    echo "publish package list is missing workspace crates: ${missing_packages[*]}" >&2
  fi
  if (( ${#extra_packages[@]} > 0 )); then
    echo "publish package list contains non-workspace crates: ${extra_packages[*]}" >&2
  fi
  exit 1
fi

# Publish order must actually follow the dependency graph, not just by
# convention: a dependent listed before its workspace dependency fails
# `cargo publish` (the dependency is not yet visible on crates.io) and, under
# `set -e`, aborts the run mid-way leaving a partial release.
package_index() {
  local wanted="$1" index
  for ((index = 0; index < ${#packages[@]}; index += 1)); do
    if [[ "${packages[$index]}" == "${wanted}" ]]; then
      printf '%s' "${index}"
      return 0
    fi
  done
  return 1
}
for ((edge = 0; edge < ${#dep_from[@]}; edge += 1)); do
  from_index="$(package_index "${dep_from[$edge]}")" || continue
  if ! to_index="$(package_index "${dep_to[$edge]}")"; then
    echo "release_crates order: ${dep_from[$edge]} depends on ${dep_to[$edge]}, which is not in the list." >&2
    exit 1
  fi
  if (( to_index > from_index )); then
    echo "release_crates order: ${dep_from[$edge]} depends on ${dep_to[$edge]}, which is listed later; publish dependencies first." >&2
    exit 1
  fi
done

package_has_workspace_deps() {
  local package_name="$1"
  local index
  for ((index = 0; index < ${#workspace_codesmith_packages[@]}; index += 1)); do
    if [[ "${workspace_codesmith_packages[$index]}" == "${package_name}" ]]; then
      [[ "${workspace_package_dep_flags[$index]}" == "1" ]]
      return
    fi
  done

  echo "Unknown workspace crate: ${package_name}" >&2
  return 1
}

crate_version_exists() {
  local crate_name="$1"
  local crate_version="$2"
  case "$(crate_version_status "${crate_name}" "${crate_version}")" in
    published) return 0 ;;
    absent) return 1 ;;
    *)
      # Unknown means the registry could not be queried (network/5xx). Treat
      # it as fatal: guessing "absent" here is what triggers a duplicate
      # `cargo publish` attempt.
      echo "Could not query crates.io for ${crate_name} ${crate_version} (network or server error); refusing to guess published/absent." >&2
      exit 1
      ;;
  esac
}

wait_for_crate_version() {
  local crate_name="$1"
  local crate_version="$2"
  local attempts=30

  for ((attempt = 1; attempt <= attempts; attempt += 1)); do
    case "$(crate_version_status "${crate_name}" "${crate_version}")" in
      published) return 0 ;;
      # "unknown" (transient network/5xx) keeps retrying inside the wait;
      # only a confirmed 404 counts as still-absent.
    esac
    echo "Waiting for ${crate_name} ${crate_version} to appear on crates.io (${attempt}/${attempts})..."
    sleep 10
  done

  echo "Timed out waiting for ${crate_name} ${crate_version} to appear on crates.io" >&2
  return 1
}

for package in "${packages[@]}"; do
  echo "::group::${mode} ${package}"
  if [[ "${mode}" == "dry-run" ]]; then
    if package_has_workspace_deps "${package}"; then
      # `--list` only prints the file list: no tarball is built, so
      # packaging failures (missing README/include, accidentally bundled
      # files) surface only during the real publish. `--no-verify` still
      # produces and validates the actual .crate without building it and
      # without needing workspace dependencies on the registry.
      cargo package --no-verify --allow-dirty --locked -p "${package}" >/dev/null
      echo "Packaged ${package} (tarball validated, build verification skipped); full crates.io dry-run requires workspace dependencies at ${workspace_version} to be published first."
    else
      cargo publish --dry-run --locked --allow-dirty -p "${package}"
    fi
  else
    if crate_version_exists "${package}" "${workspace_version}"; then
      echo "Skipping ${package} ${workspace_version}; already published."
    else
      cargo publish --locked -p "${package}"
      wait_for_crate_version "${package}" "${workspace_version}"
    fi
  fi
  echo "::endgroup::"
done
