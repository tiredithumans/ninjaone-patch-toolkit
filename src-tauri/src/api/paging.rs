//! Cursor pagination: both of NinjaOne's paging styles, parsed once into `T`.

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::value::RawValue;
use std::collections::HashSet;
use tracing::{info, warn};

use super::{
    CursorReplayUnsafe, NinjaApiClient, ReplaySafety, cursor_page_may_be_unserved,
    is_cursor_replay_unsafe,
};
use crate::error::truncate_body;
use crate::model::{Device, DeviceHealth, Location, Organization, Patch, Role};

pub(super) const DEFAULT_PAGE_SIZE: u32 = 500;
/// Page size for the high-volume `/queries/*` reporting endpoints (patches and
/// install history). These are cursor-paginated, so a larger page only means
/// fewer *sequential* round trips on a big fleet — the cursor (not the page size)
/// decides when paging stops, so an API that silently caps the page still returns
/// every row (the `Value::Object` envelope branch never compares page length to the
/// requested size). The four patch endpoints declare `pageSize` as a bare
/// `integer/int32` with **no** documented maximum — the `maximum: 10000, default:
/// 1000` this comment used to cite is declared on `/queries/logged-on-users`, which
/// this app never calls — so `5000` rests on the envelope branch tolerating a
/// server-side cap, not on a documented ceiling. That tolerance is the actual
/// safety property here; keep it if you change the value. The `after`-paginated list
/// endpoints stay at `DEFAULT_PAGE_SIZE` — their stop condition compares page length
/// to the requested size, so over-requesting there would end paging early and drop
/// the rest of the fleet.
pub(super) const REPORTING_PAGE_SIZE: u32 = 5000;

/// Hard ceiling on the pages one paginated fetch may request, so an endpoint whose
/// cursor keeps "advancing" forever (an ever-growing offset, ids that never run
/// out) still ends — as an error — instead of growing `all` without bound.
///
/// Sized for the worst honest case, not the typical one: a six-figure feed (up to
/// 999,999 rows) under a server that silently caps every page at 100 rows needs
/// 10,000 pages. At the page sizes actually requested that is 5M rows at
/// [`DEFAULT_PAGE_SIZE`] and 50M at [`REPORTING_PAGE_SIZE`] — far beyond any fleet,
/// so reaching it means the endpoint is misbehaving, not that the fleet is large.
const MAX_PAGES: u32 = 10_000;

/// How many times one cursor scan starts over, and how long to wait before each
/// restart. Two failures share the budget, because they share the one recovery
/// known to be safe: a fresh scan from page 1, with no cursor.
///
/// **An empty page while the cursor's `count` says rows remain.** NinjaOne's normal
/// end of a scan is a *non-empty* last page under a terminal cursor. In the field
/// the OS feed opened with `count=6721`, served 5000 rows, and then answered the
/// very next request — 79 ms later — with zero rows; the loop took that as the end
/// and every rollup ran on 74% of the fleet's OS patches. Asking that cursor again
/// does not recover the scan. It used to be re-read, and the field logs show the
/// cursor is broken once it serves the empty page: the OS feed's re-read was refused
/// with `400 Invalid cursor.`, failing the query, and a third-party feed's re-read
/// carried on but ended 6,959 rows short of its count.
///
/// **A cursor page whose response was lost**, when re-reading it cannot help or did
/// not: a 2xx whose body could not be read (NinjaOne served that page, so a re-read
/// gets the next one), a re-read that failed, a re-read scan that ended off its
/// count, or one lost page more than [`PAGE_REREADS`] allows.
///
/// Either way the rows already paged are dropped — nothing says which of them the
/// new cursor would serve again — and a scan that spends the budget fails like the
/// other stalls. The delay is fixed rather than exponential: the empty page has so
/// far only ever been the second one, so a restart costs one page plus this pause.
/// A re-read waits it out too.
pub(super) const SCAN_RESTARTS: u8 = 2;
const SCAN_RESTART_DELAY: std::time::Duration = std::time::Duration::from_millis(750);

/// How many lost cursor pages one fetch asks for again in place before a lost page
/// starts the scan over instead.
///
/// A timeout or a 5xx may have been lost before NinjaOne served the page, or after.
/// The backend keeps the scan's position under the cursor's stable name, so in the
/// second case the re-read is answered with the *next* page, and the forward-progress
/// guards cannot tell. The cursor's `count` can: every complete scan in the field has
/// ended at exactly its count, so a scan that re-read a page must too, or it starts
/// over. Without an announced count there is nothing to check against, and the page
/// restarts the scan as before.
///
/// Restarting on every lost page made large fetches fail. On 2026-10-26 the
/// 563,255-row software feed (113 pages) lost a page to the 45 s timeout every 19–85
/// pages for half an hour, and each loss threw away everything paged so far, so the
/// third loss in one fetch failed the query. At that rate a scan meets one to three
/// losses; four leaves headroom and still bounds a server that loses every page.
pub(super) const PAGE_REREADS: u8 = 4;

/// The envelope branch's place in one scan: everything a restart throws away.
#[derive(Default)]
struct CursorScan {
    /// The cursor the next request echoes; `None` opens a scan.
    cursor: Option<PageCursor>,
    /// Every cursor handed out so far. Comparing with the previous one only caught
    /// an echo; a cursor that oscillates A -> B -> A defeated it and re-fetched the
    /// same rows forever.
    seen: HashSet<PageCursor>,
    /// The scan's size as the server announced it on the first cursor, when the
    /// endpoint announces one. It is what lets an empty page be told apart from
    /// the end of the feed — see [`SCAN_RESTARTS`].
    announced_total: Option<usize>,
    /// The request in flight re-sends a cursor whose page was lost, so losing it
    /// again starts the scan over rather than re-reading it once more.
    rereading: bool,
    /// This scan re-read a lost page, so it stands only if it ends at exactly
    /// `announced_total` — see [`PAGE_REREADS`].
    reread: bool,
}

/// Throws away a broken cursor scan so the loop can open a fresh one, after
/// [`SCAN_RESTART_DELAY`]. `false` once [`SCAN_RESTARTS`] are spent, and the caller
/// then reports the stall with the rows it still holds.
async fn restart_scan<T>(
    path: &str,
    reason: &str,
    all: &mut Vec<T>,
    scan: &mut CursorScan,
    restarts: &mut u8,
    on_progress: Option<&ProgressFn<'_>>,
) -> bool {
    if *restarts >= SCAN_RESTARTS {
        return false;
    }
    *restarts += 1;
    warn!(
        path,
        rows = all.len(),
        // Bare, as the empty-page warning always printed it; 0 when none was announced.
        total = scan.announced_total.unwrap_or(0),
        restart = *restarts,
        reason,
        "restarting the scan"
    );
    all.clear();
    *scan = CursorScan::default();
    if let Some(report) = on_progress {
        report(0);
    }
    tokio::time::sleep(SCAN_RESTART_DELAY).await;
    true
}

/// Sink for incremental pagination progress: invoked with the cumulative row
/// count after each page is accumulated. Callers that don't stream progress to
/// the UI pass `None`.
pub type ProgressFn<'a> = dyn Fn(usize) + Send + Sync + 'a;

impl NinjaApiClient {
    /// Issues a request and decodes the success body as one page of rows,
    /// deserialized **straight into `T`** rather than through a [`Value`] tree.
    ///
    /// This is the whole reason [`Self::send_with_retry`] is split out of
    /// [`Self::request_raw`]. A whole-fleet third-party patch feed runs to six
    /// figures, and routing it through `Value` meant serde built a
    /// `Map<String, Value>` for every row — allocating a `String` for each of its
    /// ~10 JSON keys — and then walked the tree a second time to produce the
    /// `Patch`. The rows were parsed twice and the intermediate was discarded
    /// immediately. Here the row array is handed to `serde_json` once, as `Vec<T>`.
    ///
    /// For a [`ReplaySafety::CursorPage`] request a body that cannot be read is a
    /// [`CursorReplayUnsafe`]: the server answered 2xx, so it has moved the scan past
    /// the page we failed to read.
    async fn request_page<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &str,
        query: &[(&str, String)],
        replay: ReplaySafety,
    ) -> Result<PageBody<T>> {
        let resp = self
            .send_with_retry(Method::GET, path, query, None, replay)
            .await?;
        match decode_page(resp).await {
            Err(err) if replay == ReplaySafety::CursorPage => {
                Err(err.context(CursorReplayUnsafe {
                    why: "NinjaOne answered the cursor page but it could not be read".into(),
                    served: true,
                }))
            }
            other => other,
        }
    }
    /// Cursor-paginated GET covering NinjaOne's two pagination styles. The
    /// `/queries/*` endpoints return a `{ results, cursor }` envelope (cursor is a
    /// bare string or a `{ name, offset, ... }` object, fed back as `cursor`); the
    /// core list endpoints (`/devices-detailed`, `/organizations`, `/locations`, …)
    /// return a bare array and page via `after=<id>` and `pageSize`, ending when a
    /// page is shorter than `pageSize`. Without the `after` paging a fleet with
    /// more than `pageSize` devices would load only the first page, so the
    /// device-to-patch join would miss every device after the first page.
    ///
    /// The `after` cursor advances by the **maximum** id on a page (not the last
    /// one) so an endpoint that doesn't return ids in ascending order can't stop
    /// short, and ids are de-duplicated so an inclusive-`after` boundary row isn't
    /// counted twice. Forward progress is required (the max id must advance), so a
    /// misbehaving endpoint can't loop forever.
    pub async fn get_paginated<T: DeserializeOwned + PagedRow + Send + 'static>(
        &self,
        path: &str,
        base_query: &[(&str, String)],
    ) -> Result<Vec<T>> {
        self.get_paginated_reporting(path, base_query, DEFAULT_PAGE_SIZE, None)
            .await
    }

    /// Like [`get_paginated`](Self::get_paginated), reporting the cumulative row
    /// count to `on_progress` after each page so a long fetch can stream progress
    /// to the UI.
    pub async fn get_paginated_reporting<T: DeserializeOwned + PagedRow + Send + 'static>(
        &self,
        path: &str,
        base_query: &[(&str, String)],
        page_size: u32,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<T>> {
        let mut all: Vec<T> = Vec::new();
        let mut seen_ids: HashSet<i64> = HashSet::new();
        let mut scan = CursorScan::default();
        let mut after: Option<i64> = None;
        // Reported at every exit. A short read is otherwise indistinguishable from a
        // complete one at every call site above this function, and a whole-fleet feed
        // that stops early understates every number derived from it. The elapsed time
        // rides along so a slow fetch can be read as rows per second from the log.
        let mut pages: u32 = 0;
        let started = std::time::Instant::now();
        // Counted across the whole fetch, not per scan, like `pages`.
        let mut restarts: u8 = 0;
        let mut rereads: u8 = 0;

        loop {
            let mut query: Vec<(&str, String)> = base_query.to_vec();
            query.push(("pageSize", page_size.to_string()));
            if let Some(c) = &scan.cursor {
                // `cursor` is the only paging parameter these endpoints accept — the
                // offset that rides beside the name in the response is server-side
                // state, keyed by that name. See [`PageCursor`].
                query.push(("cursor", c.name.clone()));
            }
            if let Some(a) = after {
                query.push(("after", a.to_string()));
            }

            if pages == MAX_PAGES {
                bail!(
                    "{path} was still paging after {MAX_PAGES} pages ({} rows); stopping rather \
                     than reporting a partial fleet as complete",
                    all.len()
                );
            }
            pages += 1;
            // Only a request that carries a cursor moves server-side state; the one
            // that opens a scan and the `after` pages can be asked for again.
            let replay = if scan.cursor.is_some() {
                ReplaySafety::CursorPage
            } else {
                ReplaySafety::Idempotent
            };
            // Taken as the request goes out, so a re-read is tried once.
            let rereading = std::mem::take(&mut scan.rereading);
            let body = match self.request_page::<T>(path, &query, replay).await {
                Ok(body) => body,
                // A page that may never have been served is asked for again, once,
                // where the count can later show whether it was the page we lost.
                Err(err)
                    if !rereading
                        && rereads < PAGE_REREADS
                        && scan.announced_total.is_some()
                        && cursor_page_may_be_unserved(&err) =>
                {
                    rereads += 1;
                    let reason = format!("{err:#}");
                    warn!(
                        path,
                        rows = all.len(),
                        reread = rereads,
                        reason = reason.as_str(),
                        "re-reading a lost cursor page"
                    );
                    scan.rereading = true;
                    scan.reread = true;
                    tokio::time::sleep(SCAN_RESTART_DELAY).await;
                    continue;
                }
                // Any failure of a re-read restarts the scan, a refusal included:
                // the field has answered a re-sent cursor with `400 Invalid cursor.`
                Err(err) if rereading || is_cursor_replay_unsafe(&err) => {
                    let reason = if rereading {
                        format!("re-reading the lost cursor page failed: {err:#}")
                    } else {
                        format!("cursor page lost: {err:#}")
                    };
                    if restart_scan(
                        path,
                        &reason,
                        &mut all,
                        &mut scan,
                        &mut restarts,
                        on_progress,
                    )
                    .await
                    {
                        continue;
                    }
                    return Err(err.context(format!(
                        "{path} lost a cursor page at {} rows, after starting the scan over \
                         {SCAN_RESTARTS} times; stopping rather than reporting a partial fleet \
                         as complete",
                        all.len()
                    )));
                }
                Err(err) => return Err(err),
            };
            let exit = match body {
                PageBody::Array(items) => {
                    let len = items.len();
                    let mut max_id = after;
                    for item in items {
                        let id = item.row_id();
                        // Skip a row already seen on a prior page (an inclusive
                        // `after` cursor re-returns the boundary row).
                        if let Some(id) = id
                            && !seen_ids.insert(id)
                        {
                            continue;
                        }
                        if let Some(id) = id {
                            max_id = Some(max_id.map_or(id, |m| m.max(id)));
                        }
                        all.push(item);
                    }
                    if let Some(report) = on_progress {
                        report(all.len());
                    }
                    // A short page is the last page. Otherwise advance the cursor to
                    // the largest id seen; stop if it can't move forward (no id, or
                    // no new rows) so a misbehaving endpoint can't loop forever.
                    if len < page_size as usize {
                        "short page"
                    } else {
                        match max_id {
                            Some(id) if Some(id) != after => {
                                after = Some(id);
                                continue;
                            }
                            // A *full* page that cannot move the `after` cursor — no
                            // ids on the rows, or none newer than the ones already
                            // seen — is a stalled scan, not the end of one. Reported
                            // rather than returned: `all` holds a partial fleet here,
                            // and handing it back as `Ok` is what makes an undercount
                            // invisible.
                            _ => bail!(
                                "{path} returned a full page of {len} rows that did not advance \
                                 the `after` cursor; stopping at {} rows rather than reporting a \
                                 partial fleet as complete",
                                all.len()
                            ),
                        }
                    }
                }
                PageBody::Envelope {
                    results,
                    cursor: next,
                } => {
                    let page_len = results.len();
                    all.extend(results);

                    if let Some(report) = on_progress {
                        report(all.len());
                    }
                    if scan.announced_total.is_none() {
                        scan.announced_total = cursor_count(next.as_ref());
                        log_cursor_opened(path, page_len, next.as_ref());
                    }
                    if rereading {
                        // Whether NinjaOne answered with the lost page or the one after
                        // it shows in the offsets; the count check below acts on it.
                        info!(
                            path,
                            rows = all.len(),
                            page_rows = page_len,
                            sent_offset = ?scan.cursor.as_ref().and_then(|c| c.offset),
                            next_offset = ?next.as_ref().and_then(|c| c.get("offset")),
                            "re-read a lost cursor page"
                        );
                    }
                    // No rows on this page means the cursor is exhausted even if the
                    // server echoes a stale token — stop to avoid a loop. Checked
                    // *before* the cursor is interpreted, so a terminal
                    // `{"cursor": {}}` ends the fetch rather than tripping the
                    // malformed-shape error below.
                    //
                    // Unless the server itself said more rows exist. Then the cursor is
                    // broken and the scan starts over under a fresh one, a couple of
                    // times; one that keeps breaking is reported like any other stall,
                    // since the rows in hand are a partial fleet.
                    if page_len == 0 {
                        if let Some(total) = scan.announced_total
                            && all.len() < total
                        {
                            if restart_scan(
                                path,
                                "empty page before the announced count",
                                &mut all,
                                &mut scan,
                                &mut restarts,
                                on_progress,
                            )
                            .await
                            {
                                continue;
                            }
                            bail!(
                                "{path} returned an empty page at {} of the {total} rows its cursor \
                                 announced, after starting the scan over {SCAN_RESTARTS} times; \
                                 stopping rather than reporting a partial fleet as complete",
                                all.len()
                            );
                        }
                        "empty page"
                    } else {
                        match next_cursor(next.as_ref())? {
                            // Forward progress is required here for the same reason the
                            // `after` branch above requires it: an endpoint that echoes
                            // the cursor it was handed alongside a *full* page would
                            // otherwise loop forever, re-fetching the same rows and
                            // growing `all` without bound. The array branch was hardened
                            // against exactly this; the envelope branch stopped only on
                            // an empty page.
                            //
                            // Compared as a whole rather than by `name`: NinjaOne's cursor
                            // is a stable handle plus an advancing `offset`, so matching on
                            // the name alone read an advancing scan as a stalled one and
                            // cut every feed off after its second page. See [`PageCursor`].
                            Some(c) if Some(&c) == scan.cursor.as_ref() => bail!(
                                "{path} returned a full page of {page_len} rows alongside the very \
                                 cursor it was handed ({c:?}); stopping at {} rows rather than \
                                 reporting a partial fleet as complete",
                                all.len()
                            ),
                            // The same stall one step removed: a cursor this scan already
                            // visited re-fetches rows already in `all`, and then cycles.
                            Some(c) if scan.seen.contains(&c) => bail!(
                                "{path} returned a cursor it had already handed out on an earlier \
                                 page ({c:?}); stopping at {} rows rather than reporting a partial \
                                 fleet as complete",
                                all.len()
                            ),
                            Some(c) => {
                                scan.seen.insert(c.clone());
                                scan.cursor = Some(c);
                                continue;
                            }
                            None => {
                                // A warning, not an error: the count was taken when the
                                // scan opened, and on a live tenant a patch installed
                                // mid-scan leaves the feed one row shorter than announced.
                                // The server ending the scan under a terminal cursor is
                                // its own statement that this is the end; the shortfall
                                // is recorded so a real undercount is visible in the log.
                                // A re-read scan is held to its count below instead.
                                if let Some(total) = scan.announced_total
                                    && all.len() < total
                                    && !scan.reread
                                {
                                    warn!(
                                        path,
                                        rows = all.len(),
                                        total,
                                        "cursor exhausted short of the announced count"
                                    );
                                }
                                "cursor exhausted"
                            }
                        }
                    }
                }
                PageBody::Empty => "empty body",
            };
            // Every end of a scan passes here, so a re-read scan cannot slip out short
            // (or long) of its count through any of them. Off by any amount is a
            // restart: the page a re-read skips is a whole page of rows.
            if scan.reread
                && let Some(total) = scan.announced_total
                && all.len() != total
            {
                let reason = format!(
                    "the scan re-read a lost page and ended at {} of its {total} rows",
                    all.len()
                );
                if restart_scan(
                    path,
                    &reason,
                    &mut all,
                    &mut scan,
                    &mut restarts,
                    on_progress,
                )
                .await
                {
                    continue;
                }
                bail!(
                    "{path} re-read a lost page and ended at {} of the {total} rows its cursor \
                     announced, after starting the scan over {SCAN_RESTARTS} times; stopping \
                     rather than reporting a partial fleet as complete",
                    all.len()
                );
            }
            info!(
                path,
                rows = all.len(),
                pages,
                elapsed_s = started.elapsed().as_secs(),
                exit,
                "paged"
            );
            return Ok(all);
        }
    }
}

/// Row identity for the `after`-paginated list endpoints.
///
/// `get_paginated` advances its cursor by the largest `id` on a page and
/// de-duplicates the inclusive boundary row, which used to read `item["id"]` off the
/// intermediate `Value`. With the rows deserialized straight into `T` that field is
/// no longer reachable generically, so the types say what their id is — which also
/// makes it a compile error for a new paged type to forget.
///
/// `None` means "this row carries no id", which is the honest answer for a patch
/// record. It cannot move the cursor, so a bare-array endpoint returning such rows
/// stops after one full page exactly as it did before — the reporting feeds that
/// return patches are cursor-enveloped and never take that branch.
pub trait PagedRow {
    fn row_id(&self) -> Option<i64>;
}

impl PagedRow for Device {
    fn row_id(&self) -> Option<i64> {
        Some(self.id)
    }
}

impl PagedRow for Organization {
    fn row_id(&self) -> Option<i64> {
        Some(self.id)
    }
}

impl PagedRow for Location {
    fn row_id(&self) -> Option<i64> {
        Some(self.id)
    }
}

impl PagedRow for Role {
    fn row_id(&self) -> Option<i64> {
        Some(self.id)
    }
}

impl PagedRow for DeviceHealth {
    /// `/queries/device-health` pages by cursor; `deviceId` is not a row id.
    fn row_id(&self) -> Option<i64> {
        None
    }
}

impl PagedRow for Patch {
    /// The `/queries/*` patch feeds carry no row id — they page by cursor.
    fn row_id(&self) -> Option<i64> {
        None
    }
}

/// One decoded page of a paginated response, with its rows already in their final
/// type.
#[derive(Debug)]
pub(super) enum PageBody<T> {
    /// A bare JSON array — the `after`-paginated list endpoints
    /// (`/devices-detailed`, `/organizations`, `/locations`, `/roles`).
    Array(Vec<T>),
    /// The `{ results, cursor }` envelope — the `/queries/*` reporting endpoints.
    /// The cursor stays a [`Value`] because it is one small field per page, so
    /// nothing is gained by typing it and [`next_cursor`] already reads every shape
    /// NinjaOne sends.
    Envelope {
        results: Vec<T>,
        cursor: Option<Value>,
    },
    /// `204`, an empty body, or a literal `null` — no rows and no cursor.
    Empty,
}

/// The envelope's own two fields, left **unparsed**.
///
/// [`RawValue`] borrows the original bytes rather than building a tree, so the
/// shape checks below cost nothing and `results` is handed to serde exactly once,
/// as `Vec<T>`.
#[derive(Deserialize)]
struct RawEnvelope<'a> {
    #[serde(borrow, default)]
    results: Option<&'a RawValue>,
    #[serde(borrow, default)]
    cursor: Option<&'a RawValue>,
}

/// Reads a successful response as one page of `T`.
///
/// The parse runs off the runtime. A reporting page is up to 5000 rows of JSON —
/// megabytes that `serde_json` walks in one go — and it ran inline on a tokio
/// worker. A cold query's five whole-fleet fetches share one task under a single
/// `join!`, so each parse stalled the four sibling fetches outright, and the worker
/// it held served neither the job poller nor the IPC commands queued on it.
/// CPU-bound work goes on `spawn_blocking`, per the backend-core rule; the thread
/// hop costs microseconds against a page's round trip, so even the small lookup
/// pages take it rather than carry a size cutoff.
async fn decode_page<T: DeserializeOwned + Send + 'static>(
    resp: reqwest::Response,
) -> Result<PageBody<T>> {
    if resp.status() == StatusCode::NO_CONTENT {
        return Ok(PageBody::Empty);
    }
    let bytes = resp.bytes().await.context("read body")?;
    tokio::task::spawn_blocking(move || parse_page(&bytes))
        .await
        .context("page decode task failed")?
}

/// Decides which of NinjaOne's two pagination shapes a body is and deserializes it.
///
/// Dispatching on the first non-whitespace byte rather than on `Content-Type`
/// matches what [`decode_response`] already tolerated: some endpoints return JSON
/// without a JSON content type, and a body that isn't JSON at all (a proxy's HTML
/// error page) falls through to the same "unexpected shape" error it did before.
pub(super) fn parse_page<T: DeserializeOwned>(bytes: &[u8]) -> Result<PageBody<T>> {
    let trimmed = bytes.trim_ascii();
    match trimmed.first() {
        None => Ok(PageBody::Empty),
        Some(b'[') => Ok(PageBody::Array(
            serde_json::from_slice(trimmed).context("deserialize page item")?,
        )),
        Some(b'{') => {
            let env: RawEnvelope =
                serde_json::from_slice(trimmed).context("decode paginated envelope")?;
            let Some(results) = env.results else {
                bail!("paginated response missing `results`");
            };
            // `results` must be an array. A non-array (string/object/number) is a
            // malformed envelope, not an empty page — fail loudly rather than
            // silently treating it as zero rows and stopping, which would return a
            // truncated fleet as if it were complete. Checked on the raw slice so the
            // message names the shape that arrived rather than serde's type error.
            let raw = results.get();
            if !raw.trim_start().starts_with('[') {
                bail!(
                    "paginated `results` was not an array: {}",
                    truncate_body(raw)
                );
            }
            Ok(PageBody::Envelope {
                results: serde_json::from_str(raw).context("deserialize page item")?,
                cursor: env
                    .cursor
                    .map(|c| serde_json::from_str::<Value>(c.get()))
                    .transpose()
                    .context("decode cursor")?,
            })
        }
        Some(b'n') if trimmed == b"null" => Ok(PageBody::Empty),
        _ => bail!(
            "unexpected paginated body shape: {}",
            truncate_body(&String::from_utf8_lossy(trimmed))
        ),
    }
}

/// The scan's total row count as the cursor announces it, when it does.
///
/// The spec types `count` as an `int32` and says nothing about what it measures; the
/// field logs settled it: a 6,721-row OS feed and a 559,210-row software feed both
/// opened with exactly their final row count in `count`, on every page. A missing,
/// non-integer or negative value is simply "not announced", and the loop then
/// behaves as it did before the field was read at all.
pub(super) fn cursor_count(cursor: Option<&Value>) -> Option<usize> {
    let Some(Value::Object(obj)) = cursor else {
        return None;
    };
    obj.get("count")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

/// Logs what the server said about the scan it just opened, verbatim.
///
/// NinjaOne's cursor object carries `count` and `expires` beside the `name` and
/// `offset` the loop reads. `expires` is epoch seconds and sat 120 s after the open
/// on every cursor seen so far; `count` is the scan's total ([`cursor_count`]).
/// Logged raw rather than interpreted, so a change in either shows as itself. A
/// six-figure feed is a hundred sequential pages, and a failure deep into one cannot
/// otherwise be told apart from a cursor that stopped being valid partway through —
/// nor can "the server capped the page" be told from "the fleet is that size"
/// without the first page's actual length next to the size requested.
fn log_cursor_opened(path: &str, first_page_rows: usize, cursor: Option<&Value>) {
    let Some(Value::Object(obj)) = cursor else {
        return;
    };
    info!(
        path,
        first_page_rows,
        count = ?obj.get("count"),
        expires = ?obj.get("expires"),
        "cursor opened"
    );
}

/// One page-to-page cursor: the token the next request echoes back, plus the
/// position that rides alongside it.
///
/// NinjaOne's `/queries/*` cursor is `{ name, offset, count, expires }`, and those
/// endpoints accept exactly one paging parameter — `cursor`, documented as "Cursor
/// name". The position therefore lives *server-side*, keyed by that name, which
/// makes `name` a stable handle for the whole scan rather than a per-page token.
///
/// That is why forward progress is measured against the **pair**. Comparing `name`
/// alone made an advancing scan look like a stalled one, so the loop stopped at its
/// second page and handed back 2 × `pageSize` rows as if they were the whole feed —
/// invisible on a short OS feed, and a ~10x undercount on a six-figure third-party
/// one, on every surface that reads it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct PageCursor {
    /// Echoed back as `cursor`; the only paging parameter these endpoints take.
    pub(super) name: String,
    /// The server's position within the scan. `None` for a bare-string cursor,
    /// which carries no position and so can only signal progress by changing.
    pub(super) offset: Option<i64>,
}

/// Extracts the next-page cursor from a `cursor` field that may be a string or an
/// object `{ "name": "...", "offset": N }`.
///
/// `Ok(None)` means "no more pages"; `Err` means the cursor is a shape we cannot
/// interpret. The distinction matters because this is only consulted after a page
/// that *did* return rows, so an uninterpretable cursor is a fetch that stops
/// early — and the caller has no way to tell a truncated fleet from a complete
/// one. Reporting a partial fleet as complete understates every compliance number
/// derived from it. This arm used to return `None` and stop silently.
///
/// It fails at once even on a cursor page, where a malformed `results` restarts
/// the scan first: that one is a body that would not decode, which cannot cheaply
/// be told apart from a page truncated in transit, while a cursor of an unknown
/// shape came in a body that decoded whole — the server's deliberate answer, which
/// every fresh scan would repeat.
pub(super) fn next_cursor(value: Option<&Value>) -> Result<Option<PageCursor>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let named = |name: &str, offset: Option<i64>| {
        (!name.is_empty()).then(|| PageCursor {
            name: name.to_string(),
            offset,
        })
    };
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(named(s, None)),
        Value::Object(obj) => match obj.get("name") {
            // `offset` is read for the forward-progress check only — it is never
            // sent back, because these endpoints have no `offset` parameter. A
            // missing or non-integer offset simply leaves the name to carry the
            // comparison on its own, exactly as a bare-string cursor does.
            Some(Value::String(s)) => Ok(named(s, obj.get("offset").and_then(Value::as_i64))),
            // An object cursor whose `name` is absent or not a string is not
            // "finished" — it is a shape this client does not understand.
            other => bail!(
                "cursor object has no usable `name`: {}",
                truncate_body(
                    &serde_json::to_string(other.unwrap_or(&Value::Null)).unwrap_or_default()
                )
            ),
        },
        other => bail!(
            "unexpected cursor shape: {}",
            truncate_body(&other.to_string())
        ),
    }
}
