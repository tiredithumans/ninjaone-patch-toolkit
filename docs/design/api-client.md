# NinjaOne API client: retry and pagination

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/api/mod.rs` (`NinjaApiClient`), `src-tauri/Cargo.toml` (reqwest features).
Spec: rendered docs at <https://app.ninjarmm.com/apidocs/?links.active=core>; raw OpenAPI at
<https://app.ninjarmm.com/apidocs-beta/NinjaRMM-API-v2.yaml> (grep it; the SPA can't be scraped).
**Verify endpoint shapes, params, and field/status enums against the spec — never infer them
from endpoint names or memory.**

## Reuse the shared retry + pagination

Every call goes through `NinjaApiClient`: `{base}/api/v2{path}`, bearer auth, retry on timeout /
connect failure / **5xx** / 429 (honors `Retry-After`) / 401 (forces a token refresh).
`get_paginated` handles **both** a bare JSON array **and** the `{ results, cursor }` envelope,
where `cursor` may be a string or a `{ name, offset, … }` object; it stops when a page returns 0
rows even if the server echoes a stale token. Don't hand-roll a second reqwest/cursor loop.

## Paginated bodies are deserialized straight into `T`; everything else goes through `Value`

`send_with_retry` owns the request/retry loop and returns the raw `reqwest::Response`;
`request_raw` decodes it as a `Value` (single-shot GETs, acting POSTs — all small bodies) and
`request_page` decodes it as a `PageBody<T>` (`api::parse_page`). The paginated path exists
because a whole-fleet third-party feed runs to six figures, and a `Value` intermediate allocates a
`String` for every JSON key on every row and then walks the tree again to build the `Patch` — the
rows are parsed twice. `parse_page` dispatches on the body's first non-whitespace byte and reads
the `{ results, cursor }` wrapper via `serde_json`'s `RawValue` (hence the `raw_value` feature),
so the shape checks stay explicit and the rows are parsed once. The `after`-paginated branch needs
each row's id, which is no longer reachable generically — `api::PagedRow` supplies it, so a new
paged type is a compile error rather than a silently non-advancing cursor.

## The retry policy is a pure function

`retry_for(status, replay, attempt, retry_after)` returns `Retry::{No, Wait, Reauth}`, and
`decode_response` handles the body — extracted from a ~300-line `request_raw` so the policy can be
tested without a server.

## Both pagination branches require forward progress

The `after` branch stops unless the max row id advances; the envelope branch stops when the
server echoes back the same cursor it was handed on a *full* page. Without the latter, an endpoint
that never advances its cursor loops forever, re-fetching the same rows. Note also that
`REPORTING_PAGE_SIZE = 5000` rests on the envelope branch tolerating a server-side cap, **not** on
a documented ceiling: the four patch endpoints declare `pageSize` with no maximum (the
`maximum: 10000` in the spec is on `/queries/logged-on-users`, which this app never calls).

## An unreadable `cursor` is an error, not end-of-pages

`next_cursor` returns `Result<Option<String>>` and bails on a shape it cannot interpret (an object
with no usable `name`, a number, an array). It is only consulted after a page that *returned
rows* — the caller checks `page_len == 0` first — so treating an unknown shape as "finished" ends
the fetch mid-fleet and hands back a partial result that looks complete, understating every
compliance number derived from it. This mirrors the `results`-not-an-array arm, which has always
bailed.

## The 5xx and connect arms are `Idempotent`-only

A reporting pull is dozens of *sequential* cursor pages, so a gateway 502 on a late page used to
discard every page already accumulated — 5xx is the most common transient failure on that path,
far more so than 429. But a 5xx on an acting POST is exactly the ambiguity
`ReplaySafety::ActOnce` exists for (the gateway may have failed *after* the job reached the device
queue), so writes still fail through to `JobState::Unknown` and are polled, never replayed.
429/401 stay replayable for both.

## reqwest's default features are off, so every one it drops must be re-added explicitly

`default-features = false` (`src-tauri/Cargo.toml`) is there to pin TLS to rustls, but it also
drops `charset`, `http2`, and `system-proxy` — and `gzip` was never on. Uncompressed six-figure
JSON feeds and a fresh TLS handshake per concurrent fetch were both silent consequences of that
one line. If you touch the feature list, keep `gzip`, `http2`, `system-proxy`, `charset`. The
manifest comment explains each.
