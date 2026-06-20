#!/usr/bin/env node

const { run } = require("../scripts/run");

run("codesmith").catch((error) => {
  console.error("Failed to start codesmith:", error.message);
  process.exit(1);
});
