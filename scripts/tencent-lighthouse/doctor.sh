#!/usr/bin/env bash
set -euo pipefail

CODESMITH_USER="${CODESMITH_USER:-codesmith}"
CODESMITH_ROOT="${CODESMITH_ROOT:-/opt/codesmith}"
WORKSPACE_ROOT="${WORKSPACE_ROOT:-/opt/codesmith-workspace}"
RUNTIME_ENV="${RUNTIME_ENV:-/etc/codesmith/runtime.env}"
BRIDGE_ENV="${BRIDGE_ENV:-/etc/codesmith/feishu-bridge.env}"
BRIDGE_DIR="${BRIDGE_DIR:-${CODESMITH_ROOT}/bridge}"
REPO_ROOT="${REPO_ROOT:-${WORKSPACE_ROOT}/codesmith}"

failures=0
warnings=0

section() {
  printf '\n== %s ==\n' "$1"
}

pass() {
  printf '[ok] %s\n' "$1"
}

warn() {
  warnings=$((warnings + 1))
  printf '[warn] %s\n' "$1"
}

fail() {
  failures=$((failures + 1))
  printf '[fail] %s\n' "$1"
}

have_command() {
  command -v "$1" >/dev/null 2>&1
}

env_value() {
  local file="$1"
  local key="$2"
  [[ -f "${file}" ]] || return 0
  grep -E "^[[:space:]]*(export[[:space:]]+)?${key}[[:space:]]*=" "${file}" 2>/dev/null \
    | tr -d '\r' \
    | tail -n 1 \
    | sed -E "s/^[[:space:]]*(export[[:space:]]+)?${key}[[:space:]]*=//; s/[[:space:]]+#.*$//; s/^[[:space:]]+//; s/[[:space:]]+$//; s/^\"([^\"]*)\"$/\1/; s/^'([^']*)'$/\1/" \
    || true
}

is_placeholder() {
  local value
  value="$(printf '%s' "${1:-}" | tr '[:upper:]' '[:lower:]')"
  [[ -z "${value}" || "${value}" == *replace-with* || "${value}" == *xxxxxxxx* || "${value}" == "changeme" ]]
}

file_mode() {
  if stat -c '%a' "$1" >/dev/null 2>&1; then
    stat -c '%a' "$1"
  else
    stat -f '%Lp' "$1"
  fi
}

check_commands() {
  section "Runtime tools"
  for cmd in git curl node npm systemctl ss; do
    if have_command "${cmd}"; then
      pass "${cmd} is installed"
    else
      warn "${cmd} is not on PATH"
    fi
  done
}

check_node() {
  section "Node"
  if ! have_command node; then
    fail "node is required for the Feishu bridge"
    return
  fi
  local major
  major="$(node -p "Number(process.versions.node.split('.')[0])" 2>/dev/null || echo 0)"
  if [[ "${major}" =~ ^[0-9]+$ ]] && (( major >= 18 )); then
    pass "Node.js major version is ${major}"
  else
    fail "Node.js 18+ is required; found ${major}"
  fi
}

check_workspace() {
  section "Workspace"
  [[ -d "${WORKSPACE_ROOT}" ]] && pass "${WORKSPACE_ROOT} exists" || fail "${WORKSPACE_ROOT} is missing"
  [[ -d "${REPO_ROOT}/.git" ]] && pass "${REPO_ROOT} is a git checkout" || fail "${REPO_ROOT} is not a git checkout"
  [[ -d "${WORKSPACE_ROOT}/worktrees" ]] && pass "${WORKSPACE_ROOT}/worktrees exists" || warn "${WORKSPACE_ROOT}/worktrees is missing"
  if [[ -f "${WORKSPACE_ROOT}/AGENTS.md" ]]; then
    pass "${WORKSPACE_ROOT}/AGENTS.md exists"
  else
    warn "${WORKSPACE_ROOT}/AGENTS.md is missing"
  fi
}

check_binaries() {
  section "CodeSmith binaries"
  local cargo_bin="/home/${CODESMITH_USER}/.cargo/bin"
  local codesmith="${cargo_bin}/codesmith"
  if [[ -x "${codesmith}" ]]; then
    pass "${codesmith} is executable"
    "${codesmith}" --version 2>/dev/null | sed 's/^/[info] codesmith version: /' || warn "codesmith --version failed"
  else
    fail "${codesmith} is missing or not executable"
  fi
  if have_command codesmith; then
    pass "codesmith is available on PATH"
  else
    warn "codesmith is not on PATH for interactive shells"
  fi
}

check_env_file() {
  local file="$1"
  local label="$2"
  if [[ ! -f "${file}" ]]; then
    fail "${label} env file is missing: ${file}"
    return
  fi
  pass "${label} env file exists"
  local mode
  mode="$(file_mode "${file}")"
  local world="${mode: -1}"
  if [[ "${world}" =~ ^[0-9]+$ ]] && (( world > 0 )); then
    fail "${label} env file is world-readable (${mode})"
  else
    pass "${label} env file is not world-readable (${mode})"
  fi
}

check_env() {
  section "Environment"
  check_env_file "${RUNTIME_ENV}" "runtime"
  check_env_file "${BRIDGE_ENV}" "bridge"

  local runtime_token bridge_token api_key workspace domain allow_groups allow_unlisted
  runtime_token="$(env_value "${RUNTIME_ENV}" CODESMITH_RUNTIME_TOKEN)"
  bridge_token="$(env_value "${BRIDGE_ENV}" CODESMITH_RUNTIME_TOKEN)"
  api_key="$(env_value "${RUNTIME_ENV}" DEEPSEEK_API_KEY)"
  workspace="$(env_value "${BRIDGE_ENV}" CODESMITH_WORKSPACE)"
  domain="$(env_value "${BRIDGE_ENV}" FEISHU_DOMAIN)"
  allow_groups="$(env_value "${BRIDGE_ENV}" FEISHU_ALLOW_GROUPS)"
  allow_unlisted="$(env_value "${BRIDGE_ENV}" CODESMITH_ALLOW_UNLISTED)"

  if is_placeholder "${runtime_token}"; then
    fail "runtime CODESMITH_RUNTIME_TOKEN is missing or still a placeholder"
  else
    pass "runtime token is set"
  fi
  if is_placeholder "${bridge_token}"; then
    fail "bridge CODESMITH_RUNTIME_TOKEN is missing or still a placeholder"
  else
    pass "bridge token is set"
  fi
  if [[ -n "${runtime_token}" && -n "${bridge_token}" && "${runtime_token}" != "${bridge_token}" ]]; then
    fail "runtime and bridge tokens do not match"
  elif [[ -n "${runtime_token}" && -n "${bridge_token}" ]]; then
    pass "runtime and bridge tokens match"
  fi
  if is_placeholder "${api_key}"; then
    warn "DEEPSEEK_API_KEY is missing or still a placeholder"
  else
    pass "DEEPSEEK_API_KEY is set"
  fi
  [[ "${workspace}" == "${WORKSPACE_ROOT}" || "${workspace}" == "${WORKSPACE_ROOT}/"* ]] \
    && pass "bridge workspace is under ${WORKSPACE_ROOT}" \
    || warn "bridge workspace is outside ${WORKSPACE_ROOT}: ${workspace:-unset}"
  # Explicit allowlist, not a glob: in `[[ ... == pattern ]]` the pattern
  # `https://open.*` matches ANY host starting with "open." (`*` crosses
  # dots and slashes), and FEISHU_DOMAIN decides where the bridge sends
  # app credentials and message payloads — a lax check here green-lights
  # exfiltration to an attacker-controlled host. The bridge resolves only
  # these two official endpoints natively (resolveLarkDomain); anything
  # else is a custom override this doctor refuses to bless.
  case "${domain:-feishu}" in
    feishu|lark)
      pass "FEISHU_DOMAIN is ${domain:-feishu}"
      ;;
    https://open.feishu.cn|https://open.feishu.cn/*|https://open.larksuite.com|https://open.larksuite.com/*)
      pass "FEISHU_DOMAIN is ${domain}"
      ;;
    *)
      fail "FEISHU_DOMAIN must be feishu, lark, or an https://open.feishu.cn / https://open.larksuite.com URL (got: ${domain})"
      ;;
  esac
  [[ "${allow_groups:-false}" == "true" && "${allow_unlisted:-false}" == "true" ]] \
    && fail "group control cannot run with CODESMITH_ALLOW_UNLISTED=true" \
    || pass "group/unlisted mode is not openly combined"
}

check_validator() {
  section "Bridge config validator"
  local validator="${BRIDGE_DIR}/scripts/validate-config.mjs"
  if [[ ! -f "${validator}" ]]; then
    validator="${REPO_ROOT}/integrations/feishu-bridge/scripts/validate-config.mjs"
  fi
  if [[ ! -f "${validator}" ]]; then
    warn "bridge config validator is not installed"
    return
  fi
  local runner=(node)
  if [[ "${EUID}" -eq 0 ]]; then
    if id -u "${CODESMITH_USER}" >/dev/null 2>&1 && have_command sudo; then
      # -n: a misconfigured sudoers (requiretty / password prompt) must
      # report, not hang the doctor run.
      runner=(sudo -n -u "${CODESMITH_USER}" node)
    else
      warn "running as root without sudo; filesystem permission checks may pass trivially (root reads everything)"
    fi
  fi
  if "${runner[@]}" "${validator}" --env "${BRIDGE_ENV}" --runtime-env "${RUNTIME_ENV}" --workspace-root "${WORKSPACE_ROOT}" --check-filesystem; then
    pass "bridge config validator passed"
  else
    fail "bridge config validator reported blocking issues"
  fi
}

check_systemd() {
  section "systemd"
  if ! have_command systemctl || [[ ! -d /run/systemd/system ]]; then
    warn "systemd is not available in this environment"
    return
  fi
  for unit in codesmith-runtime codesmith-feishu-bridge; do
    [[ -f "/etc/systemd/system/${unit}.service" ]] \
      && pass "${unit}.service is installed" \
      || fail "${unit}.service is missing"
    systemctl is-enabled --quiet "${unit}" \
      && pass "${unit} is enabled" \
      || warn "${unit} is not enabled"
    systemctl is-active --quiet "${unit}" \
      && pass "${unit} is active" \
      || fail "${unit} is not active"
  done
}

check_bridge_install() {
  section "Bridge install"
  [[ -f "${BRIDGE_DIR}/package.json" ]] && pass "${BRIDGE_DIR}/package.json exists" || fail "bridge package.json is missing"
  [[ -f "${BRIDGE_DIR}/src/index.mjs" ]] && pass "${BRIDGE_DIR}/src/index.mjs exists" || fail "bridge entrypoint is missing"
  if [[ -d "${BRIDGE_DIR}/node_modules/@larksuiteoapi/node-sdk" ]]; then
    pass "Lark SDK dependency is installed"
  else
    warn "Lark SDK dependency is not installed under ${BRIDGE_DIR}/node_modules"
  fi
}

check_localhost_health() {
  section "Localhost health"
  local port token
  port="$(env_value "${RUNTIME_ENV}" CODESMITH_RUNTIME_PORT)"
  port="${port:-7878}"
  token="$(env_value "${BRIDGE_ENV}" CODESMITH_RUNTIME_TOKEN)"

  if have_command ss; then
    local listeners
    # Anchor the port match (`:PORT$`): 1-3 digit ports would otherwise
    # substring-match longer ports (878 matches 8780-8789).
    listeners="$(ss -ltn 2>/dev/null | awk -v port=":${port}$" '$4 ~ port {print $4}' || true)"
    if [[ -z "${listeners}" ]]; then
      fail "runtime port ${port} is not listening"
    elif grep -qvE "^(127\\.0\\.0\\.1|\\[::1\\]):${port}$" <<<"${listeners}"; then
      # ANY non-local listener on the port fails the check — a dual bind
      # (127.0.0.1 AND 0.0.0.0) still exposes the service on all
      # interfaces; matching one localhost line is not enough.
      fail "runtime port ${port} is listening on a non-local address: ${listeners//$'\n'/, }"
    else
      pass "runtime port ${port} is bound to localhost only"
    fi
  else
    warn "ss is unavailable; skipping bind-address check"
  fi

  if ! have_command curl; then
    warn "curl is unavailable; skipping HTTP checks"
    return
  fi

  if curl -fsS --max-time 3 "http://127.0.0.1:${port}/health" >/dev/null; then
    pass "/health responds on localhost"
  else
    fail "/health did not respond on localhost:${port}"
  fi

  if is_placeholder "${token}"; then
    warn "runtime token is not usable; skipping /v1/runtime/info auth check"
    return
  fi

  # Feed the token via curl's --config stdin: printf is a builtin, so the
  # bearer never appears in any process's argv (cf. check_env_file, which
  # rejects world-readable env files for the same reason).
  if curl -fsS --max-time 3 \
    --config <(printf 'header = "Authorization: Bearer %s"\n' "${token}") \
    "http://127.0.0.1:${port}/v1/runtime/info" \
    | node -e '
      let raw = "";
      process.stdin.on("data", (c) => { raw += c; });
      process.stdin.on("end", () => {
        const data = JSON.parse(raw);
        if (data.bind_host !== "127.0.0.1") process.exit(2);
        if (data.auth_required !== true) process.exit(3);
      });
    '; then
    pass "/v1/runtime/info reports localhost bind and auth_required=true"
  else
    fail "/v1/runtime/info did not respond with bearer auth or report a localhost bind with auth enabled"
  fi
}

main() {
  printf 'Tencent Lighthouse DeepSeek doctor\n'
  check_commands
  check_node
  check_workspace
  check_binaries
  check_env
  check_bridge_install
  check_validator
  check_systemd
  check_localhost_health

  section "Summary"
  printf '%s failure(s), %s warning(s)\n' "${failures}" "${warnings}"
  (( failures == 0 ))
}

main "$@"
