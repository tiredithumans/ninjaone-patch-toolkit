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
- **An empty page short of the cursor's announced `count`, or a cursor page whose response was
  lost (timeout, 5xx, unreadable 2xx), restarts the scan without a cursor under the shared
  `SCAN_RESTARTS` budget, then an error — never re-send that cursor; a terminal cursor short of
  the count is a warning.** NinjaOne's normal end is a non-empty last page.
  → `docs/design/api-client.md#a-broken-or-lost-cursor-page-restarts-the-scan-then-an-error`
- **reqwest has `default-features = false`; keep `gzip`, `http2`, `system-proxy`, `charset`.**
  → `docs/design/api-client.md#reqwests-default-features-are-off-so-every-one-it-drops-must-be-re-added-explicitly`
- **Verify endpoint shapes/params/enums against the spec, never from memory:**
  `docs/api/ninjaone-surface.md` is the committed digest; the weekly `ninjaone-contract` CI job
  fails when the vendor's spec moves. A fixture must emit the vendor's keys, not the ones the
  code hopes for: build a `DeviceSoftwarePatch` with `model::software_patch_json` (it has **no**
  `kbNumber`).