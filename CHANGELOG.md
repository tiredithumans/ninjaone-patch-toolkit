# Changelog

All notable changes to the NinjaOne Patch Toolkit are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow publishes each version's section below as the GitHub release
notes and as the `notes` in the updater manifest, so the in-app **Update available**
window shows this text. When cutting a release, rename `[Unreleased]` to the new
version and start a fresh `[Unreleased]`.

## [Unreleased]

### Changed

- **Filters now read as a vertical list with inline titles.** In the Patch
  section, Type, Status, Severity, Search, and Released each sit on their own row
  with the title (e.g. "Search (KB or name):") inline to the left, aligned into a
  single column. In the Device section, OS Type and OS name contains are stacked
  the same way, while Organization, Location, and Device Role stay in the
  responsive grid.

## [0.5.0] - 2026-06-25

### Added

- **Release date filter in the Patch section.** Narrow patches by when they were
  released — relative presets (last 24 hours / 7 / 30 / 90 days) or a custom
  After/Before date range. The relative window is stored relatively, so a saved
  preset stays "last 7 days" rather than freezing to fixed dates.
- **Presets capture the full query.** Saved presets now also restore the patch
  Type, Status selection, and the install-history window, alongside the existing
  device/OS/severity facets. (Auto-refresh cadence is intentionally excluded.)

### Changed

- **Filters are grouped into Device and Patch sub-sections.** A single Filters
  panel now separates device facets (Organization, Location, Role, OS Type, OS
  name) from patch facets (Type, Status, Severity, Search, Released,
  Installed-within), with Severity directly under Status. Run query, Export, and
  Auto-refresh moved to their own controls row, and the Search field is narrower.

### Fixed

- **The Released filter's date pickers are now dark-themed and their calendar icon
  is visible.** The native date inputs now follow the app's dark color scheme, so
  the calendar popup matches the UI and the (previously dark-on-dark, invisible)
  calendar icon is light and clickable.
- **Pending patches returned no results.** NinjaOne's patch API uses `MANUAL` for
  patches awaiting approval (its UI labels them "Pending"), but the app filtered for
  the literal `PENDING`, which the API never returns — so the Status: Pending filter
  and the Compliance pending counts matched nothing. "Pending" now maps to `MANUAL`,
  and such patches display as "Pending".
- **OS Type filtering returned no patches.** NinjaOne's patch query endpoints
  ignore `class` in the device filter, so selecting an OS Type returned matching
  devices but zero patch rows (for any class). The OS Type facet is now applied
  client-side for patches via the device join.
- **The OS Type facet only appeared after sign-in.** It's a static list, so it now
  loads at startup instead of waiting for the authenticated lookups.

## [0.4.0] - 2026-06-25

### Added

- **Live query progress.** After Run query, the app shows an elapsed-time counter
  and a progress bar that estimates completion from the previous run, plus live
  record counts streamed from the backend ("loaded N records… computing rollups").
  When idle, a "Last run took Ns" hint sets expectations.
- **Severity filter for patches** (Critical / Important / Moderate / Low /
  Optional). NinjaOne's severity is its CVSS-derived band, so this doubles as a
  CVSS-band filter.
- **Patches-tab pagination** replacing the 1000-row display cap, with a clear
  "No patches matched your filters" empty state.

### Changed

- Renamed the Compliance column "Pending Crit/Imp" to "Pending Critical/Important
  Patches" for clarity.

## [0.3.0] - 2026-06-25

A correctness, security, performance, and accessibility sweep from a full review.

### Added

- Sign-in shows progress and blocks concurrent sign-in attempts.

### Fixed

- A 401 retry now actually forces a token refresh instead of resending the same
  rejected token.
- List-endpoint pagination advances by the maximum id seen and de-duplicates the
  boundary row, so an unsorted or inclusive cursor can't drop or double-count
  devices.
- The instance URL is required to be `https` (loopback may use `http`), so OAuth
  tokens and secrets can't be sent in cleartext.
- Offline devices are excluded from the compliance denominator; undated critical
  patches are flagged as aged; millisecond timestamps are normalized.
- Numeric settings/query inputs are validated and clamped; lookup and sign-out
  failures are surfaced; the cached query result is cleared on sign-out and
  instance change; server error bodies are truncated before logging.
- Accessibility scaffolding for tables, toasts, and the update dialog.

### Changed

- Performance: patch rows are built without cloning the current set, sorts use a
  cached key, and the result tables read via borrows instead of cloning. Dropped
  unused dependencies.

## [0.2.4] - 2026-06-25

### Added

- The app version is shown in the Settings panel.

### Fixed

- Organization/Location/Role selects are restored when applying a preset.
- Bare-array list endpoints paginate via `after` so fleets larger than one page
  load fully.

### Changed

- CI gains a `cargo-deny` supply-chain gate; `justfile` recipes are cross-platform.

Earlier releases (≤ 0.2.3) predate this changelog; see the
[GitHub releases](https://github.com/tiredithumans/ninjaone-patch-toolkit/releases).
