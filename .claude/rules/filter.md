---
paths:
  - "src-tauri/src/filter*.rs"
  - "web-rs/src/app/filters.rs"
---

# Filter

- **`prepare()` once per query; `build_rows` re-checks every row against the scope** — the
  install `df` is bandwidth, not the boundary.
- **`organization_ids`/`location_ids`/`role_ids` are multi-select** (empty = all; OR within, AND
  across; `filter::ids` accepts bare or list). `df` grammar: `org=1`, `org in (1, 2)`, token
  `loc`, no `class`. → `docs/design/filter.md#the-three-identity-facets-are-multi-select`

**New filter facet** — a device facet extends `PreparedFilter::device_allowed` (+
`has_identity_scope`, and `patch_filter` if the install `df` honors it); a patch facet is a
client-side `*_allowed()` matched against rows. → `docs/design/filter.md`