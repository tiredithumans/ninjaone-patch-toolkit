# The write path: device actions

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/actions.rs` (domain + `plan()`), `src-tauri/src/api/actions.rs` (the
POSTs), `src-tauri/src/commands/actions.rs` (plan/confirm/dispatch/poll),
`src-tauri/src/api/activities.rs`, `web-rs/src/app/actions.rs` (UI).

The feature is opt-in (`settings.actions.enabled`, default false) and every command re-checks
`require_actions_enabled` — a stale frontend must not be able to widen the blast radius. It is a
hand-placed call rather than something the type system demands, so
`commands::actions::tests::every_mutating_command_checks_that_actions_are_enabled` derives the
command list from the source and fails if a new one skips it. Read-only handlers over local job
state are the documented exceptions and are named in that test.

## There is no per-KB apply endpoint, so there are two apply paths and the UI names both

`/device/{id}/patch/{os,software}/apply` installs everything approved on the device and cannot be
told which patches to install. Targeting specific patches is possible **only** via a library
script that accepts a target list. Those are different mechanisms with different blast radii, so
they are different `ActionKind`s — `OsPatchApply`/`SoftwarePatchApply` ("Apply all …") vs
`OsPatchRemediate`/`SoftwarePatchRemediate` ("Apply selected …") — grouped under separate
headings in `ACTION_GROUPS` (`web-rs/src/app/actions.rs`). Presenting them as one "Apply" button
is a real hazard: ticking one row under *By patch* grouping and pressing Apply installs the
device's whole approved backlog, and nothing says so. `plan()` warns on the native kinds and
names the targeted counterpart (`untargeted_counterpart` / `targeted_counterpart`). Don't
collapse the pairs back into one action.

### Remediation script ids are resolved backend-side

The remediation script ids live in `settings.actions.{os,software}_patch_script_id` and are
resolved **backend-side** from Settings (`actions::remediation_script_id`), never taken from the
request — the kind carries guardrails a hand-picked `Script` doesn't. An unset id is a `plan()`
blocker, and so is an empty target list (a script with an empty allow list reports success having
installed nothing). `AutomationScript::accepts_kb_allow_list` still gates the per-KB checkbox on
the hand-driven `ScriptPicker` path.

### The parameter encoding is chosen by kind

`build_parameters` sends `kbAllowList=` (comma-separated KBs) for OS and `productAllowListB64=`
(base64 of titles joined by `|`) for software, because NinjaOne splits `parameters` on
**spaces** and product titles contain them. The software arm was dead code until
`SoftwarePatchRemediate` existed: the only caller composed for `ActionKind::Script`, which falls
to the `kbAllowList` arm, so a software remediation script was handed a KB list — and third-party
patches carry no KB, so it was always empty.

## Selection is per patch row; dispatch is per device, with per-device targets

`DeviceSelection.patches` maps each ticked row's `patch_key` → a `SelectedPatch { kb, name,
is_os }`, and a device enters the selection with its first ticked row and leaves with its last.
Ticking a row must **not** tick the device's other rows: it once did, which swept every KB on the
device into `kbAllowList` and made the one path capable of per-patch targeting unable to receive a
subset.

A dispatch sends each device **only the patches ticked on it** (`util::targets_by_device` →
`ActionRequest.device_targets` → `commands::actions::per_device_parameters`, a
`BTreeMap<i64, String>` carried on `DispatchContext`). This covers **every** path that sends an
allow list — both remediation kinds *and* the script picker's "Target only the selected KBs".
There is no batch-wide `targets` field: it handed every device the union of the selection, which
is invisible in a dialog showing one parameter string. Don't reintroduce one; a genuinely uniform
string is what the verbatim `parameters` field is for, and it is honored only on the `Script`
path where the operator can actually type it.

Devices with nothing ticked *of that family* are dropped from a remediation's `device_ids`
entirely rather than dispatched with an empty list. A hand-picked `Script` keeps them (the
operator chose them and the script may not need a list), and `build_plan` warns, naming them via
`untargeted_names` / `summarize_names`.

What the *native* Apply does on those devices is still all-or-nothing — that's the endpoint, not
the selection model — so don't "fix" that gap by widening selection again.

Third-party patches carry no KB (the software feed has no `kbNumber`), so they are targeted by
**product title** instead; an OS remediation silently skips them and vice versa, mirroring the
asymmetry of the two feeds.

## `ReplaySafety::ActOnce` on every POST

`request_raw`'s timeout arm would otherwise replay the body and re-run the action; 429/401 still
replay (the gateway rejected before the device queue). A timed-out dispatch becomes
`JobState::Unknown` — polled, never auto-retried. See
[api-client.md](./api-client.md#the-5xx-and-connect-arms-are-idempotent-only) for why 5xx is also
not replayed on a write.

## Confirm tokens are payload-bound and single-use

`plan_action` hashes **everything that reaches NinjaOne or that the guardrails read** — kind ‖
sorted device ids ‖ script ref ‖ **resolved** script ‖ per-device parameters ‖ run_as ‖ reboot
choice ‖ reboot mode ‖ include_offline ‖ override_window ‖ dry_run — into a 5-minute token;
`run_action` re-plans from scratch and re-checks the hash.

- The parameters are hashed as `canonical_parameters` — every device's own string, bound to its
  id — so re-ticking one row on one device invalidates the approval.
- The *resolved* script is hashed separately because for a remediation kind it comes from
  Settings rather than from the request, so an id edited while the dialog is open would otherwise
  run a different script under the same approval.
- `canonical_parameters` **length-prefixes each value**. The `0x1f` separator discipline is
  enough for fields the toolkit composes, but a parameter string can be *typed by hand* in the
  script picker, so `{1: "a\u{1e}2=b"}` rendered identically to `{1: "a", 2: "b"}` — two
  different dispatches sharing one approval.
- Editing the selection after the dialog opened invalidates the approval rather than widening it.
- `request_hash` **destructures `ActionRequest` exhaustively**, so a new field is a compile error
  there rather than a silent omission — which is exactly how `include_offline`,
  `override_window` and `run_as` came to be missing (the first two gate `plan()`'s offline warning
  and maintenance-window blocker; the third is the execution identity).
- Fields are separated by `0x1f` so two different requests can't concatenate to one hash input.

## There is one dispatch surface, and the run options are shared

Everything dispatches from the `ActionBar` on the Patches tab, next to the selection it targets;
the `ScriptPicker` is folded into it behind a `<details>` and the Jobs tab is history only. `Run
as`, `Restart the device after installing` and `Dry run` are rendered **once** and reach every
`runs_a_script()` kind — they mean the same thing for a remediation install and a hand-picked
script, and duplicating the controls across two tabs while they wrote the same signals meant
ticking "Dry run" in the Jobs tab silently changed what an Apply button did. Each options row
carries a label naming the actions it reaches: the native endpoints take no parameters, have no
preview mode and run as NinjaOne's agent, so an unlabelled "Dry run" beside them reads as
protection they cannot give.

## Guardrails live in `actions::plan`

`plan()` is pure with an injected clock. Adding a guardrail means extending `blockers`/`warnings`
there, not adding a dialog. The one exception is the `dry_run` check, which is *also* asserted at
the dispatch site in `run_action` — defense in depth, so a new `ActionKind` whose
`supports_dry_run()` is wrong can't send a real mutating POST while the UI says "Dry run".

## After a mutating action, invalidate the current-patch cache

Call `invalidate_current_patches()` (and `invalidate_fleet_devices()` after a reboot) —
`clear_lookups_cache()` is too blunt, and the 120 s current-patch TTL would otherwise serve
pre-action data. `last_result` is deliberately *not* dropped; the frontend raises a stale-results
banner instead. **A dry run does neither**: `invalidate_after` takes `dry_run` and returns early,
and `confirm_plan` sets `results_stale` only for a non-dry-run mutating kind — `dry_run` defaults
on, so every default preview used to raise the banner and its Refresh link forced a whole-fleet
refetch. See [query-cache.md](./query-cache.md#the-stores-are-epoch-gated-and-the-fetches-are-single-flight)
for how the invalidation survives an in-flight fetch.

## Job state is tenant-stamped; the poller is single-claim

Job state lives in `AppState.jobs`, mirroring `last_result` — a tenant switch reads as a miss. The
poller is single-claim (`try_claim_job_poller`) and emits `action:progress` (no capability change
needed; `core:event:default` already covers it). It retires via `release_job_poller_if_idle()`,
which re-checks for pending jobs **and** clears the claim flag under the jobs lock. Dispatch
appends its jobs before calling `try_claim_job_poller`, so a batch landing during shutdown is
either seen (the poller keeps going) or strictly after the release (its own claim succeeds).
Releasing unconditionally left jobs dispatched in that gap with no poller at all.

**NinjaOne v2 has no script-output endpoint.** A job resolves from `/activities` only, so surface
the exit code plus the activity/series correlator.

## Resolving a dispatched action from `/activities`

Three fields decide a job's fate and the spec gives them different jobs: `statusCode` is the
enumerated lifecycle (`STARTED`/`IN_PROCESS`/`COMPLETED`/`CANCELLED`/`BLOCKED`), `status` is
free-text "Status description" with no enum, and `activityResult` is the outcome
(`SUCCESS`/`FAILURE`/`UNSUPPORTED`/`UNCOMPLETED`/`AGENT_OFFLINE`). `Activity::lifecycle()`
prefers `statusCode` and falls back to `status`; `Activity::outcome()` takes the verdict from
`activityResult` first, so a `COMPLETED` activity carrying `FAILURE` is a failed job. The exit code
comes from `data` (the spec's untyped bag), with `result` kept as an alias — reading only `result`
meant `exit_code()` always returned `None` and every job reported "Completed, no exit code".

### `newerThan` is an activity ID, not a timestamp

The dispatch-time floor is applied **client-side** in `api::activities` against `activityTime`.
Sending a Unix timestamp there asked for activities newer than an id beyond any real one, so the
feed came back empty every poll — and an empty feed reads as "the feed lags", so every dispatch
resolved by timeout instead. The endpoint's date parameters are `after`/`before`, whose format the
spec never states; don't guess.

### The activity-type filter must list what the native endpoints emit

`is_action_activity` accepts `SCRIPTING` (the spec's value; `SCRIPT` is not in the enum but is
kept anyway), `PATCH_MANAGEMENT`, `SOFTWARE_PATCH_MANAGEMENT`, `SYSTEM`, `SCHEDULED_TASK` and the
`ACTION`/`ACTIONSET` pair. `scan`/`apply`/`reboot` return no correlator, so this heuristic is their
only path to resolving.
