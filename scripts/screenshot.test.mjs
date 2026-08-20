// Guards the parts of screenshot.mjs that a dependency upgrade can break silently.
//
// This exists because `selfsigned` 2 -> 5 turned `generate` into an async function
// while leaving its resolved shape identical. The un-awaited call still "worked" —
// it destructured a Promise, so key/cert were `undefined` and the HTTPS server was
// built with no TLS material. Nothing caught it, because screenshot.yml only fires
// on release-published / workflow_dispatch — after the release exists, where a
// failure just means the README image quietly fails to refresh.
//
// Runs in release.yml's `verify` job, which `create-release` needs: a tool broken by
// a dependency bump therefore refuses the release. It does NOT run on PRs, so a break
// reaches main green and is caught at tag time. Deliberately browser-free (no
// Playwright, no Chromium download, no built dist) so it costs seconds in that gate;
// the real capture is still exercised by screenshot.yml itself.

import { test } from "node:test";
import assert from "node:assert/strict";
import { get } from "node:https";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { collectAssets, withServer } from "./screenshot.mjs";

// Minimal stand-in for a Trunk dist: an index, a nested asset, and a .wasm (whose
// exact MIME type the streaming compile depends on).
async function fixtureDist() {
  const dir = await mkdtemp(join(tmpdir(), "screenshot-dist-"));
  await writeFile(join(dir, "index.html"), "<!doctype html><title>fixture</title>");
  await writeFile(join(dir, "app.wasm"), Buffer.from([0x00, 0x61, 0x73, 0x6d]));
  await mkdir(join(dir, "assets"));
  await writeFile(join(dir, "assets", "app.css"), ".x{}");
  return dir;
}

// The server is loopback HTTPS with a throwaway cert, so verification is off here
// exactly as `ignoreHTTPSErrors` does it for Chromium in the real capture.
function fetchOnce(url) {
  return new Promise((resolve, reject) => {
    get(url, { rejectUnauthorized: false }, (res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () =>
        resolve({
          status: res.statusCode,
          type: res.headers["content-type"],
          nosniff: res.headers["x-content-type-options"],
          body: Buffer.concat(chunks),
        }),
      );
    }).on("error", reject);
  });
}

test("collectAssets maps every file to a URL path, recursively", async () => {
  const dir = await fixtureDist();
  try {
    const assets = await collectAssets(dir);
    assert.deepEqual(
      [...assets.keys()].sort(),
      ["/app.wasm", "/assets/app.css", "/index.html"],
    );
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("withServer serves the dist over working TLS", async () => {
  const dir = await fixtureDist();
  try {
    // Reaching a 200 at all is the regression guard: an unawaited `generate` leaves
    // key/cert undefined and the TLS handshake never completes.
    const res = await withServer((url) => fetchOnce(`${url}/index.html`), dir);
    assert.equal(res.status, 200);
    assert.match(res.type, /^text\/html/);
    assert.equal(res.nosniff, "nosniff");
    assert.match(res.body.toString(), /fixture/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("withServer sends .wasm as application/wasm", async () => {
  const dir = await fixtureDist();
  try {
    // Trunk's streaming compile rejects anything else, so this MIME is load-bearing.
    const res = await withServer((url) => fetchOnce(`${url}/app.wasm`), dir);
    assert.equal(res.status, 200);
    assert.equal(res.type, "application/wasm");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("an unknown path falls back to index.html so the SPA loads", async () => {
  const dir = await fixtureDist();
  try {
    const res = await withServer((url) => fetchOnce(`${url}/patches`), dir);
    assert.equal(res.status, 200);
    assert.match(res.body.toString(), /fixture/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("withServer closes its listener once fn returns", async () => {
  const dir = await fixtureDist();
  try {
    const url = await withServer((u) => u, dir);
    await assert.rejects(() => fetchOnce(`${url}/index.html`));
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
