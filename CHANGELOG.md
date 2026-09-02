# Changelog

All notable changes to the NinjaOne Patch Toolkit are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow publishes each version's section below as the GitHub release
notes and as the `notes` in the updater manifest, so the in-app **Update available**
window shows this text. When cutting a release, rename `[Unreleased]` to the new
version and start a fresh `[Unreleased]`.

## [Unreleased]

### Added

- **Rolling diagnostic logs, and a button that reveals them.** `init_tracing` wrote to stdout
  only, which a bundled `.app` launched from Finder or an `.msi` from the Start menu discards
  entirely — so a field bug report arrived with no evidence beyond the write-path audit log.
  A daily-rotating file layer now writes to `logs/` beside `settings.json` (seven days kept), and
  **Settings → Open diagnostics folder** reveals it.
- **The action audit trail is readable in the app.** `action-audit.jsonl` has always been written
  — append-only, redacted, owner-only, before the request goes out — but nothing could read it
  back, so restarting after a batch of reboots left no in-app record that it happened. The
  **Jobs** tab now renders it under *Audit trail*, newest first. It survives a restart and
  **Clear history** (which only clears the live session list) does not touch it.

### Fixed

- **A silent auto-refresh in a grouped view could strand the table on an empty page.** The
  query-completion path sized the page bound from `rows_total` unconditionally and then handed that
  index to the *group* fetch. Grouping only collapses rows, so the row-derived bound is always the
  looser one: with 40,000 rows in 400 groups, a stored page 7 is in range for rows and three pages
  past the end for groups — the grouped table rendered nothing while the pager read "Page 8 of 400".
  Both the pager and the completion path now take their bound from `util::paged_total`, and
  `fetch_groups` re-clamps once the response carries the real total.
- **A grouped view no longer fetches the flat rows behind it.** They are never rendered, so every
  auto-refresh tick spent an extra IPC round trip on them. Switching back to Flat still refetches
  page 0 through `set_group_by`, as before.
- **The audit log was written to a different directory than `settings.json`.** `settings.rs` and
  `actions/audit.rs` each resolved their own `ProjectDirs`, with different qualifiers — on macOS,
  `io.github.tiredithumans.NinjaOnePatchToolkit` versus `ninjaone-patch-toolkit`. The README and
  `docs/TROUBLESHOOTING.md` both said the two sat side by side, and both were wrong; an operator
  looking for their audit trail was sent to the wrong folder. A new `paths` module is now the only
  place the qualifier is written down, pinned by `app_dir_is_single_sourced`. Records written by
  older builds are still read, so no dispatch history is lost on upgrade.

## [0.13.5] - 2026-09-02

### Changed

- **MSRV and the pinned toolchain move to Rust 1.98** (`rust-toolchain.toml`, both crates'
  `rust-version`, the `dtolnay/rust-toolchain` SHA in all five workflows, and the prerequisite
  lines in the README, `CONTRIBUTING.md` and `docs/TROUBLESHOOTING.md`).
- **Dependencies refreshed in both crates.** Backend: `keyring` 4.1 -> 4.2, `open` 5.4.1 -> 5.4.3,
  `tauri-plugin-dialog` 2.7.2 -> 2.7.3, `tauri-plugin-updater` 2.10 -> 2.11, plus 35 transitive
  bumps; the `open` and `tokio` manifest floors were raised to the versions actually resolved
  (5.4, 1.53). Frontend: seven transitive bumps. The screenshot tooling's npm deps were already
  current.
- **The `cargo-audit` ignore list is empty again.** Both quick-xml DoS advisories
  (`RUSTSEC-2026-0194`, `RUSTSEC-2026-0195`) met the revisit condition written next to them —
  `calamine` and `plist` now resolve quick-xml 0.41 — so the entries were dropped rather than left
  to rot.

### Fixed

- Removed nine redundant `use leptos::prelude::*;` imports under `web-rs/src/app/`. Each module
  also globs `use super::*`, which already re-exports `app.rs`'s prelude import; Rust 1.98's
  `unused_imports` detects the shadowing and `web-clippy` runs with `-D warnings`.

## [0.13.4] - 2026-09-01

### Changed

- **Agent instructions split into a contract and design notes.** `AGENTS.md` shrank from 82 KB
  to under 25 KB and now states each rule in a line or two with the file and test that enforce
  it; the rationale and history moved to `docs/design/*.md`, one note per subsystem. The
  staleness hook warns when the contract passes 30 KB.
- **`rows.rs` and the frontend `util.rs` split by concern** (`src-tauri/src/rows/` and
  `web-rs/src/app/util/`), and the large inline test modules of `auth.rs`, `state.rs`,
  `commands/patches.rs`, `rows` and `util` moved to sibling `tests.rs` files. No behavior change.
- **`just verify` no longer runs `web-check`**; `web-clippy` performs the same type-check first.
  CI's frontend job mirrors the chain.
- Skills, `CONTRIBUTING.md` and the PR template point at `just verify` instead of enumerating the
  gate list, and the five skills describe the current code (the `ipc!` macro, the `web-rs/src/app/`
  modules, conditional OAuth scope, the patch-action surface).

### Fixed

- **`command-parity-check.sh` warned on every command** since the `ipc!` macro replaced the quoted
  invoke strings it looked for; it now recognizes the macro form. `.claude/hooks/test.sh` exercises
  every hook against fixed payloads so this cannot silently recur.
- Removed the `list_presets` command, which the frontend never called — the parity hook's first
  true positive once it worked (presets already ride on the settings view and the save/delete
  responses).
- The docs-staleness hook no longer asks for a README re-skim on every internal backend edit; it
  fires only for packaging, prerequisite and auth-setup surfaces.

## [0.13.3] - 2026-09-01

### Fixed

- **A device whose patch sits in the current feed as FAILED, or with no status at all, no longer
  scores as compliant.** The pending rule was an allow list (`MANUAL`, `APPROVED`, or no status), so
  any other value NinjaOne put on a current-feed record — and its own endpoint titles promise
  "Pending, Failed and Rejected" ones — counted as "not pending": the device read 100% compliant and
  the patch appeared in no rollup. It is now an exclude list: everything in the current feed counts
  unless it is `REJECTED` or `INSTALLED`. The same records now show as rows too — a FAILED one under
  the Failed status, an untyped one under Pending — where before the Compliance sheet could count a
  patch the Patches sheet could not show.
- **Compliance no longer counts devices NinjaOne cannot patch.** Switches, printers, hypervisors,
  cloud monitors and other non-Windows/macOS/Linux device classes are online and carry no patch
  records, so they scored as compliant and inflated the headline percentage by exactly the share of
  the fleet that cannot be patched — and, with no OS Type filter, opened an "(unknown)" OS at 100% in
  the by-OS table. They are now excluded from every fleet-health rollup like offline devices are, and
  the scope note on the Compliance tab, the report header and the workbook say how many:
  "Compliance covers online Windows, macOS and Linux devices only (3 offline and 12 non-patchable
  devices excluded)". The About sheet carries the count too.
- **The exports now say which filters reach which sheets.** The About sheet and the report header
  listed Status, Severity, Search and the date windows beside the Compliance and Needs Reboot
  numbers, which ignore them — a workbook filtered to Severity: Critical read as a critical-only
  backlog. Both now print the device-scope filters under "Filters (every sheet)" and the row-only
  ones under "Patch filters (Patches and Patch Failures sheets only)".
- **The Type chip is no longer struck through on the Compliance and Needs Reboot tabs.** Type does
  narrow those numbers (only the patch families the query fetched are in them), and the banner on
  the tab said so while the chip above it said "Ignored on this tab". The Type control also stays
  visible in the Filters panel on those tabs instead of being folded away with the row-only filters.
- **Signing out no longer leaves the previous session's results on screen.** The rows, selection
  and job list stayed rendered after Sign out (and after a fresh sign-in or Re-authorize) while the
  app had already dropped the data behind them, so the next page came back blank and Export
  complained there was nothing to export beside a visible table. A tenant switch now also clears the
  job list and the organization/location/role scope, which belonged to the previous tenant.
- **A dry run no longer flags the results as stale.** Previewing an action with "Dry run" ticked
  (the default) raised the "An action was dispatched — these results predate it" banner and its
  Refresh link forced a whole-fleet refetch, although nothing on any device had changed.
- **"Re-authorize" is available wherever the read-only sign-in is reported.** The link lived only in
  the action bar, which appears only once a query has returned rows; the Settings hint told the
  operator to click a button that was not on the panel. It now appears in Settings, in the Jobs
  tab's note and in the action bar.
- **A scope that names an organization, location or role the app cannot find is shown as such.**
  A preset whose organization has since been deleted, or a scope carried across a tenant switch,
  used to produce zero rows under a chip row reading "No filters — whole fleet". Unresolved ids now
  appear as "#4711 (not found)" in the chips and the scope pickers.
- **The confirmation and update dialogs now take keyboard focus.** Focus stayed on the button under
  the overlay, so Tab walked the covered page and pressing Space re-opened the plan behind the
  dialog. Both dialogs now receive focus when they open, keep Tab inside themselves, and return
  focus to the button that opened them when they close.
- **The install-history lookback is enforced on the app's side as well as the server's.** The
  exports say "Install history since <date>", so a record NinjaOne returned from outside that window
  is now dropped rather than trusted into the Failures table.

## [0.13.2] - 2026-09-01

### Fixed

- **The exported report's severity and age charts now describe the same fleet as its compliance
  sections.** Compliance deliberately excludes offline devices — they report no current patch
  records, so a zero pending count says nothing about them — and the report says so in its header.
  The "Pending patches by severity" and "Pending patch age" charts did not apply that exclusion, so
  they silently counted the excluded devices' backlog directly beneath the sentence promising they
  had been left out, and the difference was invisible on the page. Patches belonging to a device
  that is not in the scoped inventory at all no longer open their own `(unknown)` organization in
  the severity breakdown either. The same correction applies to the in-app Compliance charts, which
  are computed from the same rollups.

### Added

- **Both exports now state when their patch data was fetched**, not just when the file was
  generated. Re-running a filter recomputes over already-fetched data without a new round trip, so a
  file stamped only with its generation time could date the fleet to the moment you pressed the
  button. The HTML report prints both times in its header; the workbook gained an **About** sheet
  that carries them — it previously had no timestamp at all beyond its suggested file name.
- **Both exports now record the filters the query ran under** — organizations, locations, device
  roles, OS type and name, patch type, status, severity, search text and the date windows. Two
  workbooks taken from the same fleet, one narrowed to a single organization and one covering
  everything, were previously indistinguishable once saved, while every number in them described a
  different set of devices. Date windows are written as absolute timestamps so they still mean the
  same thing when the file is read months later, with the relative window ("last 30 days") noted in
  parentheses when that is what you picked.
- **The workbook's Patches sheet has an Offline column.** The compliance sheets state how many
  offline devices were excluded from their totals; the detail sheet now lets you find them and
  reproduce that figure by hand.

## [0.13.1] - 2026-08-20

### Security

- **Signing out now reliably ends the session.** A fleet query can run for minutes, and one that was
  still fetching when you signed out used to write its rows back into the cache *after* the sign-out
  cleared it — leaving the previous operator's patch rows readable, pageable and exportable by
  whoever signed in next on the same instance. The result cache is now version-gated the same way
  the device and patch caches already were, so a query that began before the session ended can no
  longer store after it.
- **Signing in and re-authorizing now clear the previous session too**, not just signing out. Both
  run the full browser flow, so on a shared workstation they routinely hand the app to a different
  operator — who previously inherited the last one's device inventory, patch caches, query results,
  job history and pending action approval.
- **Sign-out reports a failure instead of claiming success** when the stored refresh token could not
  be deleted. It previously always reported a clean sign-out, so a credential left on disk was
  invisible — and the next sign-in would silently reuse it.
- **The sign-in URL is no longer written to the log.** It carried the flow's anti-CSRF `state` value
  — the one thing that distinguishes the real browser redirect from anything else reaching the
  callback port — along with the PKCE challenge and the client id. Only the address is logged now.
- **A plaintext `http://` instance URL in `settings.json` is now upgraded to `https://` on load.**
  The Settings screen already refused one, but nothing enforced it when reading the file back, so a
  hand-edited value survived a restart and sent the access token — and the client secret — in the
  clear.
- **An action approval no longer survives an instance switch.** The confirmation dialog binds the
  approval to the exact devices, patches and options you approved, but not to the NinjaOne instance.
  Changing instance in Settings while the dialog was open let the approval dispatch against the new
  one, on devices you never saw. The approval is now refused instead.
- **Script credentials passed as a flag and a value are now redacted from the action audit log.**
  Only `password=secret` was caught; `-Password secret` — the PowerShell and CLI convention — went
  to disk in cleartext.
- **Exported workbooks and HTML reports are created readable by you only** (on macOS and Linux),
  matching what the action audit log already does. They carry the same fleet data — device names,
  organizations, compliance posture — and were previously written with default permissions.
- **Release builds log at `info` by default** instead of debug. Set `RUST_LOG` to turn detailed
  logging back on when diagnosing a problem.

### Fixed

- **Changing the instance while signing in no longer files the credentials under the wrong tenant.**
  A sign-in waits up to three minutes for the browser; switching instance in Settings during that
  window used to store the tokens the *old* instance issued under the *new* instance's name. Such a
  grant is now discarded with an explanation.
- **Patch state refreshes correctly after every action.** The rule for which caches to drop was
  written twice and the two copies disagreed: an apply could leave "needs reboot" stale for up to
  15 minutes, while a scan — which changes nothing — forced a full device refetch.
- **The Jobs tab and audit log now name the script an "Apply selected patches" action ran.** They
  previously recorded only the action name, and the script is configured in Settings, so there was
  no way to tell which one had run.
- **Failed-patch queries no longer drop install records that omit their own status.** They were
  labelled as installed and then filtered out, so the Failures view could silently under-report.
- **Running a query with no status selected now explains itself** instead of starting a full fleet
  fetch that could only ever return an empty table.

### Changed

- **Exports no longer freeze the patch table.** Saving a workbook or an HTML report used to copy the
  entire cached result — every row — while holding the lock the table's paging also needs, so on a
  large fleet the UI stalled for the length of the copy. Both now take a shared handle instead.
- **Faster queries and action dispatch on large fleets.** The canonical row sort no longer allocates
  two strings per row, the per-organization severity rollup no longer allocates an organization name
  per patch record, and the action audit log is written once per batch off the async runtime rather
  than reopened once per device. Saving settings no longer blocks in-flight API calls behind an OS
  keyring read.
- **Job polling is faster with many actions in flight.** Each pending job's status was fetched one
  after another, so a tick took as long as the sum of them all; they now run together.
- **The patch table re-renders more cheaply**, copying each visible row once instead of twice.

## [0.13.0] - 2026-08-15

### Added

- **Organization, Location and Device Role are now multi-select.** Each is a searchable checkbox
  picker: pick any number, or none for "all". Within a facet the selections are OR'd and the facets
  are AND'd, so "these four sites, servers only" is one query instead of four. The location list
  spans every selected organization and qualifies names that would otherwise collide (three sites
  called "HQ" are now distinguishable), and selecting a location that no longer belongs to any
  selected organization is no longer possible. Presets saved with the old single-select facets still
  load and still mean the same scope.
- **Every compliance surface now states what it covers.** The Compliance tab, the HTML report and
  both workbook compliance sheets carry a sentence naming the excluded offline devices and the patch
  families counted — the two things a bare percentage hides.

### Fixed

- **The location filter was never applied to install-history queries.** The device-filter clause was
  built as `location = <id>`, and `loc` is the only token NinjaOne's filter grammar defines, so the
  clause was rejected or silently ignored. Multi-value scopes now use the documented
  `org in (1, 2, 3)` list form as well.
- **A narrowed query could display rows from devices outside the scope.** Install-history rows were
  scoped only by the server-side device filter, and an unhonored filter clause is dropped silently
  rather than rejected. The client-side scope is now re-checked against every joined row, so it is
  authoritative and the server-side filter is only an optimization.
- **The "OS name contains" filter did not narrow the fleet-health tabs.** It is presented as device
  scope — the filter panel files it there and its chip stays active on those tabs — but was applied
  only to patch rows, so the device count, the compliance rollups, the charts and the whole Needs
  Reboot list still covered the entire fleet.
- **Compliance could report "100% compliant" beside a four-figure backlog.** Offline devices are
  excluded from the compliance denominator, but their pending patches were still counted, so an
  organization whose devices were all offline produced a row with no devices in it, a full green bar
  and a large Critical/Important count. Patches on devices outside the rollup — including orphans,
  which invented their own "(unknown)" organization — are now excluded with them.
- **99.5% no longer displays as "100%".** Compliance percentages round down rather than up, in the
  app, the Excel workbook and the HTML report, so a fleet that is not fully patched can never read as
  one that is.
- **Current patches with no reported status were missing from every rollup** while still appearing as
  rows, which understated the backlog and raised the compliance percentage. That feed is defined as
  the patches with no installation attempt, so such a record now counts as pending.
- **Dispatched actions resolved by timeout instead of by their activity.** The activity feed was
  queried with a timestamp in `newerThan`, which takes an activity *ID*, so every poll came back
  empty — indistinguishable from a lagging feed. The time bound is now applied locally. Job state
  also reads NinjaOne's actual fields: the lifecycle from `statusCode`, the verdict from
  `activityResult` (so a completed-but-failed activity is reported as a failure), and the exit code
  from `data`, which had never been read at all. Patch-apply, scan and reboot activities are now
  recognised by the correlator, which only matched script activities under a type code the API does
  not use.
- **The severity chart could overstate every band.** Segments looked their counts up by display
  label with a fallback to Unknown, so a renamed band silently drew Unknown's count twice.
- **Paging could loop forever** on an endpoint that returns a full page while echoing back an
  unchanged cursor.
- **The Failures table was missing its Patch Type column**, which both exports carried — the one
  column that distinguishes an OS failure from a third-party one, and third-party rows show
  "(no KB)". Several table headers also disagreed with the workbook and report ("Compliance %",
  "Pending Critical/Important", "Aged (past SLA)", "Device Role", "Pending Patches").
- **"N failing patches across M devices" used the fleet size** rather than the number of devices that
  actually failed, making a contained problem look fleet-wide.

### Changed

- **The Compliance and Needs Reboot tabs no longer claim the patch Type filter is ignored there.**
  It genuinely narrows those numbers — only the patch families a query fetches are in the rollups —
  so the tabs name the families instead of implying a whole-backlog figure.
- **Locations are loaded once for the whole tenant and served from cache**, so changing the
  organization scope no longer costs an API round trip per change.

## [0.12.2] - 2026-08-11

### Security

- **Credentials pasted into a script's parameters are redacted from the action audit log no matter
  how the line is spaced.** The redactor split on single spaces only, so a pasted line containing a
  tab or a newline arrived as one unsplittable token, matched nothing, and the credential was written
  to disk in cleartext.
- **The action audit log is now created owner-only (`0600`) on macOS and Linux.** It names devices,
  organizations and the operator's own parameters, and the default mode left all of that readable by
  every account on a shared or roaming-profile machine. Existing logs keep their current mode.
- **The action confirmation token and its request fingerprint are compared in constant time.** Low
  risk in practice — one single-use token at a time, expiring in five minutes — but this is the gate
  between a stale or modified frontend and a fleet-wide reboot, and a non-leaking comparison costs
  nothing.

### Performance

- **Large patch feeds are parsed once instead of twice.** Every paginated response used to be decoded
  into a generic JSON tree and then converted row by row, allocating a string for each field name of
  each row and discarding the whole intermediate. Whole-fleet queries — where the third-party feed
  alone runs to six figures of rows — now deserialize straight into their final type.
- **Queries and exports no longer stall the rest of the app while they work.** The device/patch join
  and rollups, the Excel and HTML writes, the save dialog, and the keyring write during a token
  refresh all ran on the async runtime, so a long export or a save dialog left open blocked unrelated
  work — including the background poller that resolves dispatched actions.
- **Paging a sorted patch table no longer re-sorts the whole fleet on every page.** The sort order is
  computed once and reused until the sort or the query changes, matching what grouping already did.
- **A query result holds far less memory.** Rows now share one copy of each repeated value —
  organization, device, OS, patch title, status — instead of each row owning its own; on a large
  fleet those are a few thousand distinct strings across hundreds of thousands of rows.
- **The client-side OS-name and free-text filters no longer allocate per row**, and grouping no longer
  allocates three strings per row to build its keys.

## [0.12.1] - 2026-08-10

### Security

- **Switching instance or client ID no longer destroys the sign-in you switched away from.** The
  change dropped every cache but kept the previous tenant's tokens, so the app still believed it was
  signed in, sent the old token to the new host, and the resulting rejection was treated as "this
  grant is dead" — which deletes the stored credential. The grant is now cleared as part of the
  switch.
- **Each instance keeps its own saved sign-in.** The refresh token and client secret were stored
  under one name shared by every tenant, so signing into a second instance overwrote the first one's
  credential. They are now stored per instance + client ID, and switching back finds the earlier
  sign-in still there. Existing saved credentials are migrated automatically on first launch — you
  will not be signed out by this change.

### Fixed

- **An impossible date typed into a date filter is now rejected instead of silently moving.**
  `2026-02-31` was accepted and became a day in March, so the query ran against a bound nobody
  chose.
- **A sign-in no longer ends because the server chose not to reissue a refresh token.** Servers may
  legitimately omit it and keep the existing one valid; the app discarded it, which left the session
  relying on the copy in the OS keychain — and that copy is explicitly allowed to be missing.
- **A locked keychain now says so.** It was reported as "not authenticated", which sent you to
  re-run a sign-in that could not have fixed it.
- **A stray request on the callback port can no longer kill a sign-in.** Anything arriving with a
  `state` value — a bookmark, a probe, another tool on the same port — ended the wait, and the
  sign-in then failed with "state mismatch — possible CSRF". The listener now recognises the real
  redirect by requiring both an authorization result and the exact `state` this sign-in generated,
  and answers everything else with a 404 while it keeps waiting.
- **A slow client can no longer stall sign-in.** The 10-second limit applied to each read rather
  than to the connection, so a client trickling a byte at a time held the listener open and the
  browser's redirect queued behind it until the three-minute timeout.
- **Starting a second sign-in now explains itself.** It previously failed while claiming the
  callback port with "Is another instance of this app running?", which pointed at a second copy of
  the app that did not exist — the port was held by this app's own sign-in, still waiting.
- **Job polling can no longer stop for the rest of the session.** The single-poller slot was
  released at exactly one place in the loop, so if anything else ended that task — an unexpected
  error while advancing a job, writing the audit record, or emitting progress — the slot stayed
  marked as taken. Every later dispatch then declined to start a poller, and jobs sat at "Queued"
  forever with nothing watching them. The slot now releases on every exit path.
- **Two actions sent to the same device no longer swap each other's results.** The native scan,
  apply and reboot endpoints return no job identifier, so those jobs are matched to NinjaOne's
  activity feed by "the newest matching activity on this device". With two dispatches in flight to
  one device, both could pick the same activity and report each other's exit code. An activity now
  belongs to whichever job claimed it first, and each job records its match on the first poll
  rather than re-guessing on every tick.
- **A job whose device keeps reporting activity now times out.** The 45-minute timeout was checked
  only when the activity feed returned nothing, so a job that kept matching a non-terminal activity
  stayed "Running" indefinitely — which also kept the poller alive and kept the row from ever
  being cleared out of the Jobs tab.

### Changed

- **Paging through grouped patches is no longer re-grouped from scratch on every click.** Each page
  of *By device* / *By patch* rebuilt the entire grouping — every row, hashed and sorted — while
  holding the same lock the Excel export takes. The grouping is now built once per result and
  reused, and expanding a group no longer scans the whole cache building a key string per row.
- **The HTML report's "Compliance by OS" table now matches the Excel export.** Its heading read
  "Compliance" against the workbook's "Compliance %", and it rounded percentages to whole numbers
  where every other table shows one decimal — the last table still rendering its own columns instead
  of the shared definition. It also silently dropped rows past the display cap; it now says so, like
  the failures and reboot tables do.
- **Web demo:** the Patches tab's group headers now show the offline and needs-reboot badges the
  sample data actually carries — they were hardcoded off, so those badges were unreachable in the
  demo — and its search now strips a `KB` prefix from either side, matching the app.
- **The install-history window is now shown, and adjustable, for failures too.** The "Installed
  within (days)" control appeared only when *Installed* was selected, but the window bounds the
  whole install-history pull — so a *Failed*-only query (the failure view) was silently truncated to
  it with nothing on screen saying so, and no way to widen it. It is now labelled "Install history
  window (days)", appears whenever *Installed* or *Failed* is selected, and its applied-filter chip
  reads "Install history: last Nd".
- **A failed query no longer throws away the fleet data it already downloaded.** The device
  inventory, the patch feeds and the install history were fetched together, and the first failure
  cancelled the rest — so a hiccup on the install-history call discarded minutes of completed
  whole-fleet paging and the retry started from cold. Every fetch now finishes and caches; the query
  still reports the error.

## [0.12.0] - 2026-08-07

### Added

- **"Install only the selected patches" is now a real action.** The action bar splits the apply
  buttons into two labelled groups — *Install all approved patches* and *Install only the selected
  patches* — for OS and software each. The second runs the remediation script configured in
  Settings → Patch actions and sends each device only the patches ticked **on that device**; a chip
  under the bar shows how many distinct patches would go to how many devices. Previously the only
  way to install specific patches was to pick a script by hand in the Jobs tab, which sent every
  device the same combined list.
- **The OS and Software remediation script IDs in Settings now do something.** They were persisted
  and editable but read by nothing. They gate the two new actions, and the settings help explains
  what the script must accept (`kbAllowList` for OS, `productAllowListB64` for software).

### Changed

- **Everything now dispatches from one place.** The script picker moved out of the Jobs tab and
  into the action bar on the Patches tab, collapsed behind "Run a library script…", so actions are
  launched next to the selection they target. The Jobs tab is now purely history. "Run as",
  "Restart the device after installing" and "Dry run" are shown once and apply to any script-driven
  action; each options row says which actions it reaches, since the native apply/scan/reboot
  endpoints ignore all three.

### Fixed

- **"Apply patches" no longer looks like it applies only the patches you selected.** NinjaOne's
  apply endpoint installs everything approved on the device and cannot be told which patches to
  install — so with the table grouped *By patch*, ticking one row and pressing Apply installed that
  device's whole approved backlog. The buttons are now named "Apply all …", and the confirmation
  dialog says so outright and points at the targeted action instead.
- **Software patches can now actually be targeted individually.** The software remediation path
  composed a `kbAllowList` parameter, but third-party patches carry no KB number, so the list was
  always empty and the script installed nothing. Software targets are now sent as
  `productAllowListB64` — base64-encoded, because NinjaOne splits parameters on spaces and product
  titles ("Google Chrome") contain them.
- **A script that restarts the device when it finishes is now flagged in the confirmation dialog**,
  the same way the native apply and reboot actions are.
- **"Dry run" and "Restart after installing" no longer change meaning behind your back.** Both had
  a second copy of their checkbox in the Jobs tab bound to the same setting, so toggling one
  silently changed what the other tab's buttons would do. There is now one of each.
- **"Target only the selected KBs" now sends each device its own KBs.** It previously sent every
  device the combined list from the whole selection, so a machine was told to install patches it
  did not have. The confirmation dialog lists the parameters per device, and warns when a selected
  device would be sent an empty list.
- **A hand-typed script parameter can no longer collide with another dispatch's approval.** The
  confirmation fingerprint now length-prefixes each device's parameter string; a string containing
  the internal separator could previously produce the same fingerprint as a different set of
  devices and patches.

## [0.11.3] - 2026-08-07

### Fixed

- **Switching instance mid-query no longer shows the previous tenant's patch data.** A query that
  spanned an instance change had its cached rows correctly discarded, but still handed the summary
  back to the window — so the table painted the old tenant's rows and rollups while paging, sorting,
  export and the HTML report all read the (correctly empty) new tenant's cache. The query now fails
  with an explanation and asks you to re-run. Saving a settings change that switches instance **or**
  client id also clears the results still on screen, for the same reason.
- **A patch action's effect is no longer hidden for up to two minutes by an in-flight refresh.**
  Applying patches or rebooting drops the cached patch state so the next query reads post-action
  data. If a whole-fleet fetch happened to be running at that moment, it wrote its *pre-action* rows
  back afterwards and the two-minute cache lifetime restarted on them.
- **A transient keychain problem no longer signs you out.** If the OS keychain refused a write while
  a token was being refreshed, the app discarded the freshly issued session and the next attempt
  presented an already-spent token, which forces a full interactive sign-in. The session is now kept
  and only its persistence across a restart is lost.
- **Sign-in no longer hangs when something else touches the callback port.** The loopback listener
  handled exactly one connection, so a browser preconnect, a favicon request or a local port scan
  could consume the sign-in and leave it waiting for the full three minutes. It now ignores
  non-callback connections and keeps waiting for the real one.
- **A fleet fetch that meets an unrecognized paging cursor now reports an error instead of stopping
  early.** Previously it ended quietly mid-fleet and returned a partial result that looked complete,
  which understates every compliance figure derived from it.
- **The HTML report's Needs Reboot table matches the Excel export again** — its column headings had
  drifted ("Role" vs "Device Role", "Pending patches" vs "Pending Patches").
- **The "Run as" list in the script picker reports lookup failures.** A failed fetch left the list
  empty, which looked identical to a device with no configured credentials.
- **Web demo:** with an organization selected, the Compliance tab's by-OS chart and the patch-age
  histogram now narrow with it instead of continuing to describe the whole sample fleet.
- **Settings:** a number typed above a field's maximum (for example a callback port over 65535) now
  clamps to that maximum rather than silently reverting to the previous value.

### Changed

- Concurrent queries on a cold cache now share one fleet fetch instead of each downloading the
  device inventory and third-party patch feed independently.
- Dispatching an action to many devices no longer re-copies the script, parameters and run-as
  identity per device.

## [0.11.2] - 2026-08-03

Maintenance only — **no user-facing changes**. If you are on 0.11.1 there is nothing new to see in
the app; this release exists to ship the dependency and supply-chain updates below.

### Security

- **Removed the only `unsafe` code from the base64 dependency.** base64 0.23 enables a `simd-unsafe`
  feature by default, which is that crate's sole source of `unsafe`; with it disabled the crate is
  `#![forbid(unsafe_code)]`. It exists purely to accelerate SIMD encode/decode engines that this app
  never uses — every call site uses the scalar engine — so the SIMD code was compiled into the
  shipped binary but never reached. It is now compiled out. This matters because base64 sits on the
  sign-in path (the PKCE challenge and the access token's scope claim) and on the patch-action
  confirmation path.

### Changed

- Updated `base64` (0.22 → 0.23) and the `calamine` test dependency (0.36.0 → 0.36.1).
- Refreshed the README demo screenshot.

### Fixed

- The screenshot workflow opened a fresh pull request on every release instead of updating one, and
  those pull requests could never pass their required checks. Both are fixed; this only affects
  repository maintenance, not the app.

## [0.11.1] - 2026-08-03

### Fixed

- **Signing out on its own during a query.** Every API call refreshes the access token when it is
  stale, and a query deliberately fans out many calls at once — so they could all refresh the *same*
  refresh token simultaneously. With token rotation, every loser of that race presented an
  already-used token, and the app treated the resulting rejection as "your session is dead" and
  erased the stored credential the winner had just saved. The refresh is now single-flight (one
  grant, the rest wait for it), and the credential is only cleared when the server actually says the
  grant is invalid — a rate limit, a gateway error, or a captive-portal page no longer costs you your
  sign-in.
- **The table showing one query while export and paging held another.** An auto-refresh tick
  overlapping a manual **Run query** could finish in either order, and whichever finished last won
  the cache — so the rows on screen, the rows you paged to, and the rows in an Excel export could
  come from different queries. The run you started last now wins consistently, on both sides. A query
  that spans an instance switch is discarded rather than being filed under the new tenant.
- **Patch actions: approving one thing and being able to send another.** The confirmation token did
  not cover *Include offline devices*, *Override maintenance window*, or the **Run as** identity, so
  an approval obtained with those settings still validated after they were changed. All three are now
  bound to the confirmation, and changing any of them re-opens the dialog instead of silently
  widening what gets dispatched.
- **Jobs that were dispatched but never polled.** A batch sent in the moment the job poller was
  shutting down could be left with nothing watching it, so it sat unresolved until the next dispatch
  happened to start a new poller.
- **A wrong "latest failure" and a wrong First seen sort.** Some patch records arrive with
  millisecond timestamps. The displayed date handled that, but the value used for *sorting* did not —
  so an affected row displayed the correct date while sorting as though it were thousands of years in
  the future, permanently winning "latest failure" and pinning itself to the top of a First-seen
  sort.
- **The HTML report's failure table was missing the Patch Type column** that the Excel export has
  had all along. Both now render from one shared column definition.
- **A very large day value in Settings could crash a query.** The install and SLA windows are now
  bounded (1–3650 days) at both ends.
- **The Needs Reboot tab now pages** instead of rendering every device at once, matching the rest of
  the app.
- Grouping while no query result is loaded (after signing out, or switching instance mid-request) no
  longer raises a spurious "Run a query before grouping" error.

### Changed

- The README's backend module map and several code comments described behavior the code no longer
  had; they now match what actually happens.

## [0.11.0] - 2026-07-29

### Added

- **Click a rollup number to see the rows behind it.** The Compliance and Failures tabs were
  read-only dead ends: seeing "Contoso · 63% · 41 pending Critical/Important" left you to scroll back
  to Filters, re-pick the organization by hand, tick a severity, press Run and switch tabs. Now the
  organization name in the compliance table, the KB in the failures table, and each band in the
  severity chart's legend are clickable — each one narrows the filters accordingly and drops you on
  the Patches tab showing exactly those rows. Because the fleet is already cached, this costs no
  extra requests to NinjaOne.

- **Group the Patches tab by device or by patch.** A new **View** switch above the table
  offers **Flat** (as before), **By device** — each device with its patches nested
  underneath — and **By patch** — each patch with the devices it's missing on, ordered by
  how many machines it affects. Groups expand on demand, and you can tick a whole group or
  open it and tick individual rows.

### Changed

- **Much less data over the wire.** Responses are now compressed (typically 8–12× smaller on the
  large patch feeds), the concurrent fetches at the start of a query share one connection instead of
  opening several, and the app now honours the proxy configured in macOS System Settings / Windows —
  which previously prevented some managed desktops from reaching NinjaOne at all.

- **Querying only OS patches no longer downloads the third-party catalogue.** The two patch families
  are fetched and cached independently, so a **Patch type: OS** query skips the third-party feed
  entirely — usually the largest download in a run. Widening back to **All** reuses whatever was
  already loaded.

- **Auto-refresh is no longer able to run continuously.** A refresh tick re-downloads live patch
  state, which on a large fleet can take longer than the interval itself. Ticks are now skipped while
  the window is hidden, a minimum interval is enforced regardless of what the UI requests, and the
  30-second cadence — which could never complete on a large tenant — has been removed. The remaining
  options are 1m / 5m / 15m.

- **Queries over large fleets do far less work per run.** Scoping the cached patch data to your
  selected organization/location/role used to copy every matching patch record twice — once to
  narrow it, once more to feed the compliance and severity rollups. On a fleet with six figures
  of third-party patches that was hundreds of thousands of redundant copies on every Run, refresh
  tick and filter change. The data is now read in place. No change to any number the app reports.

### Fixed

- **Grouped views were stuck on their first page.** In **By device** / **By patch**, the pager above
  the table was counting patch *rows* rather than groups, so it advertised far more pages than the
  view had and **Next** fetched rows that were never displayed — the screen simply didn't change,
  and every group past the first pageful was unreachable. The pager now counts groups ("Groups 1–100
  of 3,214") and **Next** loads the next set of group headers. Re-running a query or letting
  auto-refresh tick while grouped also used to leave the *previous* query's group headers on screen
  against the new result's totals; those are now refreshed with everything else.

- **The exported HTML report could show an empty or distorted severity chart.** Its pending-patch
  breakdown left `Security` and `Recommended` out of the total it divided by, while still drawing
  them. A backlog made up mostly of those two — the norm for third-party patches, which is where
  NinjaOne puts most of its `Security` grades — printed "No pending patches" even though the in-app
  chart showed thousands; a mixed backlog drew segments past the edge of the chart, hiding the
  lower-severity bands and overstating the widths of the rest. The chart and its total are now
  derived from the same list of bands, so they cannot disagree.

- **Legend swatches for Security and Recommended were invisible**, and **Optional and Unknown
  severities were styled identically** in the Patches table, so a patch NinjaOne graded as low
  priority looked the same as one it didn't grade at all. In the browser demo, sorting by severity
  also ranked Security and Recommended below Optional instead of above it.

- **"Aged (past SLA)" reported almost nothing, on any fleet.** The app read a field it believed was
  the patch's release date. NinjaOne's API has no such field — it exposes only the timestamp of when
  the patch data was *collected*, which is always recent — so the SLA comparison was effectively
  never true and the aged-patch count sat near zero however long a critical patch had been
  outstanding. The patch-age chart was skewed the same way. The app now measures and labels this
  honestly, as **how long NinjaOne has been reporting the patch**: the Patches column is **First
  seen**, the compliance column is **Pending past SLA**, the chart is **Pending patch age (since
  first seen)**, and the date filter reads **First seen**. Expect these numbers to be much higher
  than before — that is the backlog that was previously invisible. Patches NinjaOne has never
  timestamped now appear in their own **Unknown** age bucket instead of inflating **180+ days**.

- **A single gateway hiccup no longer discards a whole query.** Large fleets are fetched as dozens of
  sequential pages; a transient 502/503 on any one of them used to fail the entire run and throw away
  every page already downloaded. Server errors and dropped connections are now retried with backoff.
  Patch-apply, reboot and script actions are deliberately **not** retried this way — a failure there
  stays ambiguous and is reported for you to check, never silently re-run.

- **Ticking one patch no longer ticks every other patch on that device.** Selection was
  keyed on the device, so checking a single row visibly checked all of its siblings. It now
  tracks the exact rows you tick. This also fixes what was sent to a `kbAllowList` script:
  it previously received *every* KB on the device the moment one row was checked, so the
  one path NinjaOne offers for targeting specific patches could never actually be given a
  subset. Note that **Apply** itself is unchanged and still installs everything approved on
  the selected devices — NinjaOne has no per-patch apply endpoint — and third-party patches
  carry no KB at all, so they can't be targeted individually either way.

## [0.10.1] - 2026-07-29

### Fixed

- **"Sign in to run patch actions" no longer sticks after you've signed in.** The message
  explaining why patch actions were unavailable was worked out once when the app started and
  then left alone, so signing in — or enabling patch actions in Settings — never updated it.
  The action buttons stayed disabled with a stale reason until the app was restarted. The
  message is now derived from the current sign-in state each time it's shown, so it keeps up
  with every step and disappears the moment actions are genuinely available.

- **Third-party patches no longer disappear behind the Severity filter.** NinjaOne grades
  third-party software with its own vocabulary — `security`, `recommended` — and neither was
  recognised, so both collapsed into "Unknown". That sorted them below every other patch in
  the table and, worse, made them vanish entirely the moment any severity was ticked, since
  an unrecognised grade could never match a selection. Both are now first-class severities
  with their own badges and chart bands, alongside a new **Unknown** option so patches
  NinjaOne never graded stay reachable instead of being silently excluded by every possible
  filter. On a real fleet this was hiding the single largest bucket of OS patches too.

### Changed

- `Security` and `Recommended` rank below `Important`, so they sort and filter as real
  severities without entering the critical-backlog and SLA-aging figures on the Compliance
  tab — existing compliance numbers are unaffected.

## [0.10.0] - 2026-07-29

### Added

- **Patch actions — remediate from the same place you find the problem.** Tick patch rows
  and scan, apply, reboot, or run any script from your NinjaOne automation library, then
  watch each dispatch reach a terminal state in the new **Jobs** tab. Ticking a row selects
  its **device**: NinjaOne's API has no per-patch apply endpoint, so targeting specific KBs
  means running a library script that accepts a `kbAllowList` variable — the toolkit detects
  which scripts can and only offers per-KB targeting for those.
- **Off by default, and it stays that way until you say otherwise.** The feature lives
  behind **Settings → Patch actions**. Until you enable it the app requests the same
  read-only scope it always has, so an existing install is unchanged. Enabling it adds the
  `management` scope at the *next* sign-in — the app tells you when your current sign-in is
  still read-only and offers **Re-authorize**, because the OAuth refresh grant never
  re-sends the scope on its own.
- **Guardrails, enforced in the backend rather than the dialog.** Dry run is the default for
  scripts; a blast-radius cap (25 devices) and org-span cap (1) are hard blockers; offline
  devices are skipped by default, since NinjaOne queues work for them and would otherwise
  restart a machine hours later; forced reboots need a typed confirmation and a stated
  reason, which lands in NinjaOne's own activity feed. Every dispatch is written to an audit
  log before it goes out, with credential-shaped script parameters redacted.

### Fixed

- **A request that times out mid-flight is no longer re-sent.** The API client retried any
  timed-out request, which was harmless for reads but would have re-run an action. Timed-out
  actions are now reported as *Unknown* — never retried, since they may already be running
  on the device — while reads retry exactly as before.

## [0.9.1] - 2026-07-02

### Fixed

- **Results tab bar no longer overflows on narrow screens.** On a phone-width
  viewport the tab groups and the results summary ran past the panel's right
  edge (and widened the page); the row now wraps, and the summary takes its own
  line under the tabs.

## [0.9.0] - 2026-07-02

### Added

- **Sortable Patches table.** Every column header in the Patches tab is now a
  sort button cycling ascending → descending → off. Sorting covers the full
  result (it happens backend-side over the cached rows, not just the visible
  page); blank cells always sort last, Severity leads with the most urgent, and
  a new manual run returns to the default severity-first order. The web demo
  sorts its sample the same way.

- **Persistent error banner.** When a query (or a page fetch) fails, the results
  area now keeps a dismissible red banner describing the failure — previously the
  only trace was a toast that auto-dismissed after a few seconds. The banner
  clears on the next successful run.

### Changed

- **Destructive actions ask for confirmation.** Deleting a saved preset and
  **Clear stored secret** now take two clicks: the first click arms the control
  (it turns red and says so), the second fires it. Moving the pointer away or
  tabbing off disarms.

### Fixed

- **Update notes formatting.** The **Update available** window now renders the
  changelog as formatted text — section headings, bullet lists, and bold leads —
  instead of showing the raw Markdown source (`###`, `-`, `**…**`) verbatim.

## [0.8.0] - 2026-06-29

### Added

- **Compliance by OS.** The Compliance tab now has a "Compliance by OS" section — a
  per-OS compliance bar chart and table (devices, compliant, compliance %, pending
  Critical/Important, aged past SLA), grouped by each device's reported OS — so a
  lagging OS version is easy to spot alongside the existing per-organization view. Like
  the rest of the tab it reflects the selected device scope's whole pending backlog. The
  Excel export gains a matching **Compliance by OS** sheet, and the HTML executive
  report a matching "Compliance by OS" section (bar chart + table).

### Changed

- **Clearer data scope across the result tabs.** The results area now groups the tabs
  by scope tier — **Filtered results** (Patches, Failures), which honor every patch
  filter, and **Fleet health** (Compliance, Needs Reboot), which reflect the whole
  pending backlog for the selected device scope and ignore the patch filters. Each tab
  leads with a banner spelling out exactly what it shows and which filters apply, and a
  read-only chip row summarizes the filters behind the current result. On a Fleet-health
  tab the patch-tier chips grey out (they're ignored there) and the patch-filter controls
  hide entirely, so the view never implies a filter is shaping numbers it doesn't.

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
