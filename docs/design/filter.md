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

## Device groups are read per query and fail closed

A NinjaOne device group is a saved search. `GET /groups` lists the groups (a non-fatal lookup
leg: a token that cannot list them loses the picker, not every query), but no device record says
which groups it belongs to. So `run_query` reads `/group/{id}/device-ids` for each selected group
on every query, in the same `join!` as the other legs, and unions the results. It never caches
them, because membership moves with the fleet. `assemble_result` hands the set to
`PreparedFilter::with_device_group_members`.

`device_allowed` **fails closed**: with groups selected and no member set supplied, no device
passes, because a scope that cannot be checked must not widen to the whole fleet. A membership
read that fails fails the query, naming the group. The facet is never put in the install `df`,
because the grammar documents no group token, so a group-only scope pulls the tenant's install
history and narrows it client-side, as `class` does. Its `QueryScope` fingerprint part is appended
only when groups are selected, so trend lines recorded before the facet existed still match.

## `device_allowed` lives on `PreparedFilter`, not on `FilterParams`

`prepare()` lowers the text needles and parses the severities once per query and borrows the
id/class facets, so the device sweep and the row join share one object — a device the scope
excludes cannot reappear as a row. `assemble_result` prepares once and passes it to `build_rows`.

## The id facets are multi-select

`organization_ids` / `location_ids` / `role_ids` / `device_group_ids`: empty = every one of them; within a facet the
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

## The absolute install range is a patch-tier facet that is not matched per row

`installed_after` / `installed_before` live on `FilterParams` so presets carry them, but they are
neither a device facet nor a `PreparedFilter` row predicate: they bound the **install-history
fetch** (pushed down as `installedAfter`/`installedBefore` and re-applied in `assemble_result`),
replacing the relative lookback. They sit in the export's patch tier like the lookback, validated by
`FilterParams::install_range` only when an install status is selected. See
[compliance.md](./compliance.md#an-absolute-install-range-replaces-the-lookback).
