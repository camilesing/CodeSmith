const fs = require("fs");
const { execFileSync } = require("child_process");

const GLIBC_VERSION_SOURCE_RE = /GLIBC_(\d+)\.(\d+)(?:\.(\d+))?/g;

function isLinux() {
  return process.platform === "linux";
}

function parseVersion(text) {
  const match = String(text || "").match(/(\d+)\.(\d+)(?:\.(\d+))?/);
  if (!match) return null;
  return [Number(match[1]), Number(match[2]), Number(match[3] || 0)];
}

function compareVersion(a, b) {
  for (let i = 0; i < 3; i += 1) {
    if (a[i] !== b[i]) return a[i] - b[i];
  }
  return 0;
}

function formatVersion(version) {
  return version[2] ? `${version[0]}.${version[1]}.${version[2]}` : `${version[0]}.${version[1]}`;
}

function detectHostGlibc() {
  try {
    const out = execFileSync("getconf", ["GNU_LIBC_VERSION"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    const version = parseVersion(out);
    if (version) return version;
  } catch {
    // fall through to ldd
  }
  try {
    const out = execFileSync("ldd", ["--version"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    const firstLine = out.split("\n", 1)[0];
    // Guard against non-glibc loaders (musl, uClibc, busybox variants)
    // whose --version output contains an unrelated version number —
    // parsing it reported a bogus "glibc 1.2.5 too old" (or silently
    // passed a high unrelated version) instead of the intended
    // "no GNU libc detected" guidance.
    if (!/glibc|GNU libc/i.test(firstLine)) return null;
    const version = parseVersion(firstLine);
    if (version) return version;
  } catch {
    // glibc not present (e.g. musl / Alpine)
  }
  return null;
}

function highestGlibcIn(text) {
  let highest = null;
  for (const match of text.matchAll(GLIBC_VERSION_SOURCE_RE)) {
    const version = [Number(match[1]), Number(match[2]), Number(match[3] || 0)];
    if (!highest || compareVersion(version, highest) > 0) {
      highest = version;
    }
  }
  return highest;
}

function detectBinaryRequiredGlibc(filePath) {
  let buf;
  try {
    buf = fs.readFileSync(filePath);
  } catch (err) {
    // The preflight exists to give install-time guidance; a raw ENOENT /
    // EACCES defeats that purpose.
    throw new Error(
      `Unable to read the prebuilt binary at ${filePath} (${err.message}). ` +
        "Reinstall the package or build from source with Cargo.",
    );
  }
  // Authoritative source first: readelf -V reports only the dynamic version
  // metadata (version needs / symbol versions). The raw string scan below
  // over-reports — GLIBC_ matches inside debug sections or other non-symbol
  // data inflate the requirement and can fail hosts that would run the
  // binary fine. matchAll on a local copy of the regex, so no shared
  // lastIndex state survives an early throw.
  try {
    const out = execFileSync("readelf", ["-V", filePath], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    return highestGlibcIn(out);
  } catch {
    // readelf unavailable — fall back to the raw scan.
  }
  return highestGlibcIn(buf.toString("latin1"));
}

function buildFromSourceHint() {
  return [
    "You can still run codesmith by building from source with Cargo:",
    "",
    "  # Requires Rust 1.88+ (https://rustup.rs)",
    "  cargo install codesmith --locked",
    "",
    "Or build from a checkout:",
    "",
    "  git clone https://github.com/camilesing/CodeSmith.git",
    "  cd CodeSmith",
    "  cargo install --path crates/cli --locked",
    "",
    "See https://github.com/camilesing/CodeSmith/blob/main/docs/INSTALL.md",
  ].join("\n");
}

function preflightGlibc(filePath) {
  if (!isLinux()) return;
  if (
    process.env.CODESMITH_SKIP_GLIBC_CHECK === "1"
  ) {
    return;
  }

  const required = detectBinaryRequiredGlibc(filePath);
  if (!required) {
    // Statically linked / musl binary, or no GLIBC_* version dependencies present.
    return;
  }

  const host = detectHostGlibc();
  if (!host) {
    throw new Error(
      [
        `The prebuilt binary requires GLIBC_${formatVersion(required)}, but no GNU libc was detected on this host.`,
        "This usually means you're on a musl-based distro such as Alpine.",
        "",
        buildFromSourceHint(),
        "",
        "Set CODESMITH_SKIP_GLIBC_CHECK=1 to bypass this check at your own risk.",
      ].join("\n"),
    );
  }

  if (compareVersion(host, required) < 0) {
    throw new Error(
      [
        `Prebuilt CodeSmith binary requires GLIBC_${formatVersion(required)} but this system has glibc ${formatVersion(host)}.`,
        "Older distros (CentOS 7/8, RHEL 7/8, Debian 10, etc.) ship an older glibc that is not compatible with the prebuilt artifact.",
        "",
        buildFromSourceHint(),
        "",
        "Set CODESMITH_SKIP_GLIBC_CHECK=1 to bypass this check at your own risk.",
      ].join("\n"),
    );
  }
}

module.exports = {
  preflightGlibc,
  detectHostGlibc,
  detectBinaryRequiredGlibc,
  // exported for tests
  _internal: { parseVersion, compareVersion, formatVersion },
};
