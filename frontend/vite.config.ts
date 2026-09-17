import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Dev server proxies /api (REST + the /api/stream/:id WebSocket) to the
// Rust backend so `npm run dev` can hot-reload the UI against a real
// `cargo run` backend on the default port 8090.
export default defineConfig({
  plugins: [svelte()],
  server: {
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8090",
        ws: true,
      },
    },
  },
  build: {
    outDir: "dist",
  },
});
