---
paths:
  - "src-tauri/src/filter*.rs"
  - "web-rs/src/app/filters.rs"
---

# Filter

- **`prepare()` once per query; `build_rows` re-checks every row against the scope** — the
  install `df` is bandwidth, not the boundary.
- **`organization_ids`/`location_ids`/`role_ids`/`device_group_ids` are multi-select** (empty =
  all; OR within, AND across; `filter::ids` accepts bare or list). `df` grammar: `org=1`,
  `org in (1, 2)`, token `loc`, no `class`, no group. → `docs/design/filter.md#the-id-facets-are-multi-select`
- **Device groups are read per query (`/group/{id}/device-ids`) and fail closed**: no member set
  passed to `with_device_group_members` means no device passes; an unreadable group fails the
  query. → `docs/design/filter.md#device-groups-are-read-per-query-and-fail-closed`
- **A group selection is capped and checked before any read**: over `MAX_GROUPS_PER_QUERY` is
  refused (never truncated), an id missing from a non-empty `/groups` listing fails as "no
  longer exists", and the reads run `GROUP_READ_CONCURRENCY` at a time.
  → `docs/design/filter.md#device-groups-are-read-per-query-and-fail-closed`

**New filter facet** — a device facet extends `PreparedFilter::device_allowed` (+
`has_identity_scope`, and `patch_filter` if the install `df` honors it); a patch facet is a
client-side `*_allowed()` matched against rows. → `docs/design/filter.md`