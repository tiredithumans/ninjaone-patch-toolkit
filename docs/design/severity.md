# Severity: two vocabularies on one field

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/model.rs` (`Severity`), `src-tauri/src/rows/rollups.rs` (`SeverityCounts`),
`src-tauri/src/report.rs`, `web-rs/src/types.rs`, `web-rs/src/app/charts.rs`,
`web-rs/src/app/util/`, `web-rs/src/app.rs` (`SEVERITY_OPTIONS`), `web-rs/styles.css`,
`web-rs/src/demo.rs`.

## The feeds mix two vocabularies

The feeds mix uppercase MSRC values (`CRITICAL`/`IMPORTANT`/`OPTIONAL`/`NONE`) with lowercase
engine values (`critical`/`security`/`optional`/`recommended`/`unknown`), and third-party patches
carry the grade in `impact`, not `severity` (aliased onto `Patch::severity`). `security` and
`recommended` are NinjaOne **classifications, not urgency grades**, so `Severity` models them as
their own variants — bucketing them into MSRC levels would misreport them in the export and
charts. Anything `from_raw` fails to map becomes `Unknown` (rank 0), which both sinks it below
every other patch in the severity sort **and** makes it unreachable from the severity facet; that
is why an unmapped value reads as "those patches don't exist". `SEVERITY_OPTIONS`
(`web-rs/src/app.rs`) must therefore cover the whole vocabulary including `UNKNOWN`. Ranks are
ordered so `Security`/`Recommended` fall **below** `Important`, keeping them out of the
`rank() >= Important.rank()` compliance/SLA rollups.

## Adding a value — the full checklist

1. `from_raw` + `label` + `rank` (`model.rs`).
2. The `SeverityCounts` field **and** `SeverityCounts::BANDS` **and** its `AddAssign` (`rows/`).
3. The `web-rs/src/types.rs` mirror.
4. `SEV_BANDS`/`sum_severity`/`sev_count`/`severity_segments` (`charts.rs`).
5. `SEVERITY_COLORS` (`report.rs`).
6. `sev_class` **and** `sev_ordinal` **and** `severity_raw` (the frontend `util` module).
7. `SEVERITY_OPTIONS` (`web-rs/src/app.rs`).
8. The CSS.
9. `demo.rs`.

The CSS end of this is guarded: the eight band colors are `--sev-*` / `--sev-*-fg` custom
properties defined once on `:root`, and the three rule families (`.sev-*`, `.chart .seg-*`,
`.chart-swatch.seg-*`) `var()` them rather than restating hex values.
`severity_css_defines_every_band` (frontend `util` tests) compiles `styles.css` in with
`include_str!` and fails if a band is missing any of the four — CSS cannot give a compile error,
so that test is the substitute. The three families still exist for a reason: the middle one sets
`fill` and is scoped to `.chart`, so it does nothing for a legend `<span>`.

## `rows::SeverityCounts::BANDS` is the canonical enumeration on the counts side

It pairs each label with a typed accessor, and `total()`, the HTML report's chart, its legend and
its denominator all derive from it — so they cannot disagree about how many bands exist.
`report.rs` contributes only `SEVERITY_COLORS`, whose length is tied to `BANDS.len()` by its array
type (a band without a color is a compile error). The earlier `report.rs` helper matched bands by
**string label** with a `_ => counts.unknown` catch-all, so a renamed band silently reported
Unknown's count and double-counted it into the total. `total_severity_is_the_sum_of_its_bands`
(`rows/`) fails if a field is added to the struct but not to `BANDS`.

## `rows::TableColumn<T>` is the shared table definition

See [compliance.md](./compliance.md#table-headers-come-from-rowstablecolumn-spellings) — the
severity tables and the export/report sheets all render through it.

## This checklist was previously incomplete, and every site it omitted had silently drifted

`report.rs` summed six of eight bands by hand (a `security`/`recommended`-only backlog printed
"No pending patches"; a mixed one overflowed the viewBox), the two `.chart-swatch` rules were
missing (blank legend squares), `.sev-optional`/`.sev-unknown` were missing (both collapsed into
`.sev-none`, so "low priority" and "unmapped" rendered identically), and `sev_ordinal` ranked both
classifications *below* `Optional`. Prefer deriving over enumerating where you can —
`write_severity_chart` now sums via `SEVERITY_BANDS` so its denominator cannot diverge from the
segments it draws, which removes one hand-maintained site from this list entirely.
