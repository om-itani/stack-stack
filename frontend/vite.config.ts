import { defineConfig } from "vite";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [tailwindcss()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    // Fingerprinted bundle dir; assets.rs serves this path as immutable.
    assetsDir: "assets",
    sourcemap: false,
  },
  server: {
    port: 5173,
    // `npm run dev` talks to the Rust backend instead of needing a second copy
    // of the API. ws: true forwards the telemetry socket too.
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8553",
        changeOrigin: true,
        ws: true,
      },
    },
  },
});
