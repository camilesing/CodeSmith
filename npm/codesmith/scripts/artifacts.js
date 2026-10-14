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
    "  cargo install codesmith --locked",
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
  // Plain truthiness would route "0"/"false" — the conventional opt-out
  // spellings — to the CNB mirror.
  const flag = String(process.env.CODESMITH_USE_CNB_MIRROR || "")
    .trim()
    .toLowerCase();
  return flag !== "" && flag !== "0" && flag !== "false";
}

function releaseBaseUrl(version, repo = DEFAULT_REPO) {
  // CODESMITH_RELEASE_BASE_URL is the canonical override.
  const override = String(process.env.CODESMITH_RELEASE_BASE_URL || "").trim();
  if (override) {
    const base = override.endsWith("/") ? override : `${override}/`;
    try {
      new URL(base);
    } catch {
      throw new Error(
        `CODESMITH_RELEASE_BASE_URL must be a valid absolute URL (got: ${override}).`,
      );
    }
    return base;
  }
  // When CODESMITH_USE_CNB_MIRROR is enabled, use the CNB (China-friendly)
  // mirror that already builds and publishes binary release assets.
  if (isCnbMirrorEnabled()) {
    return `https://cnb.cool/${repo}/-/releases/v${version}/`;
  }
  return `https://github.com/${repo}/releases/download/v${version}/`;
}

function releaseAssetUrl(baseName, version, repo = DEFAULT_REPO) {
  return new URL(baseName, releaseBaseUrl(version, repo)).toString();
}

function checksumManifestUrl(version, repo = DEFAULT_REPO) {
  return releaseAssetUrl(CHECKSUM_MANIFEST, version, repo);
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
