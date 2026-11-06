import { defineConfig } from "vite";
import { resolve } from "node:path";

// The vendored plugin bundles are classic scripts addressed through the
// `plugins/??<id>/client.js&rev=<rev>` URL form (chunkUrl in
// packages/client/web/src/system.ts upstream); serve them verbatim from
// ui/public/plugins so the module loader resolves package-local chunks.
export default defineConfig({
  clearScreen: false,
  // The vendored cordis-plugin-loader ships its Node face; browser boot
  // supplies loader.internal before importing plugin entries, so its
  // filesystem resolver is never invoked — stub node:module and pin the
  // version probes to the empty-internal branch (same tricks as upstream
  // apps/web/vite.config.ts).
  resolve: {
    alias: [{ find: /^node:module$/, replacement: resolve(__dirname, "src/node-module-stub.ts") }],
  },
  define: {
    "process.versions.node": '"0.0.0"',
    "process.execArgv": "[]",
    "process.env.CORDIS_SHARED": "undefined",
  },
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: "chrome120",
    outDir: "dist",
    rollupOptions: {
      input: resolve(__dirname, "index.html"),
    },
  },
});
