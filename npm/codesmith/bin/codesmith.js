#!/usr/bin/env node

const { runCodeSmith } = require("../scripts/run");

runCodeSmith().catch((error) => {
  console.error("Failed to start codesmith:", error.message);
  process.exit(1);
});
