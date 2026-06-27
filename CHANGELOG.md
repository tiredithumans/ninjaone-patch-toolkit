# Changelog

All notable changes to the NinjaOne Patch Toolkit are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow publishes each version's section below as the GitHub release
notes and as the `notes` in the updater manifest, so the in-app **Update available**
window shows this text. When cutting a release, rename `[Unreleased]` to the new
version and start a fresh `[Unreleased]`.

## [Unreleased]

## [0.7.1] - 2026-06-27

### Fixed

- **Filters panel on mobile.** Removed the dead space below the "Filters" title
  (the heading is now aligned with the Show/Hide toggle) and made the filter rows
  stack cleanly on narrow / phone-width viewports, where the fixed label and control
  widths previously crowded the screen.

## [0.7.0] - 2026-06-26

### Added

- **Patch failure analysis.** A new **Failures** tab rolls up FAILED installs by
  patch across the fleet — for each failing patch you see the affected-device count,
  the affected devices, and the most recent failure — so a fleet-wide install
  problem (e.g. "this update failed on 23 devices") is visible at a glance instead of
  buried in the detail rows. It populates when the **FAILED** status is part of the
  query; the Excel export gains a matching **Patch Failures** sheet with the complete
  device list.
- **Compliance charts + shareable report.** The **Compliance** tab now leads with
  charts — per-organization compliance bars, a pending-patch severity breakdown, and
  a pending-patch age histogram — above the per-org table. **Export report** saves a
  self-contained HTML executive summary (the same charts plus failure and reboot
  tables) that you can open in any browser and print to PDF for management or
  auditors. Like the Excel export, the report needs a live query in the desktop app.
- **Clearer result scope.** The filter panel now labels its **Device scope** section
  (organization / location / role / OS type — applies to every tab) separately from
  **Patch filters** (status / severity / search / dates — Patches & Failures only),
  and each results tab carries a one-line note on what scope it reflects, so it's
  obvious that the compliance and reboot rollups span the whole device scope rather
  than the narrowed patch list.
- **Live web demo.** The frontend now also runs as a browser-only demo published to
  GitHub Pages at <https://tiredithumans.github.io/ninjaone-patch-toolkit/>, backed
  by a representative fictional fleet so you can explore the UI with no install,
  account, or sign-in (and with no real fleet data exposed). It starts empty and
  lists patches when you press **Run query**, exactly like the real app, and the
  filter controls work against the sample: Organization, Location, Device Role, OS
  Type, status, severity, type, search, and the date windows all filter it just like
  a live query would; Compliance and Needs-Reboot stay representative, narrowing by
  organization. Sign-in, live NinjaOne queries, and Excel export need the native
  backend, so they're disabled in the demo. The downloadable desktop app is
  unaffected — it's the production tool, with no sample-data mode.

### Changed

- **Instant re-filtering between queries.** The device inventory and current patches
  are now fetched once for the whole fleet and cached, so changing the organization,
  location, role, OS type, severity, or patch type and pressing **Run query** re-filters
  the data on the spot instead of making a fresh round trip to NinjaOne every time —
  switching scope feels immediate. Live patch state still stays current: the existing
  **Auto-refresh** dropdown refetches on the cadence you choose, a new **↻ Refresh**
  button pulls fresh data on demand, and a "patch data as of …" stamp shows how current
  the figures are. The device list — which changes rarely — is reused for ~15 minutes,
  so an auto-refresh during a patching operation re-pulls only the patch data that's
  actually moving.
- **Faster Failed/Installed queries.** When you filter to a single install result
  (just **Failed**, or just **Installed**), the toolkit now asks NinjaOne for only
  those records instead of downloading the entire install history for the window and
  discarding the rest. On a healthy fleet — where successful installs vastly
  outnumber failures — a **Failures** query in particular pulls far less data and
  returns noticeably quicker. Large fleets also page through patch reports in bigger
  chunks (fewer round trips), trimming overall query time.

## [0.6.2] - 2026-06-25

### Added

- **Troubleshooting guide** ([docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md))
  covering sign-in hangs and callback-port conflicts, the OAuth 404
  instance/Client-ID mismatch, Native-vs-Web client setup, empty exports, blank
  location names, and keyring issues.

### Changed

- **Settings validation.** An invalid **Callback port** (`0`) or a sub-day
  **Install window** / **SLA** is now rejected with a clear message instead of
  being silently clamped, so a typo surfaces rather than quietly changing your
  configuration.

### Fixed

- **More robust API handling.** A malformed paginated response from NinjaOne now
  surfaces an error instead of silently returning a truncated result set as if it
  were complete, and previously-swallowed locations-fetch and result-cache
  failures are now logged.

## [0.6.1] - 2026-06-25

### Fixed

- **Patch Status: Failed now returns results.** A "Failed" patch is an install
  *result*, which NinjaOne reports in its patch-install history — not in the
  current-patch feed (which lists only patches with no install attempts). The query
  was looking for failed patches in the current feed, where they never appear, so a
  Failed filter always came back empty. Failed patches are now read from the
  install-history endpoints (like Installed), bounded by the same install-history
  lookback window.

## [0.6.0] - 2026-06-25

### Added

- **Collapse the Filters panel.** A Hide/Show toggle in the Filters header folds
  the filter controls away so the results table gets more room. The panel stays
  expanded by default, and Run query plus the results stay available while it's
  collapsed.

### Changed

- **Faster, lighter patch queries on large fleets.** The detail table now loads
  rows a page at a time from the backend instead of transferring the whole result
  set to the UI at once, so large queries (10k+ rows) feel noticeably snappier.
  The fleet-wide patch fetches use larger API pages to cut round-trips, and the
  per-query join, filtering, and Excel export do less redundant work. The rows,
  compliance figures, and export contents are unchanged — only how quickly they
  arrive.

## [0.5.1] - 2026-06-25

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
