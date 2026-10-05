---
paths:
  - "src-tauri/src/actions/**"
  - "src-tauri/src/commands/actions/**"
  - "src-tauri/src/api/actions.rs"
  - "src-tauri/src/api/activities.rs"
  - "web-rs/src/app/actions.rs"
  - "web-rs/src/app/tables/**"
  - "web-rs/src/types.rs"
  - "remediation/**"
---

# Device-action write path

Violating these silently widens the blast radius.

**New device action — 4 steps**: the POST in `api/actions.rs` via `post_action`/`post_json`
(`ReplaySafety::ActOnce`); an `ActionKind` variant with correct `is_mutating()` /
`supports_dry_run()`; the dispatch arm in `commands::actions::dispatch::send_action`; the button
in `web-rs/src/app/actions.rs::ACTION_GROUPS` under the heading that names its *mechanism*. Mirror
the variant in `web-rs/src/types.rs::ActionKind`. → `docs/design/actions.md`

- **Every write POST passes `ReplaySafety::ActOnce`**; any ambiguous outcome (timeout, in-flight
  transport error, 5xx, unreadable 2xx) fails with `api::OutcomeUnknown` and becomes
  `JobState::Unknown` via `is_outcome_unknown` (a downcast, never message text) — polled, never
  replayed. → `docs/design/actions.md#replaysafetyactonce-on-every-post`
- **"Apply all" (native endpoint) and "Apply selected" (library script) are different
  `ActionKind`s** under different `ACTION_GROUPS` headings. Don't collapse them. Remediation
  script ids resolve from Settings, never the request; an unset id or an empty target list is a
  `plan()` blocker. → `docs/design/actions.md#there-is-no-per-kb-apply-endpoint-so-there-are-two-apply-paths-and-the-ui-names-both`
- **Selection is per patch row; dispatch is per device with per-device targets**
  (`util::targets_by_device` → `ActionRequest.device_targets` → `per_device_parameters`). Ticking
  a row must not tick the device's other rows. No batch-wide `targets` field. Needs Reboot has
  its own device selection (`util::device_selection_allows`).
  → `docs/design/actions.md#selection-is-per-patch-row-dispatch-is-per-device-with-per-device-targets`
- **`build_parameters` encodes by kind:** `kbAllowList=` for OS, `productAllowListB64=` for
  software (NinjaOne splits on spaces). OS targets must pass `kb_number` or `plan()` blocks.
  → `docs/design/actions.md#the-parameter-encoding-is-chosen-by-kind`
- **Confirm tokens are payload-bound and single-use.** `request_hash` destructures `ActionRequest`
  exhaustively, hashes the *resolved* script and run-as and length-prefixed per-device
  parameters; ids are not de-duplicated (a repeated id is a `plan()` blocker); `run_action`
  re-plans and re-checks. The token carries the `JobSession` sampled before `build_plan`.
  → `docs/design/actions.md#confirm-tokens-are-payload-bound-and-single-use`
- **Guardrails go in `actions::plan` (`blockers`/`warnings`), not in a dialog.** The `dry_run`
  check is also asserted at the dispatch site. → `docs/design/actions.md#guardrails-live-in-actionsplan`
- **Dry run requires a script declaring `dryRun`** (`DryRunSupport::Declared`); the window
  override is per dispatch and audited; the "Apply all" preview reads only cached patches (cold =
  "unknown"). → `docs/design/actions.md`
- **One dispatch surface (`ActionBar`, on Patches and Needs Reboot); run options render once.**
  A retry is `Failed`-only (never `Unknown`) and re-plans from `JobReport.request`.
  → `docs/design/actions.md#there-is-one-dispatch-surface-and-the-run-options-are-shared`
- **After a non-dry-run mutating action call `invalidate_current_patches()`** (and
  `invalidate_fleet_devices()` after a reboot); never `clear_lookups_cache()`; never drop
  `last_result`. A dry run invalidates nothing and raises no stale banner.
  → `docs/design/actions.md#after-a-mutating-action-invalidate-the-current-patch-cache`
- **Jobs are tenant-stamped; the poller is single-claim** (`try_claim_job_poller` /
  `release_job_poller_if_idle`). Dispatch appends jobs before claiming. Every write-path store
  and every send attempt (retries too, via `with_send_guard`) re-checks the `JobSession`
  sampled before the first `.await`.
  → `docs/design/actions.md#job-state-is-tenant-stamped-the-poller-is-single-claim`
- **A job resolves from `/activities` only, one read per device per tick** (`poller::feed_reads`,
  at most `MAX_FEED_READS_IN_FLIGHT` in flight): `statusCode` is lifecycle, `activityResult` is
  the verdict, exit code from `data`; `newerThan` is an activity **id**, so the time floor is
  applied client-side; `is_action_activity(kind, type)` accepts only the types that kind emits.
  → `docs/design/actions.md#resolving-a-dispatched-action-from-activities`