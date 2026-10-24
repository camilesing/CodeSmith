#!/usr/bin/env node

// Everything inside main(): a require() failure (module resolution, syntax
// error in a transitive dep) or a renamed export throws synchronously and
// must still flow through the single error handler below, not escape as a
// raw stack trace.
async function main() {
  const { runCodeSmith } = require("../scripts/run");
  await runCodeSmith();
}

main().catch((error) => {
  console.error("Failed to start codesmith:", error?.message ?? error);
  process.exit(1);
});
