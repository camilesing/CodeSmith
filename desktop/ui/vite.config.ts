import { defineConfig } from "vite";
import { resolve } from "node:path";

// The vendored DSH plugin bundles are classic scripts addressed through the
// `plugins/??<id>/client.js&rev=<rev>` URL form (chunkUrl in
// packages/client/web/src/system.ts upstream); serve them verbatim from
// ui/public/plugins so the module loader resolves package-local chunks.
export default defineConfig({
  clearScreen: false,
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
