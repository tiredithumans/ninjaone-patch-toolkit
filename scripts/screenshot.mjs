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

import { createServer } from "node:https";
import { readFile, readdir } from "node:fs/promises";
import { join, extname, relative, resolve, sep } from "node:path";
import { chromium } from "playwright";
import selfsigned from "selfsigned";

const DIST = process.env.SCREENSHOT_DIST || "web-rs/dist";
const OUT = process.env.SCREENSHOT_OUT || "docs/images/screenshot.png";
const REMOTE = process.env.SCREENSHOT_URL || "";
const WIDTH = Number(process.env.SCREENSHOT_W || 1360);
const HEIGHT = Number(process.env.SCREENSHOT_H || 1040);
const DSF = Number(process.env.SCREENSHOT_DSF || 1);

// Absolute DIST root — the build output is enumerated under it into an allowlist.
const DIST_ROOT = resolve(DIST);

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

// Enumerate the build output once into an allowlist mapping each URL path to its
// on-disk file. The request path then only ever indexes this map — it's never joined
// into a filesystem path and never reaches the response body — so a crafted path can
// neither escape DIST nor reflect request input back into the served HTML.
async function collectAssets(dir, root = dir, out = new Map()) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const abs = join(dir, entry.name);
    if (entry.isDirectory()) await collectAssets(abs, root, out);
    else out.set("/" + relative(root, abs).split(sep).join("/"), abs);
  }
  return out;
}

// Serves `DIST` over loopback HTTPS on an ephemeral port for the duration of `fn`,
// falling back to index.html (the allowlist above resolves every other path) so the
// single-page app loads. TLS uses a throwaway self-signed cert (regenerated per run)
// so the transport is never cleartext, even on 127.0.0.1; Chromium is told to accept
// the self-signed cert below. Skipped when SCREENSHOT_URL is set.
async function withServer(fn) {
  if (REMOTE) return fn(REMOTE);
  const assets = await collectAssets(DIST_ROOT);
  const { private: key, cert } = selfsigned.generate(
    [{ name: "commonName", value: "localhost" }],
    { days: 1, keySize: 2048, altNames: [{ type: 7, ip: "127.0.0.1" }] },
  );
  const server = createServer({ key, cert }, async (req, res) => {
    const sendIndex = async () => {
      const body = await readFile(join(DIST, "index.html"));
      res.writeHead(200, {
        "Content-Type": MIME[".html"],
        "X-Content-Type-Options": "nosniff",
      });
      res.end(body);
    };
    try {
      const path = decodeURIComponent(new URL(req.url, "https://localhost").pathname);
      // The request path only ever indexes the allowlist; a miss (including "/")
      // serves index.html. `file` is always a trusted, pre-enumerated path — never
      // derived from the request — so request input never reaches the response body.
      const file = assets.get(path);
      if (!file) return await sendIndex();
      const body = await readFile(file);
      res.writeHead(200, {
        "Content-Type": MIME[extname(file)] || "application/octet-stream",
        "X-Content-Type-Options": "nosniff",
      });
      // deepcode ignore Xss: `file` is a pre-enumerated allowlist value (collectAssets),
      // never request-derived — the request path only ever indexes the Map (a miss serves
      // index.html), so request input never reaches the response body. Dev-only loopback tool.
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
  await new Promise((ready) => server.listen(0, "127.0.0.1", ready));
  const { port } = server.address();
  try {
    return await fn(`https://127.0.0.1:${port}`);
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
      // Accept the throwaway self-signed cert from the local HTTPS server above.
      ignoreHTTPSErrors: true,
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
