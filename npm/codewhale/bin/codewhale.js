#!/usr/bin/env node

const { runCodewhale } = require("../scripts/run");

runCodewhale().catch((error) => {
  console.error("Failed to start codewhale:", error.message);
  process.exit(1);
});
