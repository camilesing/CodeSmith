import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

test("ThreadStore is initialized before bridge startup opens it", async () => {
  const source = await fs.readFile(path.join(__dirname, "../src/index.mjs"), "utf8");
  // indexOf finds the first textual occurrence, so a marker that moved into a
  // comment or disappeared in a refactor must fail loudly here rather than
  // silently weakening the ordering assertions below.
  const positionOf = (marker) => {
    const index = source.indexOf(marker);
    assert.notEqual(index, -1, `marker "${marker}" not found in src/index.mjs; source may have been refactored`);
    return index;
  };
  const declaration = positionOf("class ThreadStore");
  const startupUse = positionOf("await ThreadStore.open");
  const wsStart = positionOf("wsClient.start");
  const reattachCall = positionOf("reattachActiveTurns().catch");

  assert.ok(declaration < startupUse, "class ThreadStore must be declared before ThreadStore.open");
  assert.ok(startupUse < wsStart, "await ThreadStore.open must complete before wsClient.start");
  assert.ok(startupUse < reattachCall, "await ThreadStore.open must complete before reattachActiveTurns");
  assert.ok(wsStart < reattachCall, "wsClient.start must run before reattachActiveTurns");
});
