// `npm run screenshot`: renders the demo (src/demo) in headless Edge or Chrome and saves the README image.
import { execFile } from "node:child_process";
import { existsSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { createServer } from "vite";

const out = fileURLToPath(new URL("../../docs/screenshot.png", import.meta.url));

// The demo's songs with their real covers, looked up for the image only: src/demo/covers.json stays out of git.
const songs = [
  ["Rick Astley", "Never Gonna Give You Up"],
  ["Darude", "Sandstorm"],
  ["DJ Satomi", "Castle in the Sky"],
  ["TheFatRat", "MAYDAY"],
  ["Tears for Fears", "Everybody Wants to Rule the World"],
  ["Eurythmics", "Sweet Dreams (Are Made of This)"],
  ["Daft Punk", "Digital Love"],
  ["Kraftwerk", "Computer Love"],
  ["Moderat", "A New Error"],
  ["Kraftwerk", "The Model"],
  ["New Order", "Blue Monday"],
];
const covers = {};
for (const [artist, title] of songs) {
  try {
    const query = new URLSearchParams({ term: `${artist} ${title}`, entity: "song", limit: "1" });
    const found = await (await fetch(`https://itunes.apple.com/search?${query}`)).json();
    const art = found.results?.[0]?.artworkUrl100?.replace("100x100bb", "300x300bb");
    if (art) covers[`${artist} - ${title}`] = `data:image/jpeg;base64,${Buffer.from(await (await fetch(art)).arrayBuffer()).toString("base64")}`;
  } catch (e) {
    console.warn(`no cover for ${artist} - ${title}: ${e}`); // the made-up one then
  }
}
writeFileSync(fileURLToPath(new URL("../src/demo/covers.json", import.meta.url)), JSON.stringify(covers));
console.log(`${Object.keys(covers).length} of ${songs.length} covers`);
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
  await promisify(execFile)(browser, ["--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-prefers-reduced-motion","--window-size=1040,880", "--force-device-scale-factor=2", "--virtual-time-budget=3000", `--screenshot=${out}`, "http://localhost:5174/?shot"]);
} finally {
  await server.close();
}
console.log(`saved ${out}`);
