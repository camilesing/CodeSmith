const path = require("path");
const os = require("os");

const CHECKSUM_MANIFEST = "codesmith-artifacts-sha256.txt";

const DEFAULT_REPO = "camilesing/CodeSmith";

// `binary` is the installable executable for the platform/arch; `extras`
// ships alongside it as a release asset but is never installed as the
// binary (e.g. the Windows launcher .bat).
const ASSET_MATRIX = {
  linux: {
    x64: { binary: "codesmith-linux-x64", extras: [] },
    arm64: { binary: "codesmith-linux-arm64", extras: [] },
    riscv64: { binary: "codesmith-linux-riscv64", extras: [] },
  },
  darwin: {
    x64: { binary: "codesmith-macos-x64", extras: [] },
    arm64: { binary: "codesmith-macos-arm64", extras: [] },
  },
  win32: {
    x64: { binary: "codesmith-windows-x64.exe", extras: ["codesmith.bat"] },
  },
};

// HarmonyPC (openharmony) is an x86_64 Linux-compatible environment; map it to
// the linux binary family so npm install succeeds without a separate build target.
const PLATFORM_ALIASES = {
  openharmony: "linux",
};

function detectBinaryNames() {
  const rawPlatform = os.platform();
  const platform = PLATFORM_ALIASES[rawPlatform] || rawPlatform;
  const arch = os.arch();
  const defaults = ASSET_MATRIX[platform];
  if (!defaults) {
    const supported = Object.keys(ASSET_MATRIX).map(p => `'${p}'`).join(', ');
    throw new Error(
      `Unsupported platform: ${rawPlatform}. Supported platforms: ${supported}.\n\n` +
      unsupportedBuildHint(),
    );
  }
  const pair = defaults[arch];
  if (!pair) {
    const supported = Object.keys(defaults).map(a => `'${a}'`).join(', ');
    throw new Error(
      `Unsupported architecture: ${arch} on platform ${platform}. ` +
      `Supported architectures: ${supported}.\n\n` +
      unsupportedBuildHint(),
    );
  }
  return {
    platform,
    arch,
    codesmith: pair.binary,
  };
}

function unsupportedBuildHint() {
  return [
    "No prebuilt binary is available for this platform/architecture combo.",
    "You can still run codesmith by building from source with Cargo:",
    "",
    "  # Requires Rust 1.88+ (https://rustup.rs)",
    "  cargo install codesmith-cli --locked   # provides `codesmith`",
    "",
    "Or build from a checkout:",
    "",
    "  git clone https://github.com/camilesing/CodeSmith.git",
    "  cd CodeSmith",
    "  cargo install --path crates/cli --locked",
    "",
    "See https://github.com/camilesing/CodeSmith/blob/main/docs/INSTALL.md",
    "for cross-compilation, mirror, and Linux ARM64 specifics.",
  ].join("\n");
}

function isCnbMirrorEnabled() {
  // Fail closed: only an explicit allowlist of enabling spellings routes to
  // the mirror — opt-outs like "no"/"off"/"disable" and any unrecognized
  // value must not silently enable it (the mirror hosts linux-x64 only, so
  // a wrongly-enabled mirror breaks macOS/Windows installs entirely).
  const flag = String(process.env.CODESMITH_USE_CNB_MIRROR || "")
    .trim()
    .toLowerCase();
  return ["1", "true", "yes", "on"].includes(flag);
}

// The CNB mirror only builds and hosts the linux-x64 binary (and its own
// checksum manifest). Every other asset must fall back to GitHub even when
// the mirror is enabled, or installs on macOS/Windows point at 404s.
const CNB_MIRROR_ASSETS = new Set(["codesmith-linux-x64"]);

// `version` and `repo` come from documented user-facing env overrides
// (CODESMITH_VERSION, CODESMITH_GITHUB_REPO) and are interpolated into URL
// templates, so they are validated here — at the configuration site — rather
// than surfacing later as an opaque 404 from the installer's generic
// download-failure error.
function normalizeVersion(version) {
  const normalized = String(version).trim().replace(/^v/, "");
  if (!/^\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?$/.test(normalized)) {
    throw new Error(
      `Invalid release version: ${JSON.stringify(version)} ` +
        `(expected e.g. 0.8.19 or v0.8.19).`,
    );
  }
  return normalized;
}

function normalizeRepo(repo) {
  const normalized = String(repo).trim().replace(/\.git$/, "");
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(normalized)) {
    throw new Error(
      `Invalid repo: ${JSON.stringify(repo)} (expected owner/name).`,
    );
  }
  return normalized;
}

function resolveBaseUrl(version, repo, useMirror) {
  version = normalizeVersion(version);
  repo = normalizeRepo(repo);
  // CODESMITH_RELEASE_BASE_URL is the canonical override.
  const override = String(process.env.CODESMITH_RELEASE_BASE_URL || "").trim();
  if (override) {
    const base = override.endsWith("/") ? override : `${override}/`;
    let parsed;
    try {
      parsed = new URL(base);
    } catch {
      throw new Error(
        `CODESMITH_RELEASE_BASE_URL must be a valid absolute URL (got: ${override}).`,
      );
    }
    // The checksum manifest is fetched from this same base, so an insecure
    // scheme on a non-loopback host would let a network attacker serve both
    // the binary and its manifest, silently defeating checksum verification.
    const host = parsed.hostname;
    const isLoopback =
      host === "localhost" || host === "127.0.0.1" || host === "::1" || host === "[::1]";
    if (parsed.protocol !== "https:" && !isLoopback) {
      throw new Error(
        `CODESMITH_RELEASE_BASE_URL must use https: (got: ${parsed.protocol}//${host}). ` +
          `The checksum manifest is downloaded from the same base, so an insecure ` +
          `scheme would defeat checksum verification.`,
      );
    }
    if (parsed.protocol === "http:") {
      console.warn(
        `warning: CODESMITH_RELEASE_BASE_URL uses plaintext http on ${host}; only acceptable for local testing.`,
      );
    }
    return base;
  }
  if (useMirror && isCnbMirrorEnabled()) {
    // The CNB pipeline (`.cnb.yml` "v*" tag job) publishes only the
    // linux-x64 asset family plus the checksum manifest: every other
    // platform/arch must stay on GitHub or the install 404s with no
    // fallback.
    const platform = process.env.npm_config_platform || process.platform;
    const arch = process.env.npm_config_arch || process.arch;
    if (platform === "linux" && arch === "x64") {
      return `https://cnb.cool/${repo}/-/releases/v${version}/`;
    }
  }
  return `https://github.com/${repo}/releases/download/v${version}/`;
}

function releaseBaseUrl(version, repo = DEFAULT_REPO) {
  return resolveBaseUrl(version, repo, false);
}

function releaseAssetUrl(baseName, version, repo = DEFAULT_REPO) {
  return new URL(baseName, resolveBaseUrl(version, repo, CNB_MIRROR_ASSETS.has(baseName))).toString();
}

// The manifest must come from the same source as the binary it verifies:
// `forAsset` names the binary being installed, so a mirrored linux-x64
// download is checked against the mirror's own manifest (the two builds'
// bytes differ).
function checksumManifestUrl(version, repo = DEFAULT_REPO, forAsset = null) {
  const useMirror = forAsset !== null && CNB_MIRROR_ASSETS.has(forAsset);
  return new URL(CHECKSUM_MANIFEST, resolveBaseUrl(version, repo, useMirror)).toString();
}

function releaseBinaryDirectory() {
  return path.join(__dirname, "..", "bin", "downloads");
}

function allAssetNames() {
  const names = [];
  for (const platformAssets of Object.values(ASSET_MATRIX)) {
    for (const entry of Object.values(platformAssets)) {
      names.push(entry.binary, ...entry.extras);
    }
  }
  return Array.from(new Set(names));
}

function allReleaseAssetNames() {
  return [...allAssetNames(), CHECKSUM_MANIFEST];
}

module.exports = {
  allAssetNames,
  allReleaseAssetNames,
  CHECKSUM_MANIFEST,
  checksumManifestUrl,
  DEFAULT_REPO,
  detectBinaryNames,
  releaseAssetUrl,
  releaseBaseUrl,
  releaseBinaryDirectory,
};
