#!/usr/bin/env node

const crypto = require("crypto");
const fs = require("fs/promises");
const { createReadStream } = require("fs");
const path = require("path");

const {
  allAssetNames,
  CHECKSUM_MANIFEST,
  detectBinaryNames,
} = require("../../npm/codesmith/scripts/artifacts");

const WINDOWS_LAUNCHER = "codesmith.bat";
const WINDOWS_CLI_ASSET = "codesmith-windows-x64.exe";

// Streaming hash: release executables can be hundreds of MB; whole-file
// buffering just to hash them scales peak memory with binary size.
async function sha256(filePath) {
  const hash = crypto.createHash("sha256");
  for await (const chunk of createReadStream(filePath)) {
    hash.update(chunk);
  }
  return hash.digest("hex");
}

async function main() {
  const prepareAllAssets =
    process.env.CODESMITH_PREPARE_ALL_ASSETS === "1";
  const outputDir = path.resolve(
    process.argv[2] || path.join("target", "npm-release-assets"),
  );
  const buildDir = path.resolve(
    process.argv[3] || path.join("target", "release"),
  );
  const { codesmith } = detectBinaryNames();
  const isWindows = process.platform === "win32";

  const assets = [
    {
      source: path.join(buildDir, isWindows ? "codesmith.exe" : "codesmith"),
      target: codesmith,
    },
  ];

  if (prepareAllAssets) {
    for (const assetName of allAssetNames()) {
      if (assetName === WINDOWS_LAUNCHER) {
        continue;
      }
      if (assets.some((asset) => asset.target === assetName)) {
        continue;
      }
      // Map platform-named assets to their OWN files in buildDir: both
      // branches of the old ternary produced the same locally built
      // binary, so e.g. the *-windows-x64.exe asset silently contained
      // the Linux binary.
      const localBinaryName = isWindows ? "codesmith.exe" : "codesmith";
      const sourceName = assetName === "codesmith" ? localBinaryName : assetName;
      assets.push({
        source: path.join(buildDir, sourceName),
        target: assetName,
      });
    }
  }

  // Pre-flight before the destructive rm/mkdir phase: in PREPARE_ALL_ASSETS
  // mode the cross-platform sources exist only if the operator staged
  // cross-build fixtures in buildDir — a missing file would otherwise abort
  // copyFile mid-way with a raw ENOENT after outputDir was already wiped,
  // leaving a partially populated directory and no manifest.
  const missing = (
    await Promise.all(
      assets.map(async (asset) =>
        (await fs.stat(asset.source).catch(() => null)) ? null : asset.source,
      ),
    )
  ).filter(Boolean);
  if (missing.length > 0) {
    throw new Error(
      `Missing build outputs in ${buildDir}:\n  ${missing.join("\n  ")}\n` +
        "Cross-build or stage the per-platform binaries before running with CODESMITH_PREPARE_ALL_ASSETS=1.",
    );
  }

  // Clear stale assets from previous runs: mkdir alone leaves behind files
  // with no manifest entry, which a later upload could publish with
  // mismatched checksums.
  await fs.rm(outputDir, { recursive: true, force: true });
  await fs.mkdir(outputDir, { recursive: true });

  // Copy + hash are independent per asset (the manifest is re-sorted
  // afterwards anyway), so run them concurrently.
  const manifestLines = await Promise.all(
    assets.map(async (asset) => {
      const outputPath = path.join(outputDir, asset.target);
      await fs.copyFile(asset.source, outputPath);
      return `${await sha256(outputPath)}  ${asset.target}`;
    }),
  );

  if (assets.some((asset) => asset.target === WINDOWS_CLI_ASSET)) {
    const batContent = [
      "@echo off",
      "set NO_ANIMATIONS=1",
      "where wt >nul 2>nul",
      'if "%ERRORLEVEL%"=="0" (',
      `    wt --title CodeSmith cmd /k "%~dp0${WINDOWS_CLI_ASSET}"`,
      ") else (",
      `    "%~dp0${WINDOWS_CLI_ASSET}"`,
      ")",
      "",
    ].join("\r\n");
    const batPath = path.join(outputDir, WINDOWS_LAUNCHER);
    await fs.writeFile(batPath, batContent, "utf8");
    const batHash = await sha256(batPath);
    manifestLines.push(`${batHash}  ${WINDOWS_LAUNCHER}`);
    console.log(`Generated ${batPath}`);
  }

  manifestLines.sort();
  const manifestPath = path.join(outputDir, CHECKSUM_MANIFEST);
  await fs.writeFile(manifestPath, `${manifestLines.join("\n")}\n`, "utf8");

  // manifestLines, not assets: the Windows launcher gets a manifest entry
  // without being an asset entry.
  console.log(`Prepared ${manifestLines.length} assets in ${outputDir}`);
  console.log(`Wrote checksum manifest ${manifestPath}`);
}

main().catch((error) => {
  console.error("Failed to prepare local release assets:", error);
  process.exit(1);
});
