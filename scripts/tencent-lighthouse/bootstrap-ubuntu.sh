#!/usr/bin/env bash
set -euo pipefail

if [[ "${EUID}" -ne 0 ]]; then
  echo "Run as root: sudo bash scripts/tencent-lighthouse/bootstrap-ubuntu.sh" >&2
  exit 1
fi

CODESMITH_USER="${CODESMITH_USER:-codesmith}"
CODESMITH_ROOT="${CODESMITH_ROOT:-/opt/codesmith}"
WHALEBRO_ROOT="${WHALEBRO_ROOT:-/opt/whalebro}"
REPO_URL="${CODESMITH_REPO_URL:-https://github.com/Hmbown/CodeSmith.git}"
WHALEBRO_EXTRA_REPOS="${WHALEBRO_EXTRA_REPOS:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SOURCE_BRANCH="$(git -C "${SOURCE_ROOT}" branch --show-current 2>/dev/null || true)"
REPO_BRANCH="${CODESMITH_REPO_BRANCH:-${SOURCE_BRANCH:-main}}"

apt-get update
apt-get install -y \
  ca-certificates \
  curl \
  git \
  iproute2 \
  openssh-client \
  build-essential \
  pkg-config \
  libssl-dev \
  nodejs \
  npm \
  rsync \
  tmux \
  fail2ban \
  ufw

node_major="$(node -p "Number(process.versions.node.split('.')[0])")"
if (( node_major < 18 )); then
  echo "Node.js 18+ is required for the Feishu bridge; install a newer Node.js before running install-services.sh." >&2
fi

if ! id -u "${CODESMITH_USER}" >/dev/null 2>&1; then
  useradd --create-home --shell /bin/bash "${CODESMITH_USER}"
fi

install -d -o "${CODESMITH_USER}" -g "${CODESMITH_USER}" "${CODESMITH_ROOT}"
install -d -o "${CODESMITH_USER}" -g "${CODESMITH_USER}" "${CODESMITH_ROOT}/bridge"
install -d -o "${CODESMITH_USER}" -g "${CODESMITH_USER}" "${WHALEBRO_ROOT}"
install -d -o "${CODESMITH_USER}" -g "${CODESMITH_USER}" "${WHALEBRO_ROOT}/worktrees"
install -d -m 0750 -o root -g "${CODESMITH_USER}" /etc/codesmith
install -d -m 0700 -o "${CODESMITH_USER}" -g "${CODESMITH_USER}" /var/lib/codesmith-feishu-bridge

if [[ ! -d "${WHALEBRO_ROOT}/codesmith/.git" ]]; then
  sudo -u "${CODESMITH_USER}" git clone --branch "${REPO_BRANCH}" "${REPO_URL}" "${WHALEBRO_ROOT}/codesmith"
fi

for repo_spec in ${WHALEBRO_EXTRA_REPOS}; do
  repo_name="${repo_spec%%=*}"
  repo_url="${repo_spec#*=}"
  if [[ -z "${repo_name}" || -z "${repo_url}" || "${repo_name}" == "${repo_url}" ]]; then
    echo "Skipping malformed WHALEBRO_EXTRA_REPOS entry: ${repo_spec}" >&2
    continue
  fi
  if [[ ! -d "${WHALEBRO_ROOT}/${repo_name}/.git" ]]; then
    sudo -u "${CODESMITH_USER}" git clone "${repo_url}" "${WHALEBRO_ROOT}/${repo_name}" || {
      echo "Warning: failed to clone optional repo ${repo_name} from ${repo_url}" >&2
    }
  fi
done

if [[ ! -f /etc/codesmith/runtime.env ]]; then
  cat >/etc/codesmith/runtime.env <<'EOF'
CODESMITH_RUNTIME_TOKEN=replace-with-long-random-token
CODESMITH_RUNTIME_PORT=7878
CODESMITH_RUNTIME_WORKERS=2
DEEPSEEK_API_KEY=replace-with-deepseek-platform-key
RUST_LOG=info
EOF
  chown root:"${CODESMITH_USER}" /etc/codesmith/runtime.env
  chmod 0640 /etc/codesmith/runtime.env
fi

if [[ ! -f /etc/codesmith/feishu-bridge.env ]]; then
  cat >/etc/codesmith/feishu-bridge.env <<'EOF'
FEISHU_APP_ID=cli_xxxxxxxxxxxxxxxx
FEISHU_APP_SECRET=replace-with-app-secret
FEISHU_DOMAIN=feishu
DEEPSEEK_RUNTIME_URL=http://127.0.0.1:7878
CODESMITH_RUNTIME_TOKEN=replace-with-same-token-as-runtime-env
DEEPSEEK_WORKSPACE=/opt/whalebro
DEEPSEEK_MODEL=auto
DEEPSEEK_MODE=agent
DEEPSEEK_ALLOW_SHELL=true
DEEPSEEK_TRUST_MODE=false
DEEPSEEK_AUTO_APPROVE=false
DEEPSEEK_CHAT_ALLOWLIST=
DEEPSEEK_ALLOW_UNLISTED=false
FEISHU_THREAD_MAP_PATH=/var/lib/codesmith-feishu-bridge/thread-map.json
FEISHU_ALLOW_GROUPS=false
FEISHU_REQUIRE_PREFIX_IN_GROUP=true
FEISHU_GROUP_PREFIX=/ds
FEISHU_MAX_REPLY_CHARS=3500
DEEPSEEK_TURN_TIMEOUT_MS=900000
EOF
  chown root:"${CODESMITH_USER}" /etc/codesmith/feishu-bridge.env
  chmod 0640 /etc/codesmith/feishu-bridge.env
fi

ufw allow OpenSSH
ufw --force enable

cat <<EOF

Base server setup complete.

Next:
1. Install Rust 1.88+ for ${CODESMITH_USER}; rustup is the usual path.
2. Build/install both binaries:
   sudo -iu ${CODESMITH_USER}
   cd ${WHALEBRO_ROOT}/codesmith
   cargo install --path crates/cli --locked --force
   cargo install --path crates/tui --locked --force
3. Copy integrations/feishu-bridge to ${CODESMITH_ROOT}/bridge and run npm install.
4. Edit /etc/codesmith/runtime.env and /etc/codesmith/feishu-bridge.env.
5. Install systemd units with scripts/tencent-lighthouse/install-services.sh.
6. After the env files are edited and services are started, run:
   sudo bash scripts/tencent-lighthouse/doctor.sh

EOF
