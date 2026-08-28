#!/usr/bin/env node

const { runCodeSmithTui } = require("../scripts/run");

runCodeSmithTui().catch((error) => {
  console.error("Failed to start codesmith-tui:", error.message);
  process.exit(1);
});
