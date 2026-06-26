// Regenerates the README demo screenshot (docs/images/screenshot.png) by driving
// the built web demo in headless Chromium: load it, run the sample query, and
// capture the result. Dev/CI tool only — not part of the app.
//
// Usage (from the repo root, after `just web-build`):
//   node scripts/screenshot.mjs
//
// Env overrides:
//   SCREENSHOT_DIST  static dir to serve   (default: web-rs/dist)
//   SCREENSHOT_OUT   output PNG path        (default: docs/images/screenshot.png)
//   SCREENSHOT_URL   capture this URL instead of serving DIST (e.g. the live demo)
//   SCREENSHOT_W / SCREENSHOT_H  CSS viewport size (default 1360x1040, the wide
//                                desktop layout, tall enough to show the filters,
//                                controls, and the first patch rows — narrow widths
//                                give the stacked mobile layout); SCREENSHOT_DSF
//                                sets the device scale.
//
// The demo detects the absence of `window.__TAURI__` and runs in browser/demo mode
// with bundled sample data, so no backend or sign-in is involved.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { join, extname, normalize } from "node:path";
import { chromium } from "playwright";

const DIST = process.env.SCREENSHOT_DIST || "web-rs/dist";
const OUT = process.env.SCREENSHOT_OUT || "docs/images/screenshot.png";
const REMOTE = process.env.SCREENSHOT_URL || "";
const WIDTH = Number(process.env.SCREENSHOT_W || 1360);
const HEIGHT = Number(process.env.SCREENSHOT_H || 1040);
const DSF = Number(process.env.SCREENSHOT_DSF || 1);

// Minimal MIME map — enough for a Trunk dist (the WASM streaming compile needs the
// exact application/wasm type, which a naive static server omits).
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
};

// Serves `DIST` on an ephemeral port for the duration of `fn`, falling back to
// index.html so the single-page app resolves. Skipped when SCREENSHOT_URL is set.
async function withServer(fn) {
  if (REMOTE) return fn(REMOTE);
  const server = createServer(async (req, res) => {
    const sendIndex = async () => {
      const body = await readFile(join(DIST, "index.html"));
      res.writeHead(200, { "Content-Type": MIME[".html"] });
      res.end(body);
    };
    try {
      let path = decodeURIComponent(new URL(req.url, "http://x").pathname);
      if (path === "/") return await sendIndex();
      // Strip any leading `../` so a request can't escape DIST.
      const file = join(DIST, normalize(path).replace(/^(\.\.[/\\])+/, ""));
      const body = await readFile(file);
      res.writeHead(200, { "Content-Type": MIME[extname(file)] || "application/octet-stream" });
      res.end(body);
    } catch {
      try {
        await sendIndex();
      } catch {
        res.writeHead(404);
        res.end("not found");
      }
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  try {
    return await fn(`http://127.0.0.1:${port}`);
  } finally {
    server.close();
  }
}

await withServer(async (url) => {
  // --no-sandbox keeps Chromium's own sandbox from clashing with restricted CI /
  // container environments; the page we load is our own local build.
  // SCREENSHOT_CHROMIUM lets a caller point at an already-installed full Chromium
  // (e.g. when only the headless shell is unavailable); unset → Playwright default.
  const browser = await chromium.launch({
    args: ["--no-sandbox", "--disable-dev-shm-usage"],
    ...(process.env.SCREENSHOT_CHROMIUM
      ? { executablePath: process.env.SCREENSHOT_CHROMIUM }
      : {}),
  });
  try {
    // No __TAURI__ in plain Chromium → the app enters demo mode automatically.
    const page = await browser.newPage({
      viewport: { width: WIDTH, height: HEIGHT },
      deviceScaleFactor: DSF,
    });
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 30000 });
    // The demo starts empty ("Run a query to list patches") until Run query, just
    // like the real app — click it, then wait for the result summary to render.
    await page.getByRole("button", { name: "Run query" }).click();
    await page.getByText(/patch rows/).first().waitFor({ timeout: 15000 });
    // Clicking can scroll the button into view; reset to the top so the capture
    // starts at the header, not wherever the click left the scroll position.
    await page.evaluate(() => window.scrollTo(0, 0));
    await page.waitForTimeout(400); // let fonts/layout settle before the capture
    await page.screenshot({ path: OUT }); // viewport (header → first result rows)
    console.log(`wrote ${OUT} (${WIDTH}x${HEIGHT} @${DSF}x)`);
  } finally {
    await browser.close();
  }
});
