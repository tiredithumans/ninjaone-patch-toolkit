# Filter: device scope, the install `df`, and row facets

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/filter.rs` (`FilterParams`, `PreparedFilter`), `src-tauri/src/rows/join.rs`
(`build_rows`).

## Client-side device scope vs server-side install `df` vs client-side row facets

Because devices/current patches are prefetched whole-fleet (see
[query-cache.md](./query-cache.md#whole-fleet-prefetch--client-side-scoping)), **every
device-scope facet** (`org`/`location`/`role`, the coarse OS-type `class`, and the granular
OS-name substring) is matched **client-side** by `PreparedFilter::device_allowed`
(case-insensitive class and OS name), and `has_identity_scope` reports whether any is active. Only
the free-text KB/name search (`search_allowed`, which accepts a `KB` prefix on either side), the
severity facet and the first-seen window are matched per **row**.

Keep the split: a facet that describes a *device* extends `device_allowed` (and so reaches the
device count and every fleet-health rollup); a facet that describes a *patch* is a client-side
`*_allowed()`. The OS-name needle was once row-only while the UI filed it under "Device scope"
and left its chip undimmed on the fleet tabs, so compliance and Needs-Reboot silently covered the
whole fleet.

## `device_allowed` lives on `PreparedFilter`, not on `FilterParams`

`prepare()` lowers the text needles and parses the severities once per query and borrows the
id/class facets, so the device sweep and the row join share one object — a device the scope
excludes cannot reappear as a row. `assemble_result` prepares once and passes it to `build_rows`.

## The three identity facets are multi-select

`organization_ids` / `location_ids` / `role_ids`: empty = every one of them; within a facet the
ids are OR'd, and the facets are AND'd. They deserialize from a bare id *or* a list
(`filter::ids`, which also sorts and dedupes), so presets saved when they were `Option<i64>` still
load as the same scope. `id_clause` normalizes again on the way out, so the emitted `df` is
canonical however the struct was built.

## The `df` grammar is NinjaOne's

Check it against their syntax doc. Single value is `org=<id>` (no spaces around `=`), several are
`org in (1, 2, 3)`, and the location token is **`loc`** — `location` is not a token the grammar
defines, so that clause was either rejected or silently dropped. `class` is omitted entirely (the
`/queries/*` endpoints ignore it).

## The install-history `df` is a bandwidth optimization, not the scope boundary

`build_rows` re-checks every joined row against the client-side scope (`scope_active &&
device.is_none()` → drop), for *all* sources rather than only the node-class facet it once
covered. Install-history rows arrive scoped only by whatever `df` the server chose to honor, and
an unhonored clause is dropped silently — so without this a narrowed query could display rows from
devices the operator had scoped out. With no scope active, orphan patches are still kept.
