//! Cursor pagination: both of NinjaOne's paging styles, parsed once into `T`.

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::value::RawValue;
use std::collections::HashSet;
use tracing::info;

use super::{NinjaApiClient, ReplaySafety};
use crate::error::truncate_body;
use crate::model::{Device, Location, Organization, Patch, Role};

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
    async fn request_page<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<PageBody<T>> {
        let resp = self
            .send_with_retry(Method::GET, path, query, None, ReplaySafety::Idempotent)
            .await?;
        decode_page(resp).await
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
    pub async fn get_paginated<T: DeserializeOwned + PagedRow>(
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
    pub async fn get_paginated_reporting<T: DeserializeOwned + PagedRow>(
        &self,
        path: &str,
        base_query: &[(&str, String)],
        page_size: u32,
        on_progress: Option<&ProgressFn<'_>>,
    ) -> Result<Vec<T>> {
        let mut all: Vec<T> = Vec::new();
        let mut seen_ids: HashSet<i64> = HashSet::new();
        let mut cursor: Option<PageCursor> = None;
        let mut after: Option<i64> = None;
        // Reported at every exit. A short read is otherwise indistinguishable from a
        // complete one at every call site above this function, and a whole-fleet feed
        // that stops early understates every number derived from it.
        let mut pages: u32 = 0;

        loop {
            let mut query: Vec<(&str, String)> = base_query.to_vec();
            query.push(("pageSize", page_size.to_string()));
            if let Some(c) = &cursor {
                // `cursor` is the only paging parameter these endpoints accept — the
                // offset that rides beside the name in the response is server-side
                // state, keyed by that name. See [`PageCursor`].
                query.push(("cursor", c.name.clone()));
            }
            if let Some(a) = after {
                query.push(("after", a.to_string()));
            }

            pages += 1;
            match self.request_page::<T>(path, &query).await? {
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
                        info!(path, rows = all.len(), pages, exit = "short page", "paged");
                        return Ok(all);
                    }
                    match max_id {
                        Some(id) if Some(id) != after => after = Some(id),
                        // A *full* page that cannot move the `after` cursor — no ids
                        // on the rows, or none newer than the ones already seen — is
                        // a stalled scan, not the end of one. Reported rather than
                        // returned: `all` holds a partial fleet here, and handing it
                        // back as `Ok` is what makes an undercount invisible.
                        _ => bail!(
                            "{path} returned a full page of {len} rows that did not advance the \
                             `after` cursor; stopping at {} rows rather than reporting a partial \
                             fleet as complete",
                            all.len()
                        ),
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
                    // No rows on this page means the cursor is exhausted even if the
                    // server echoes a stale token — stop to avoid a loop. Checked
                    // *before* the cursor is interpreted, so a terminal
                    // `{"cursor": {}}` ends the fetch rather than tripping the
                    // malformed-shape error below.
                    if page_len == 0 {
                        info!(path, rows = all.len(), pages, exit = "empty page", "paged");
                        return Ok(all);
                    }
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
                        Some(c) if Some(&c) == cursor.as_ref() => bail!(
                            "{path} returned a full page of {page_len} rows alongside the very \
                             cursor it was handed ({c:?}); stopping at {} rows rather than \
                             reporting a partial fleet as complete",
                            all.len()
                        ),
                        Some(c) => cursor = Some(c),
                        None => {
                            info!(
                                path,
                                rows = all.len(),
                                pages,
                                exit = "cursor exhausted",
                                "paged"
                            );
                            return Ok(all);
                        }
                    }
                }
                PageBody::Empty => {
                    info!(path, rows = all.len(), pages, exit = "empty body", "paged");
                    return Ok(all);
                }
            }
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
async fn decode_page<T: DeserializeOwned>(resp: reqwest::Response) -> Result<PageBody<T>> {
    if resp.status() == StatusCode::NO_CONTENT {
        return Ok(PageBody::Empty);
    }
    let bytes = resp.bytes().await.context("read body")?;
    parse_page(&bytes)
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
/// derived from it. The sibling `results` handling already bails loudly on a
/// malformed envelope for exactly this reason; this arm used to return `None` and
/// stop silently.
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
