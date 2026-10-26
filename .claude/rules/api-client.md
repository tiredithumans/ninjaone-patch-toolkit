---
paths:
  - "src-tauri/src/api/**"
---

# NinjaOne API client

- **Every call goes through `NinjaApiClient`** (`get_paginated` / `request_raw`); retry is the
  pure `retry_for`; paginated bodies parse once via `parse_page` + `PagedRow`. A new endpoint is
  a method on `NinjaApiClient` (`api/<domain>.rs`) using `get_paginated` / `request_raw` — never
  a second reqwest/cursor loop. → `docs/design/api-client.md`
- **Both pagination branches require forward progress, measured against the *whole* cursor** —
  (`name` is a stable handle; the position rides in `offset`). **A stall is an error, not a short
  read; an unreadable cursor is an error, not end-of-pages; timeout/5xx retries are
  `Idempotent`-only (a cursor page is `CursorPage`); connect retries are never `ActOnce`.**
  → `docs/design/api-client.md#both-pagination-branches-require-forward-progress--and-neither-may-stop-quietly`
- **A cursor page lost to a timeout or 5xx under an announced `count` is re-read once (at most
  `PAGE_REREADS` per fetch), and a scan that re-read stands only if it ends at exactly its
  count. Otherwise — an empty page short of the count, an unreadable 2xx, a failed re-read, a
  re-read scan off its count, no count to check — the scan restarts without a cursor under the
  shared `SCAN_RESTARTS` budget, then an error. Never re-send a cursor without that count check;
  a terminal cursor short of the count is otherwise a warning.** NinjaOne's normal end is a
  non-empty last page.
  → `docs/design/api-client.md#a-lost-cursor-page-is-re-read-against-the-count-or-the-scan-restarts`
- **The whole-fleet third-party feed is paged as location slices of the device inventory
  (`fleet_software_patches_sliced`, `plan_location_slices`) only if the slices' one-row
  `feed_size`s, and then their paged rows, sum to exactly the whole feed's; any mismatch pages it
  whole.** Never trust a `df` slice unverified — NinjaOne drops an unhonored clause silently.
  → `docs/design/api-client.md#the-whole-fleet-third-party-feed-is-paged-in-location-slices-that-must-add-up-to-it`
- **A 429 extends the client-wide `paused_until` (shared by every clone, never shortened) and
  every attempt waits it out first; `backoff` and the 429 wait are jittered; `Retry-After` is
  read as seconds or an HTTP-date, capped at `MAX_RETRY_AFTER_SECS`.**
  → `docs/design/api-client.md#a-429-pauses-the-whole-client-and-each-attempts-token-is-bound-to-the-urls-tenant`
- **Each attempt's token is bound to the URL's tenant (`access_token_for(&tenant_stamp)`); a moved
  tenant aborts with `TenantMoved` — no retry, no token invalidation.**
  → `docs/design/api-client.md#a-429-pauses-the-whole-client-and-each-attempts-token-is-bound-to-the-urls-tenant`
- **reqwest has `default-features = false`; keep `gzip`, `http2`, `system-proxy`, `charset`.**
  → `docs/design/api-client.md#reqwests-default-features-are-off-so-every-one-it-drops-must-be-re-added-explicitly`
- **Verify endpoint shapes/params/enums against the spec, never from memory:**
  `docs/api/ninjaone-surface.md` is the committed digest; the weekly `ninjaone-contract` CI job
  fails when the vendor's spec moves. A fixture must emit the vendor's keys, not the ones the
  code hopes for: build a `DeviceSoftwarePatch` with `model::software_patch_json` (it has **no**
  `kbNumber`).