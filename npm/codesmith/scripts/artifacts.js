const path = require("path");
const os = require("os");

const CHECKSUM_MANIFEST = "codesmith-artifacts-sha256.txt";

const ASSET_MATRIX = {
  linux: {
    x64: ["codesmith-linux-x64", "codesmith-tui-linux-x64"],
    arm64: ["codesmith-linux-arm64", "codesmith-tui-linux-arm64"],
    riscv64: ["codesmith-linux-riscv64", "codesmith-tui-linux-riscv64"],
  },
  darwin: {
    x64: ["codesmith-macos-x64", "codesmith-tui-macos-x64"],
    arm64: ["codesmith-macos-arm64", "codesmith-tui-macos-arm64"],
  },
  win32: {
    x64: ["codesmith-windows-x64.exe", "codesmith-tui-windows-x64.exe", "codesmith.bat"],
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
    codesmith: pair[0],
    tui: pair[1],
  };
}

function unsupportedBuildHint() {
  return [
    "No prebuilt binary is available for this platform/architecture combo.",
    "You can still run codesmith by building from source with Cargo:",
    "",
    "  # Requires Rust 1.88+ (https://rustup.rs)",
    "  cargo install codesmith-cli --locked   # provides `codesmith`",
    "  cargo install codesmith-tui --locked   # provides `codesmith-tui`",
    "",
    "Or build from a checkout:",
    "",
    "  git clone https://github.com/camilesing/CodeSmith.git",
    "  cd CodeSmith",
    "  cargo install --path crates/cli --locked",
    "  cargo install --path crates/tui --locked",
    "",
    "See https://github.com/camilesing/CodeSmith/blob/main/docs/INSTALL.md",
    "for cross-compilation, mirror, and Linux ARM64 specifics.",
  ].join("\n");
}

function executableName(base, platform) {
  return platform === "win32" ? `${base}.exe` : base;
}

function releaseBaseUrl(version, repo = "camilesing/CodeSmith") {
  // CODESMITH_RELEASE_BASE_URL is the canonical override.
  // CODESMITH_RELEASE_BASE_URL / CODESMITH_RELEASE_BASE_URL are legacy aliases.
  const override =
    process.env.CODESMITH_RELEASE_BASE_URL ||
    process.env.CODESMITH_RELEASE_BASE_URL;
  if (override) {
    const trimmed = String(override).trim();
    return trimmed.endsWith("/") ? trimmed : `${trimmed}/`;
  }
  // When CODESMITH_USE_CNB_MIRROR is set, use the CNB (China-friendly)
  // mirror that already builds and publishes binary release assets.
  if (process.env.CODESMITH_USE_CNB_MIRROR) {
    return `https://cnb.cool/camilesing/CodeSmith/-/releases/v${version}/`;
  }
  return `https://github.com/${repo}/releases/download/v${version}/`;
}

function releaseAssetUrl(baseName, version, repo = "camilesing/CodeSmith") {
  return new URL(baseName, releaseBaseUrl(version, repo)).toString();
}

function checksumManifestUrl(version, repo = "camilesing/CodeSmith") {
  return releaseAssetUrl(CHECKSUM_MANIFEST, version, repo);
}

function releaseBinaryDirectory() {
  return path.join(__dirname, "..", "bin", "downloads");
}

function allAssetNames() {
  const names = [];
  for (const platformAssets of Object.values(ASSET_MATRIX)) {
    for (const assets of Object.values(platformAssets)) {
      names.push(...assets);
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
  detectBinaryNames,
  executableName,
  releaseAssetUrl,
  releaseBaseUrl,
  releaseBinaryDirectory,
};
