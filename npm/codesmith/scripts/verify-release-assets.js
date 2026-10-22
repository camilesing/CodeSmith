const crypto = require("crypto");
const https = require("https");
const {
  allAssetNames,
  allReleaseAssetNames,
  checksumManifestUrl,
  DEFAULT_REPO,
  releaseAssetUrl,
} = require("./artifacts");

const pkg = require("../package.json");

// This script's whole purpose is release integrity verification, so it
// refuses plaintext HTTP for the initial URL and for every redirect target
// — an https -> http downgrade would let a MITM serve a tampered manifest.
// Plain HTTP is allowed only when CODESMITH_RELEASE_BASE_URL is explicitly
// overridden (local testing against a staging origin).
function requireHttps(url) {
  if (!url.startsWith("https:") && !process.env.CODESMITH_RELEASE_BASE_URL) {
    throw new Error(`Refusing insecure HTTP request: ${url}`);
  }
}

function resolveBinaryVersion() {
  const configuredVersion =
    process.env.CODESMITH_VERSION ||
    pkg.codesmithBinaryVersion ||
    pkg.version;
  return String(configuredVersion).trim();
}

function resolveRepo() {
  return process.env.CODESMITH_GITHUB_REPO || DEFAULT_REPO;
}

// Without this, a hung connection (GitHub asset endpoints, CI networks)
// leaves the request pending forever and blocks the release gate.
const REQUEST_TIMEOUT_MS = 30_000;

function armRequestTimeout(req, url) {
  req.setTimeout(REQUEST_TIMEOUT_MS, () => {
    req.destroy(new Error(`request timed out after ${REQUEST_TIMEOUT_MS} ms: ${url}`));
  });
}

function clientFor(url) {
  requireHttps(url);
  if (url.startsWith("https:")) {
    return https;
  }
  // Only reachable when CODESMITH_RELEASE_BASE_URL is explicitly
  // overridden (see requireHttps) — local testing against a staging origin.
  return require("http");
}

function requestStatus(url, method = "HEAD", redirects = 0) {
  if (redirects > 10) {
    throw new Error(`Too many redirects while checking ${url}`);
  }
  const client = clientFor(url);
  return new Promise((resolve, reject) => {
    const req = client.request(
      url,
      {
        method,
        headers: {
          "User-Agent": "codesmith-npm-release-check",
        },
      },
      (res) => {
        const status = res.statusCode || 0;
        const location = res.headers.location;
        res.resume();
        if (status >= 300 && status < 400 && location) {
          const next = new URL(location, url).toString();
          resolve(requestStatus(next, method, redirects + 1));
          return;
        }
        resolve(status);
      },
    );
    req.on("error", reject);
    armRequestTimeout(req, url);
    req.end();
  });
}

async function downloadText(url, redirects = 0) {
  const client = clientFor(url);
  return new Promise((resolve, reject) => {
    const req = client.get(
      url,
      {
        headers: {
          "User-Agent": "codesmith-npm-release-check",
        },
      },
      (res) => {
        const status = res.statusCode || 0;
        if (status >= 300 && status < 400 && res.headers.location) {
          if (redirects > 10) {
            reject(new Error(`Too many redirects while downloading ${url}`));
            return;
          }
          const next = new URL(res.headers.location, url).toString();
          resolve(downloadText(next, redirects + 1));
          return;
        }
        if (status !== 200) {
          reject(new Error(`Request failed with status ${status}: ${url}`));
          res.resume();
          return;
        }
        const chunks = [];
        res.setEncoding("utf8");
        res.on("data", (chunk) => chunks.push(chunk));
        res.on("end", () => resolve(chunks.join("")));
      },
    );
    req.on("error", reject);
    armRequestTimeout(req, url);
  });
}

function parseChecksumManifest(text) {
  const checksums = new Map();
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) {
      continue;
    }
    const match = trimmed.match(/^([a-fA-F0-9]{64})\s+\*?(.+)$/);
    if (!match) {
      throw new Error(`Invalid checksum manifest line: ${trimmed}`);
    }
    checksums.set(match[2], match[1].toLowerCase());
  }
  return checksums;
}

async function downloadBuffer(url, redirects = 0) {
  if (redirects > 10) {
    throw new Error(`Too many redirects while downloading ${url}`);
  }
  const client = clientFor(url);
  return new Promise((resolve, reject) => {
    const req = client.get(
      url,
      {
        headers: {
          "User-Agent": "codesmith-npm-release-check",
        },
      },
      (res) => {
        const status = res.statusCode || 0;
        if (status >= 300 && status < 400 && res.headers.location) {
          const next = new URL(res.headers.location, url).toString();
          resolve(downloadBuffer(next, redirects + 1));
          return;
        }
        if (status !== 200) {
          res.resume();
          reject(new Error(`Request failed with status ${status}: ${url}`));
          return;
        }
        const chunks = [];
        res.on("data", (chunk) => chunks.push(chunk));
        res.on("end", () => resolve(Buffer.concat(chunks)));
      },
    );
    req.on("error", reject);
    armRequestTimeout(req, url);
  });
}

async function run() {
  const version = resolveBinaryVersion();
  const repo = resolveRepo();
  const assets = allReleaseAssetNames();

  // Drift guard: install.js prefers codesmithBinaryVersion over version, and
  // this gate only checks that assets exist for the binary version — a
  // version bump that forgets codesmithBinaryVersion would silently ship a
  // package whose npm version mismatches the fetched binary.
  if (
    pkg.codesmithBinaryVersion &&
    pkg.codesmithBinaryVersion !== pkg.version &&
    !process.env.CODESMITH_ALLOW_VERSION_DRIFT
  ) {
    throw new Error(
      `package.json version (${pkg.version}) != codesmithBinaryVersion ` +
        `(${pkg.codesmithBinaryVersion}); set CODESMITH_ALLOW_VERSION_DRIFT=1 ` +
        "only when the divergence is intended",
    );
  }

  console.log(`Verifying ${assets.length} release assets for ${repo}@v${version}...`);
  const checksums = parseChecksumManifest(
    await downloadText(checksumManifestUrl(version, repo)),
  );
  await Promise.all(
    allAssetNames().map(async (asset) => {
      // Actually compare the SHA-256 of each downloaded asset against the
      // manifest value — checking only that the name appears as a key would
      // let a tampered release pass verification.
      const expected = checksums.get(asset);
      if (!expected) {
        throw new Error(`Checksum manifest is missing ${asset}`);
      }
      const url = releaseAssetUrl(asset, version, repo);
      const actual = crypto
        .createHash("sha256")
        .update(await downloadBuffer(url))
        .digest("hex");
      if (actual !== expected) {
        throw new Error(
          `${asset} checksum mismatch: expected ${expected}, got ${actual} (${url})`,
        );
      }
      console.log(`  ok ${asset} (sha256 verified)`);
    }),
  );
  console.log("Release assets verified.");
}

run().catch((error) => {
  console.error("Release asset verification failed:", error.message);
  process.exit(1);
});
