const { spawnSync } = require("child_process");
const { getBinaryPath } = require("./install");

const pkg = require("../package.json");

function isVersionFlag(args = process.argv.slice(2)) {
  return args.includes("--version") || args.includes("-V");
}

function handleVersionFallback(binaryName) {
  if (isVersionFlag()) {
    const binVersion =
      pkg.codesmithBinaryVersion || pkg.version;
    console.log(`${binaryName} (npm wrapper) v${pkg.version}`);
    console.log(`binary version: v${binVersion}`);
    console.log(`repo: ${pkg.repository?.url || "N/A"}`);
    process.exit(0);
  }
}

async function run(binaryName) {
  // Intercept --version before attempting binary download/launch
  handleVersionFallback(binaryName);

  const binaryPath = await getBinaryPath(binaryName);
  const result = spawnSync(binaryPath, process.argv.slice(2), {
    stdio: "inherit",
  });
  if (result.error) {
    throw result.error;
  }
  if (result.signal) {
    // Killed by a signal (status is null): mirror the conventional
    // 128+signal-number exit code so callers can distinguish signal deaths
    // from genuine failures.
    const SIGNAL_CODES = {
      SIGHUP: 1,
      SIGINT: 2,
      SIGQUIT: 3,
      SIGKILL: 9,
      SIGSEGV: 11,
      SIGTERM: 15,
    };
    process.exit(128 + (SIGNAL_CODES[result.signal] ?? 0));
  }
  process.exit(result.status ?? 1);
}

async function runCodeSmith() {
  await run("codesmith");
}

module.exports = {
  run,
  runCodeSmith,
  _internal: { isVersionFlag },
};

if (require.main === module) {
  // Direct entry (`node scripts/run.js`): bin/codesmith.js already wraps
  // this; this path needs the same treatment or a download failure escapes
  // as a raw unhandled-rejection stack trace.
  runCodeSmith().catch((error) => {
    console.error("Failed to start codesmith:", error?.message ?? error);
    process.exit(1);
  });
}
