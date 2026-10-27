import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [svelte()],
  build: {
    target: "es2022",
    // The server's CSP allows no inline scripts, styles or data: URLs.
    modulePreload: { polyfill: false },
    assetsInlineLimit: 0,
  },
  worker: { format: "es" },
  // `npm run dev` talks to a local memo-server (public_url = "http://localhost:5173").
  server: { proxy: { "/api": "http://127.0.0.1:8080" } },
});
