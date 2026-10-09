# Repo map

File-by-file orientation map, kept in sync by the `agents-md-staleness-check.sh` hook
(structural edits remind you). It is deliberately not loaded into every agent session — read it
here when orienting. The always-loaded index is [AGENTS.md](../AGENTS.md); per-domain rules are
in [`.claude/rules/`](../.claude/rules/); rationale is in [design notes](./design/README.md).

```
src-tauri/                       # Tauri 2 backend (native target)
├── src/lib.rs                   # Tauri builder, tracing init, generate_handler![] registry
├── src/paths.rs                 # app_dir(): the one on-disk location for settings, logs, audit + history, window-state files
├── src/state.rs                 # AppState: auth, api client, settings, result cache + memos, fleet/lookup accessors, invalidation
├── src/state/cache.rs           # TenantCache<T>: tenant-stamped, TTL'd, epoch-gated, single-flight slot
├── src/state/jobs.rs            # job store, single-claim poller slot, confirm-token slot
├── src/auth.rs                  # OAuth2 PKCE (S256, loopback), keyring, single-flight refresh, conditional scope + management grant
├── src/actions/                 # device-action domain, re-exported flat as `crate::actions::*`
│   ├── mod.rs                   # JOB_TIMEOUT_MINUTES / MAX_JOBS, the submodule re-exports
│   ├── kind.rs                  # ActionKind + its guardrail predicates, RebootChoice, remediation_script_id
│   ├── job.rs                   # JobState / JobReport / JobRequest, fmt_ts
│   ├── planning.rs              # pure plan() guardrails, the plan types, the "Apply all" preview
│   ├── parameters.rs            # build_parameters: the per-kind `parameters` string a script is sent
│   ├── activity.rs              # match_activity / advance_job: resolving a job from /activities
│   ├── audit.rs                 # append-only action-audit.jsonl (parameters redacted by type)
│   └── tests.rs
├── src/api/                     # NinjaOne Public API client
│   ├── mod.rs                   # NinjaApiClient: /api/v2, bearer, retry policy (retry_for), ReplaySafety/OutcomeUnknown, df_query
│   ├── paging.rs                # get_paginated, parse_page/PagedRow, cursor forward-progress
│   ├── tests.rs                 # retry / pagination / replay tests (wiremock)
│   ├── devices.rs               # device inventory
│   ├── patches.rs               # current patches + install-history endpoints
│   ├── actions.rs               # WRITE path: patch scan/apply, reboot, script/run, automation-script library
│   ├── activities.rs            # /activities feed used to resolve dispatched jobs
│   └── lookups.rs               # orgs / all-locations / roles / node classes
├── src/filter.rs                # FilterParams → install-query df + PreparedFilter::device_allowed / row facets
├── src/model.rs                 # domain types (Device, Patch, PatchType, PatchStatus, Severity, …)
├── src/rows/                    # join → PatchRow and every rollup off the cached result
│   ├── mod.rs                   # QueryResult / QuerySummary + re-exports of every submodule
│   ├── join.rs                  # device↔patch join, Interner, DeviceLabels, build_rows
│   ├── compliance.rs            # compliance / by-OS / per-device rollups (apply_device_health), rollup_device, scope note, SlaCutoffs
│   ├── rollups.rs               # failures, severity by org, age buckets, SeverityCounts::BANDS
│   ├── backlog.rs · install_time.rs  # worst devices / offline backlog; first seen → installed median
│   ├── groups.rs                # grouping, sorting, paging, device_detail, rows_by_device over the cache
│   ├── scope.rs                 # QueryScope export provenance
│   ├── table.rs                 # TableCell / TableColumn / format_pct / clamp_cell / join_capped — the shared column definition
│   └── tests.rs
├── src/history.rs               # append-only run-history.jsonl (one rollup + per-org line per query) + RunRecord
├── src/changes.rs               # per-scope run-snapshots/ → RunChanges (changes since the previous comparable run)
├── src/export.rs                # rust_xlsxwriter workbook: detail, rollup, Devices, device-list, Stuck Approvals, Changes, About; UTC date cells
├── src/csv_export.rs            # detail-row CSV: BOM, CRLF, RFC 4180 quoting, formula-injection guard
├── src/report.rs                # standalone HTML executive report from the cached QueryResult
├── src/window_state.rs          # window geometry: debounced save, clamped restore before first show
├── src/settings.rs              # persisted Settings (instance, client id, ports, windows, SLA policy, presets); atomic save, corrupt file quarantined
├── src/error.rs                 # UiError { message } — the IPC error shape
├── src/commands/                # #[tauri::command] handlers (actions, auth, diagnostics, export, lookups, patches, settings, update)
├── src/commands/actions/        # mod.rs handlers · confirm.rs request_hash · plan.rs build_plan · dispatch.rs send_action · poller.rs poll_tick · tests.rs
├── src/commands/diagnostics.rs  # read-only: open the log folder, read back action-audit.jsonl
├── tauri.conf.json              # CSP, bundle targets, before{Dev,Build}Command, updater (pubkey/endpoint); main window starts hidden
├── updater-build.json           # release-only overlay: createUpdaterArtifacts on (signing required)
└── capabilities/default.json    # webview capabilities: `core:default` only (the save dialog runs in Rust)

web-rs/                          # Leptos 0.8 CSR frontend — separate wasm32 crate
├── src/app.rs                   # module decls, shared consts (SEVERITY_OPTIONS), App root + startup wiring
├── src/app/
│   ├── state.rs                 # AppState wrapper + Copy sub-structs by concern; no test module — logic goes to util
│   ├── state/                   # impl AppState, one file per concern (no test modules)
│   │   └── query.rs · view.rs · selection.rs · actions.rs · lookups.rs · presets.rs · view_link.rs
│   ├── actions.rs               # ActionBar (the one dispatch surface), ConfirmActionModal, RunAsRoles, JobsTable
│   ├── tables.rs                # results panel: tab bar, banners, applied-filter chips, Pager
│   ├── tables/                  # one file per results tab (+ changes panel, backlog, device drill-down dialog)
│   ├── header.rs · controls.rs · filters.rs · settings.rs · charts.rs · toaster.rs · update.rs
│   ├── shortcuts.rs             # key handler + help dialog (map: util::shortcut_for)
│   ├── modal.rs                 # focus_trap: dialogs take focus on open, keep Tab inside, restore the opener
│   └── util/                    # JS-free pure helpers + their host tests
│       └── one file per concern (query, selection, sla, guardrails, changes, shortcuts, view_link, theme, …) + tests.rs
├── src/api.rs                   # ipc! macro → typed invoke wrappers + is_tauri() browser-mode guard
├── src/demo.rs                  # pure sample-data builder for demo / web mode
├── src/types.rs                 # request/response types mirrored from the backend
├── tests/backend-grouping.json  # backend-generated fixture the demo's grouping is asserted against
├── styles.css                   # plain global CSS (BEM-ish names); every colour a :root token, light palette via data-theme
└── Trunk.toml                   # WASM build/serve (127.0.0.1:8080); never set public_url here
```

Supporting surfaces: `.claude/hooks/` (commit-msg rule + command parity + staleness + secrets
scan, `test.sh` self-tests them), `.github/workflows/` (ci · codeql · pages · release ·
screenshot), `remediation/` (reference "Apply selected" PowerShell scripts; `tests/fixtures`
pins `build_parameters`), `scripts/` (screenshot tooling, changelog-notes.sh,
check-license-lists.sh, ninjaone-spec-digest.py), `about.toml` + templates →
THIRD-PARTY-LICENSES.md (`just licenses`), `.githooks/` (commit-msg + pre-push; installed by
`just setup`).
