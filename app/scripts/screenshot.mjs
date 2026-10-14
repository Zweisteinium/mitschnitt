// `npm run screenshot`: renders the demo (src/demo) in headless Edge or Chrome and saves the README image.
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { createServer } from "vite";

const out = fileURLToPath(new URL("../../docs/screenshot.png", import.meta.url));
const browser = [
  process.env.BROWSER,
  "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
].find((path) => path && existsSync(path));
if (!browser) throw new Error("no Edge or Chrome found (set BROWSER)");

const server = await createServer({ mode: "demo", logLevel: "warn" });
await server.listen();
try {
  // Async: the dev server in this process has to answer the browser meanwhile. Virtual time lets the demo start and
  // fill its meters without real waiting.
  await promisify(execFile)(browser, ["--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-prefers-reduced-motion","--window-size=1040,800", "--force-device-scale-factor=2", "--virtual-time-budget=3000", `--screenshot=${out}`, "http://localhost:5174/?shot"]);
} finally {
  await server.close();
}
console.log(`saved ${out}`);
