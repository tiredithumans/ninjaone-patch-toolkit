# NinjaOne API client: retry and pagination

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/api/mod.rs` (`NinjaApiClient`, retry), `src-tauri/src/api/paging.rs`
(`get_paginated`, `parse_page`, `PagedRow`, `PageCursor`), `src-tauri/Cargo.toml` (reqwest features).
Spec: rendered docs at <https://app.ninjarmm.com/apidocs/?links.active=core>; raw OpenAPI at
<https://app.ninjarmm.com/apidocs-beta/NinjaRMM-API-v2.yaml> (grep it; the SPA can't be scraped).
**Verify endpoint shapes, params, and field/status enums against the spec — never infer them
from endpoint names or memory.**

## Reuse the shared retry + pagination

Every call goes through `NinjaApiClient`: `{base}/api/v2{path}`, bearer auth, retry on timeout
and **5xx** (`Idempotent` requests only, after a 2/4/8 s `backoff` stretched by up to 25% jitter)
/ connect failure (anything but `ActOnce`; connecting has its own 15 s budget inside the 45 s
total, see `build_http_client`) / 429 (honors `Retry-After` in seconds or as an HTTP-date, capped
at `MAX_RETRY_AFTER_SECS` = 60 s — the header is server-controlled, and an uncapped value parked a
dispatch for as long as it asked — plus up to 1 s of jitter on the shared deadline and up to 1 s
more per request as it is released, and pauses **every** request on the
client, see [below](#a-429-pauses-the-whole-client-and-each-attempts-token-is-bound-to-the-urls-tenant))
/ 401 (forces a token refresh). Each attempt's token is bound to the tenant the URL was built for;
a tenant switch mid-request aborts it with `TenantMoved`. A cursor page gets 100 s rather than the
client's 45 s (see [below](#a-cursor-page-waits-longer-than-any-other-request)), and one
whose response was lost is never retried blindly: it is re-read under a count check, or
the scan starts over (see [below](#a-lost-cursor-page-is-re-read-against-the-count-or-the-scan-restarts)). `get_paginated` handles **both** a bare JSON array **and** the
`{ results, cursor }` envelope, where `cursor` may be a string or a `{ name, offset, … }` object;
it stops when a page returns 0 rows even if the server echoes a stale token. Don't hand-roll a
second reqwest/cursor loop.

Only an endpoint the spec gives paging parameters goes through `get_paginated`. `/roles` declares
none (see `docs/api/ninjaone-surface.md`), so it is one `get_json`: paged with `after`, a tenant
with a full page of roles got the same page back for the second request, which the forward-progress
guard below rightly reports as a stall.

## Paginated bodies and row lists are deserialized straight into `T`; everything else goes through `Value`

`send_with_retry` owns the request/retry loop and returns the raw `reqwest::Response`;
`request_raw` decodes it as a `Value` (single-shot GETs, acting POSTs — all small bodies) and
`request_page` decodes it as a `PageBody<T>` (`api::paging::parse_page`). The paginated path exists
because a whole-fleet third-party feed runs to six figures, and a `Value` intermediate allocates a
`String` for every JSON key on every row and then walks the tree again to build the `Patch` — the
rows are parsed twice. `parse_page` dispatches on the body's first non-whitespace byte and reads
the `{ results, cursor }` wrapper via `serde_json`'s `RawValue` (hence the `raw_value` feature),
so the shape checks stay explicit and the rows are parsed once. The `after`-paginated branch needs
each row's id, which is no longer reachable generically — `api::paging::PagedRow` supplies it, so a new
paged type is a compile error rather than a silently non-advancing cursor.

The two unpaged row lists, `/jobs` and `/activities`, go through `get_decoded`: the body is read
as bytes and parsed on the blocking pool, its array borrowed as `RawValue`s and each entry decoded
on its own into its final type (`lenient_rows`), so one malformed entry is dropped without hiding
the rest. They used to come through `get_json` as one `Value` tree converted entry by entry, inline
on a tokio worker — and `/jobs` is the whole tenant's running jobs, each carrying a full node record,
re-read every 30 s and at its largest during a patch window.

**A success body that is not JSON is an error.** `decode_response` used to keep any body that did
not parse as a `Value::String`, whole; a proxy's sign-in page answering 200 then reached the toast
and the log in full through serde's "invalid type: string …", past the `truncate_body` cap every
other server text goes through, and on `script/run` it became the job's series uid. Now a blank
body is `Null`, JSON is JSON, a short token without spaces or markup (`MAX_BARE_TOKEN`) is kept
because some tenants answer `script/run` with the bare series uid, and anything else is an error
quoting at most `truncate_body` of it (`a_body_not_labelled_json_is_json_a_token_or_an_error`,
`a_2xx_page_that_is_not_json_is_a_capped_error_but_a_bare_uid_is_kept`). On an acting POST that
error is an `OutcomeUnknown`, like any unreadable 2xx.

## The retry policy is a pure function

`retry_for(status, replay, attempt, retry_after)` returns `Retry::{No, Wait, Reauth}`, and
`decode_response` handles the body — extracted from a ~300-line `request_raw` so the policy can be
tested without a server.

## A 429 pauses the whole client, and each attempt's token is bound to the URL's tenant

Three independent sources send on the one production client at once: a query's seven concurrent
legs, the job poller's `/activities` reads (up to `MAX_FEED_READS_IN_FLIGHT`), and the `/jobs`
poll. A rate limit is the tenant's, but a 429 used to pause only the request that drew it, so the
others kept sending into it and drew 429s of their own. `NinjaApiClient` now holds a shared
`paused_until` deadline (every clone, `with_send_guard` included, shares it): every 429 — the one
that exhausts a request's retries too — extends it to now + the capped delay + jitter, never
shortening it, and every attempt sits it out before it asks for a token or the send guard. The
wait is not an attempt. The backoff and the 429 wait are both jittered so the legs a burst failed
together do not come back together (`a_429_pauses_every_request_the_client_sends`,
`the_429_that_ends_a_request_still_pauses_its_siblings`, `backoff_is_jittered_by_at_most_a_quarter`).

**The 429 jitter is per request, not only on the deadline.** The deadline is one instant for every
waiter, so jitter added to it alone moved the burst without spreading it: a query's legs, the
poller's reads and the `/jobs` poll all re-sent in the same millisecond. Each request now sleeps
its own `rate_limit_jitter()` once the pause releases it, and checks the pause again after, so a
429 drawn meanwhile is still sat out (`requests_the_pause_releases_do_not_all_send_at_once`).

`send_with_retry` builds the URL once but asked for a token on every attempt, and
`apply_settings` swaps the host and clears the tokens under one write lock. A read waiting out a
backoff or a 429 across a tenant switch therefore refreshed for the **new** tenant and sent that
bearer token to the **old** host; the old host's 401 then invalidated the new tenant's live token
and forced another refresh grant, up to `MAX_RETRIES` times per request. Writes were covered by the
send guard; reads were not. The request now captures `tenant_stamp()` with its URL and asks
`AuthState::access_token_for(&stamp)` each attempt, which fails with `TenantMoved` once the tenant
has moved (see [auth.md](./auth.md#sign-out-sticks-the-session-generation)). The request is
abandoned without a retry and without invalidating anything; an `ActOnce` request also carries
`SendRefused`, since every earlier attempt was a rejection
(`a_tenant_switch_during_a_429_wait_abandons_the_request`).

## Both pagination branches require forward progress — and neither may stop quietly

The `after` branch stops unless the max row id advances; the envelope branch stops unless the
**whole** cursor advances. Without those, an endpoint that never advances its cursor loops forever,
re-fetching the same rows.

**Compare the whole cursor, never its `name`.** NinjaOne's `/queries/*` cursor is
`{ name, offset, count, expires }`, and those endpoints accept exactly one paging parameter —
`cursor`, documented as "Cursor name". The position therefore lives server-side keyed by that name,
which makes `name` a *stable handle for the whole scan* rather than a per-page token. `next_cursor`
returns a [`PageCursor`] carrying the `offset` alongside the name for exactly this reason: reading
the cursor as its name alone made page 2 compare equal to page 1, tripped the guard, and returned
`2 × pageSize` rows as the whole feed. That is invisible on an OS feed short enough never to reach
page 2, and roughly a 10x undercount on a six-figure third-party one — on the Patches table, the
compliance rollups, both exports and the charts at once, since all of them read the same cached
vector. Every cursor fixture in the suite changed the name per page, which is why CI could not see
it; `a_stable_cursor_name_with_an_advancing_offset_is_progress_not_a_stall` pins the real shape.

**"Advances" means "never seen before in this scan", not "differs from the last one".** The guard
used to compare only with the previous cursor, so one that oscillated A → B → A passed every check
and re-fetched the same pages forever, growing the row vector without bound. Every cursor the scan
has been handed is now kept, and a revisited one bails like an echo
(`a_cursor_that_oscillates_is_an_error_not_an_endless_scan`). Behind both guards sits `MAX_PAGES`
(10,000): a cursor that keeps genuinely changing forever still ends, as an error. It is sized for a
999,999-row feed under a server that silently caps pages at 100 rows, so reaching it means the
endpoint is misbehaving, not that the fleet is large.

**A stall is an error, not a short read.** When the cursor genuinely does not advance, the loop
bails rather than returning `Ok(all)`. The rows in hand are a partial fleet, and handing them back
as success is indistinguishable from a complete fetch at every call site above — the same reasoning
the unreadable-cursor arm below already applies. The exits that *are* legitimate ends of paging log
`path`, `rows`, `pages` and which arm fired at `info`, so a fetch's size is recoverable from the
rolling log when a number needs reconciling against NinjaOne's own reports.

Note also that `REPORTING_PAGE_SIZE = 5000` rests on the envelope branch tolerating a server-side
cap, **not** on a documented ceiling: the four patch endpoints declare `pageSize` with no maximum
(the `maximum: 10000` in the spec is on `/queries/logged-on-users`, which this app never calls).

## A lost cursor page is re-read against the count, or the scan restarts

NinjaOne's cursor object carries `count` beside `name` and `offset`. The spec says nothing about
what it measures; the field logs did: a 6,721-row OS feed and a 559,210-row software feed both
opened with exactly their final row count in `count`. The normal end of a scan is a *non-empty*
last page under a terminal cursor (`exit="cursor exhausted"`), not an empty page.

Then one day the OS feed opened with `count=6721`, served 5000 rows, and answered the next request
— 79 ms later — with zero rows. The envelope branch took any empty page as the end, so the fetch
returned 5000 rows as the whole feed and every rollup ran on 74% of the OS patches; the next
query, two minutes later, got all 6,721.

The first fix re-sent the same cursor name after a short pause, on the theory that the position is
server-side state and the page could simply be asked for again. The field logs disproved it: once
a cursor serves that empty page it is broken. On the OS feed the re-read was refused with
`400 Invalid cursor.`, failing the query; on the software feed it carried on, ended 6,959 rows
short of its `count`, and was stored as complete with only a warning. Every occurrence so far has
been the second page, within a few hundred milliseconds of the open.

`get_paginated_reporting` therefore keeps the first cursor's `count` (`cursor_count`) and, on an
empty page while `all.len() < count`, **starts the scan over** after a short fixed pause
(`SCAN_RESTARTS` × `SCAN_RESTART_DELAY`, via `restart_scan`): no `cursor` parameter, the rows
already paged dropped (nothing says which of them the new cursor would serve again), and the count
re-read from the new cursor. A scan that keeps breaking is reported like the other stalls.

A **lost cursor page** — a request carrying a cursor that timed out, drew a 5xx, or came back as
a 2xx whose body could not be read — fails with the `CursorReplayUnsafe` type (see
[below](#the-timeout-and-5xx-arms-are-idempotent-only)) rather than being retried in place by the
retry loop. What the pagination loop does next depends on whether NinjaOne can have served it.

A timeout or a 5xx may have been lost before the backend served the page, so the loop **re-reads
it once**: the same cursor, after `SCAN_RESTART_DELAY`. If the backend had in fact served it, the
re-read is answered with the page after it and the forward-progress guards cannot tell — but the
count can. Every complete scan in the field has ended at exactly its announced `count`, so a scan
that re-read a page (`CursorScan::reread`) stands only if it ends at exactly that count, through
any of its exits; off by any amount, it starts over under `SCAN_RESTARTS`. A re-read is tried
once: if it fails in any way, a `400 Invalid cursor.` included, the scan starts over.
`PAGE_REREADS` (4) caps re-reads per fetch, and with no announced count there is nothing to check
against, so the page restarts the scan. An unreadable 2xx was served by definition, so it restarts
at once. Each re-read is logged at `warn` before it is sent and at `info` once answered, with the
offset sent and the offset returned, so the log shows whether NinjaOne answered with the lost page.

This replaced restarting on every lost page, which made large fetches fail. On 2026-10-26 the
563,255-row software feed (113 pages of 5000) lost a page to the client's 45 s timeout every 19–85
pages for about half an hour. Each loss threw away everything paged so far, so the third loss in
one fetch failed the query; one fetch got through only after two restarts. Re-paging also
multiplied the pages each fetch needed, and every page was another chance to lose one.

A restart is logged at `warn` with its reason, and `pages` is not reset, so `MAX_PAGES` still
bounds the whole fetch. An empty page at or
past the count, or from an endpoint that announces none, ends the scan exactly as before
(`an_empty_page_at_or_without_an_announced_count_ends_the_scan`).

A **terminal cursor** short of the count is only a warning
(`an_exhausted_cursor_short_of_the_count_is_a_warning_not_an_error`): the count is taken when the
scan opens, and on a live tenant a patch installed mid-scan legitimately leaves the feed a row
shorter than announced. The server ending the scan is its own statement that this is the end; the
shortfall is logged so a real undercount is visible.

The first page also logs the cursor raw (`cursor opened`: `count`, `expires`, and the first page's
actual length next to the size requested). `expires` is epoch seconds and has sat 120 s after the
open on every cursor seen, which bounds how slow a scan may run before the server forgets it.

## A cursor page waits longer than any other request

The re-read and the restart recover a lost page; a cursor page that is merely slow should not be
lost in the first place. NinjaOne advances a scan's position when it *serves* a page, so a page the
client stopped waiting for at its 45 s total timeout and the backend went on to serve was lost by
construction: the re-read is answered with the next page, the scan ends short of its count and
starts over. That timeout is what lost the 2026-10-26 pages, one every 19–85. So a request that
carries a cursor gets `CURSOR_PAGE_TIMEOUT` (100 s) through reqwest's per-request timeout, which
replaces the client's 45 s for that request alone; connecting keeps its own 15 s. 100 s stays under
the 120 s `expires` above, past which the page would find its cursor gone anyway. The request that
opens a scan is an ordinary read: a timeout there is retried in place and loses nothing
(`a_cursor_page_outlasts_the_client_timeout`). Whether a lost page had been served shows in the
`re-read a lost cursor page` log line: a `next_offset` one page past `sent_offset` means it had.

## The whole-fleet third-party feed is paged in location slices that must add up to it

A whole-fleet third-party feed runs to six figures — 563,255 rows, 113 pages of 5000, on the tenant
whose logs drove this — and one cursor scan pages it strictly in sequence. Every page is a chance to
lose one, and a lost page can cost the scan everything before it (see
[above](#a-lost-cursor-page-is-re-read-against-the-count-or-the-scan-restarts)), so the longer the
scan, the likelier the query fails, and the wait is the whole sequence.
`fleet_software_patches_sliced` pages it instead as up to `SOFTWARE_SLICES` (4) concurrent scans,
each scoped by a `loc` clause in `df`. `plan_location_slices` builds them from the cached device
inventory, largest location first onto the lightest slice by device count. Location rather than
organization because it is the finer of the two facets the `/queries/*` endpoints honor, so a
single-organization fleet still splits. A fleet with fewer than two locations, a device with no
location, or a slice that would name more than `MAX_SLICE_LOCATIONS` (200, to keep the URL well under
a proxy's request-line limit) is paged whole.

**The slices replace the feed only if they provably add up to it.** NinjaOne drops a `df` clause
it does not honor without saying so, so a slice could silently be the whole feed. Rows on devices
the inventory does not list sit in no slice at all. So before any paging, `feed_size` reads a
one-row page (`pageSize=1`) of each slice and of the whole feed, and the slices' sizes must sum to
exactly the whole's: the cursor's `count`, or the rows on the page when it is the only one. The
paged rows must then sum to the same total, which catches a device that moved between slices
mid-fetch. They are counted before anything is allocated, and the joined vector is sized from the
rows in hand: sizing it by the announced total let a bogus `count` that the slices summed to abort
the app on one huge allocation (`an_absurd_announced_count_is_never_allocated`). A server's count
never sizes an allocation. Any mismatch, or a size that cannot be read, is logged at `warn` and the feed is paged
whole as before: a few one-row requests and a wasted slice download, never a duplicated or missing
row. Each slice keeps its own re-read and restart budgets, its log lines carry a `slice{n, of}`
span, progress is reported as the slices' sum, and the first slice to fail fails the fetch
(`try_join_all` drops the rest). The OS feed is a single page and is not sliced. The device
inventory is usually warm; on a cold start the third-party leg waits behind the query's own
inventory fetch (the same single-flight gate), and an inventory that cannot be read pages the feed
whole.

## An unreadable `cursor` is an error, not end-of-pages

`next_cursor` returns `Result<Option<String>>` and bails on a shape it cannot interpret (an object
with no usable `name`, a number, an array). It is only consulted after a page that *returned
rows* — the caller checks `page_len == 0` first — so treating an unknown shape as "finished" ends
the fetch mid-fleet and hands back a partial result that looks complete, understating every
compliance number derived from it. The `results`-not-an-array arm refuses to read a malformed
envelope as zero rows for the same reason, but on a cursor page it restarts the scan first and
fails only once `SCAN_RESTARTS` is spent: it is part of decoding the body, and a body that will
not decode cannot cheaply be told apart from one truncated in transit — a page the server already
counts as delivered. A cursor of an unknown shape arrived in a body that decoded whole, so it is
the server's deliberate answer, would come back the same from every fresh scan, and fails at once
rather than spending two full re-pages to reach the same error.

## The timeout and 5xx arms are `Idempotent`-only

5xx is the most common transient failure on a reporting pull, far more so than 429, and a read
that carries no server-side state — a single-shot GET, an `after` page, the request that opens a
cursor scan — is simply asked for again. But a 5xx on an acting POST is exactly the ambiguity
`ReplaySafety::ActOnce` exists for (the gateway may have failed *after* the job reached the device
queue), so writes are never replayed. A connect failure is retried for everything but `ActOnce`.
429/401 stay replayable for all three, but an `ActOnce` request on a client built with
`with_send_guard` asks the guard before every attempt (see
[actions.md](./actions.md#job-state-is-tenant-stamped-the-poller-is-single-claim)).

**A cursor page is `ReplaySafety::CursorPage`, and is not replayed either.** It changes nothing on
a device, but it is not idempotent: the position is server-side state keyed by the cursor's stable
`name` (see [`PageCursor`](#both-pagination-branches-require-forward-progress--and-neither-may-stop-quietly)),
and the backend advances it when it serves the page, not when the client receives it. A 45 s
client timeout, or a 502 from a gateway in front of a backend that already answered, therefore
loses a page the server considers delivered, and re-sending the name is answered with the *next*
page. That retry used to be the 5xx arm's whole purpose — keeping the accumulated pages of a long
pull — and it skipped up to `pageSize` (5000) rows per attempt, 15,000 at `MAX_RETRIES`, with no
error: the forward-progress guards saw the cursor advance, and the end-of-scan shortfall is only a
warning. The suite could not see it because its 5xx fixture changed the cursor name per page. So
the timeout, in-flight, 5xx and unreadable-2xx arms fail a `CursorPage` with `CursorReplayUnsafe`
(`is_cursor_replay_unsafe` sees through `.context()`; its `served` flag marks the 2xx), and the
pagination loop either re-reads the page under the end-of-scan count check or restarts the scan
(see [above](#a-lost-cursor-page-is-re-read-against-the-count-or-the-scan-restarts)). A re-read
without that check is the silent skip this section describes. A connect failure, 429 and 401
never reached the handler and are retried in place.
`a_re_read_that_skipped_the_lost_page_restarts_the_scan` pins it against a server that keeps the
position the way NinjaOne does, and
`a_timed_out_cursor_page_the_server_never_served_is_re_read_in_place` pins the recovery.

## An ambiguous write fails with the `OutcomeUnknown` type, never a phrase

Every `ActOnce` failure after which the action may still have reached NinjaOne fails with
`api::OutcomeUnknown`: a timeout or a connection lost after send (anything but `is_connect()` /
`is_builder()`), a 5xx, and a 2xx whose body cannot be decoded (or, for `script/run`, has a shape
`parse_dispatch_response` does not recognize). The dispatch site checks `api::is_outcome_unknown`
— a downcast that sees through added `.context()` — and records `JobState::Unknown`, which is
polled and never replayed. A 4xx, a connect failure and a refusal before sending stay plain errors
and become `Failed`.

This used to be `msg.contains("may already")`, a phrase only the timeout arm produced, while this
note already claimed a 5xx became `Unknown`. It didn't: a gateway 502 on an apply, a connection
reset mid-POST, or an unreadable 200 was recorded as `Failed` while the device could be installing
— and `Failed` reads as "safe to press Apply again".

## reqwest's default features are off, so every one it drops must be re-added explicitly

`default-features = false` (`src-tauri/Cargo.toml`) is there to pin TLS to rustls, but it also
drops `charset`, `http2`, and `system-proxy` — and `gzip` was never on. Uncompressed six-figure
JSON feeds and a fresh TLS handshake per concurrent fetch were both silent consequences of that
one line. If you touch the feature list, keep `gzip`, `http2`, `system-proxy`, `charset`. The
manifest comment explains each.
