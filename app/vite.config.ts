import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

// Tauri expects a fixed dev port and keeps its own output on screen.
// `--mode demo` runs the UI in a browser with a simulated backend (src/demo), e.g. for the README screenshot.
export default defineConfig(({ mode }) => ({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: mode === "demo" ? 5174 : 5173, strictPort: true },
  build: { target: "es2022" },
  resolve: mode === "demo" ? { alias: [{ find: /^@tauri-apps\/api\/(core|event|app)$/, replacement: fileURLToPath(new URL("src/demo/tauri.ts", import.meta.url)) }] } : {},
}));
