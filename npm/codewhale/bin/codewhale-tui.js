#!/usr/bin/env node

const { runCodewhaleTui } = require("../scripts/run");

runCodewhaleTui().catch((error) => {
  console.error("Failed to start codewhale-tui:", error.message);
  process.exit(1);
});
