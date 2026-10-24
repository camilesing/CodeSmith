#!/usr/bin/env node
import { constants as fsConstants } from "node:fs";
import fs from "node:fs/promises";
import path from "node:path";

import {
  cleanEnvValue,
  formatValidationReport,
  parseEnvText,
  validateBridgeConfig
} from "../src/lib.mjs";

const DEFAULT_WORKSPACE_ROOT = "/opt/whalebro";

try {
  // Inside the try so an unknown argument gets the friendly failure path
  // below instead of a raw stack trace.
  const args = parseArgs(process.argv.slice(2));
  const bridgeEnv = args.env ? parseEnvText(await fs.readFile(args.env, "utf8")) : process.env;
  const runtimeEnv = args.runtimeEnv
    ? parseEnvText(await fs.readFile(args.runtimeEnv, "utf8"))
    : null;
  const result = validateBridgeConfig(bridgeEnv, {
    runtimeEnv,
    workspaceRoot: args.workspaceRoot
  });

  if (args.checkFilesystem) {
    await appendFilesystemChecks(result, bridgeEnv);
  }

  if (args.json) {
    console.log(JSON.stringify(result, null, 2));
  } else {
    console.log(formatValidationReport(result));
  }
  process.exitCode = result.ok ? 0 : 1;
} catch (error) {
  // 2 = operational/usage error (missing option value, unknown argument,
  // unreadable --env file); 1 is reserved for "config invalid" so CI can
  // distinguish the two. error may be a non-Error throw.
  console.error(`Failed to validate config: ${error?.message ?? String(error)}`);
  process.exitCode = 2;
}

function parseArgs(argv) {
  const parsed = {
    env: "",
    runtimeEnv: "",
    workspaceRoot: DEFAULT_WORKSPACE_ROOT,
    checkFilesystem: false,
    json: false
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const readOptionValue = (name) => {
      const value = argv[index + 1];
      if (!value || value.startsWith("--")) {
        console.error(`missing value for ${name}`);
        process.exit(2);
      }
      index += 1;
      return value;
    };
    switch (arg) {
      case "--env":
        parsed.env = readOptionValue("--env");
        break;
      case "--runtime-env":
        parsed.runtimeEnv = readOptionValue("--runtime-env");
        break;
      case "--workspace-root":
        parsed.workspaceRoot = readOptionValue("--workspace-root");
        break;
      case "--check-filesystem":
        parsed.checkFilesystem = true;
        break;
      case "--json":
        parsed.json = true;
        break;
      case "-h":
      case "--help":
        printHelp();
        process.exit(0);
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }
  return parsed;
}

async function appendFilesystemChecks(result, env) {
  const workspace = cleanEnvValue(env.CODESMITH_WORKSPACE);
  if (workspace) {
    await checkDirectoryAccess(result, workspace, "workspace", false);
  }

  const threadMapPath = cleanEnvValue(env.FEISHU_THREAD_MAP_PATH);
  if (threadMapPath) {
    const parent = path.dirname(threadMapPath);
    await checkDirectoryAccess(result, parent, "thread map directory", true);
  }
}

async function checkDirectoryAccess(result, dir, label, writable) {
  const access = writable ? "writable" : "readable";
  try {
    const stat = await fs.stat(dir);
    if (!stat.isDirectory()) {
      result.errors.push({ code: "not_directory", message: `${label} is not a directory: ${dir}` });
      result.ok = false;
      return;
    }
    const modes = writable
      ? fsConstants.R_OK | fsConstants.W_OK | fsConstants.X_OK
      : fsConstants.R_OK | fsConstants.X_OK;
    await fs.access(dir, modes);
    result.info.push({ code: `${access}_directory`, message: `${label} is ${access}: ${dir}` });
  } catch (error) {
    // Include the underlying failure (ENOENT vs EACCES vs a stat/access
    // race): a bare "not readable" sends operators to fix permissions when
    // the directory may simply not exist.
    const detail = error?.code ? ` (${error.code})` : "";
    result.errors.push({ code: "directory_access", message: `${label} is not ${access}: ${dir}${detail}` });
    result.ok = false;
  }
}

function printHelp() {
  console.log(`Usage: node scripts/validate-config.mjs [options]

Options:
  --env FILE             Read bridge env from FILE instead of process.env.
  --runtime-env FILE     Read runtime env and verify the shared bearer token.
  --workspace-root DIR   Expected remote workspace root (default: ${DEFAULT_WORKSPACE_ROOT}).
  --check-filesystem     Verify workspace and thread-map paths are usable.
  --json                 Print machine-readable JSON.
  -h, --help             Show this help.
`);
}
