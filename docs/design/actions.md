# The write path: device actions

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/actions.rs` (domain + `plan()`), `src-tauri/src/api/actions.rs` (the
POSTs), `src-tauri/src/commands/actions/` (`mod.rs` the commands, `plan.rs`, `confirm.rs`,
`dispatch.rs`, `poller.rs`),
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

Because the string is space-split, a KB target is spliced in unquoted, so `plan()` blocks any
target of a KB-encoded kind that is not digits after an optional, case-insensitive `KB` prefix
(`actions::kb_number`): `"123 dryRun=false"` would otherwise add a key of its own. The check reads
`commands::actions::plan::composed_targets` — exactly the targets that will be composed, so a
hand-typed string (sent verbatim) and the native endpoints are not checked against targets they
never send. `build_parameters` also drops a malformed KB, in case a caller skips the planner.
Software targets are free-form product titles and need no such check: they travel base64-encoded.

The receiving end is [`remediation/`](../../remediation/README.md): reference scripts that parse
exactly this string, strictly (an unknown key, a malformed KB or an empty list exits 1 rather
than installing nothing and reporting success). `remediation/tests/fixtures/parameter-contract.json`
is read by both `build_parameters_matches_the_reference_script_fixture` and the scripts' Pester
suites, so changing the encoding here fails a test until the scripts follow. A library script is
only offered per-KB targeting or dry run when it **declares** the `kbAllowList` / `dryRun` script
variable (`AutomationScript`), so the import instructions there name the variables to declare.

## Selection is per patch row; dispatch is per device, with per-device targets

`DeviceSelection.patches` maps each ticked row's `patch_key` → a `SelectedPatch { kb, name,
is_os }`, and a device enters the selection with its first ticked row and leaves with its last.
Ticking a row must **not** tick the device's other rows: it once did, which swept every KB on the
device into `kbAllowList` and made the one path capable of per-patch targeting unable to receive a
subset.

A dispatch sends each device **only the patches ticked on it** (`util::targets_by_device` →
`ActionRequest.device_targets` → `commands::actions::plan::per_device_parameters`, a
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

### "Apply all" shows what it will install before it is confirmed

The native endpoint takes no list, so the confirm dialog is the only place the operator can see
the backlog they are approving. `plan()` attaches an `ApplyPreview` to the two native applies
(`actions::apply_preview`): per eligible device, the count of current-patch records with status
`APPROVED` (what the endpoint installs) and `MANUAL` (NinjaOne's "pending approval", which it
does **not** install — `PatchStatus::Pending.api_value()`), counted from the family's whole-fleet
current-patch cache, with that cache's fetch time.

- The planner **never fetches** for it. `AppState::cached_current_patches` peeks the
  `TenantCache` slot (tenant-checked, TTL ignored — the dialog states the fetch time instead);
  a cold family is `known: false` and the dialog says "unknown (patch data not loaded)", never
  zero. Paging a six-figure feed to draw a dialog would make every plan wait on it.
- A post-action `invalidate_current_patches()` clears the slot, so the preview never counts
  patches the previous apply may already have installed.
- A device with **zero** approved patches is a `plan()` *warning*, not a blocker: the apply
  "succeeds" and installs nothing while the rows the operator was looking at sit in `MANUAL`.

Third-party patches carry no KB (the software feed has no `kbNumber`), so they are targeted by
**product title** instead; an OS remediation silently skips them and vice versa, mirroring the
asymmetry of the two feeds.

### The Needs Reboot tab selects devices, in a map of its own

The Needs Reboot tab lists devices, not patches, so it selects **devices**
(`ActionState.device_selected`, `util::SelectionSource::Devices`). It is a separate map from the
row selection on purpose: a device ticked there has no patch rows, and folding it into
`selected` would either tick the device's rows (the sweep this section exists to forbid) or put a
row-less device where a remediation reads targets. From a device-level selection only
`util::device_selection_allows` kinds are reachable — Reboot and the two scans. The remediation
kinds need per-patch targets it cannot supply; the native "Apply all" needs none, but it installs a
backlog that tab never shows, so it stays where the patches it reaches are listed; a script is
chosen next to the selection it may target. Those buttons are disabled with
`util::source_disabled_reason`, and the request goes through the same `build_action_request` so
the run options reach exactly the kinds they reach from the Patches tab. The device selection is
cleared with the row selection (`clear_selection`) and pruned on an auto-refresh against the fresh
reboot list (`util::prune_device_level_selection`), since a device that just rebooted no longer
belongs in it.

## `ReplaySafety::ActOnce` on every POST

`request_raw`'s timeout arm would otherwise replay the body and re-run the action; 429/401 still
replay (the gateway rejected before the device queue). Every ambiguous outcome — a timeout or a
connection lost after send, a 5xx, an unreadable 2xx body — fails with the `api::OutcomeUnknown`
type, and `commands::actions::dispatch::record_dispatch` turns it into `JobState::Unknown`: polled, never
auto-retried. Only a 4xx, a connect failure or a local refusal is `Failed`. See
[api-client.md](./api-client.md#an-ambiguous-write-fails-with-the-outcomeunknown-type-never-a-phrase).

A job that settles at dispatch (`Failed`) never reaches the poller, which writes the other closing
audit records, so `dispatch_one` writes its `AuditEntry::closing` itself — otherwise the trail
held only "dispatching" for a request NinjaOne rejected.

## Confirm tokens are payload-bound and single-use

`plan_action` hashes **everything that reaches NinjaOne or that the guardrails read** — kind ‖
sorted device ids ‖ script ref ‖ **resolved** script ‖ per-device parameters ‖ **resolved**
run_as ‖ reboot choice ‖ reboot mode ‖ reboot reason ‖ include_offline ‖ override_window ‖
dry_run — into a 5-minute token; `run_action` re-plans from scratch and re-checks the hash.

- The reboot reason is sent to NinjaOne and lands in its activity feed as the server-side record
  of why the machine went down, so it is bound too (length-prefixed, as dispatched: `None` is an
  empty string). It used to be excluded alongside the display-only `script_name`, so the reason
  could be edited after review under the same approval.

- The run-as identity is resolved in `build_plan` (`resolve_run_as`: a blank request means the
  Settings default) and `run_action` dispatches that value. It used to fall back to Settings
  *after* the token check, so the default could change between review and confirm and the
  dispatch ran as a different identity under the same approval.
- The device ids are sorted but **not de-duplicated**, and a repeated id is a `plan()` blocker. The
  hash used to de-duplicate while `plan()` and the dispatch loop did not, so an approval for `[5]`
  validated `[5, 5]` — the same script run twice on one machine.

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
- The token is stamped with the `JobSession` (tenant + jobs epoch) taken **before** `build_plan`,
  and `store_pending_confirm` re-checks it under the slot lock. It used to read the tenant at
  store time, after the plan's fetches, so a sign-out and sign-in (same tenant, different
  operator) or a tenant switch in that gap left a token stamped for the new session, which
  `run_action`'s re-plan under that session then matched. `clear_jobs` bumps the epoch before it
  clears, so the late store is refused and `plan_action` says to plan again.

## There is one dispatch surface, and the run options are shared

Everything dispatches from the `ActionBar`, next to the selection it targets — on the Patches tab
(patch rows) and the Needs Reboot tab (devices, `source=SelectionSource::Devices`). It is one
component on both, reading the same run-option signals, not a second surface; from the device
source it simply omits the script-only options row and the script picker, which nothing it can
reach reads. The `ScriptPicker` is folded into it behind a `<details>`, and the Jobs tab is history
plus a Retry that re-opens the same plan → confirm dialog (below). `Run
as`, `Restart the device after installing` and `Dry run` are rendered **once** and reach every
`runs_a_script()` kind — they mean the same thing for a remediation install and a hand-picked
script, and duplicating the controls across two tabs while they wrote the same signals meant
ticking "Dry run" in the Jobs tab silently changed what an Apply button did. Each options row
carries a label naming the actions it reaches: the native endpoints take no parameters, have no
preview mode and run as NinjaOne's agent, so an unlabelled "Dry run" beside them reads as
protection they cannot give. The maintenance-window override sits in the "Applies to every
action" row for the same reason: it is one choice about the next dispatch, not a per-button one.

## A retry is a re-plan, never a replay

Every `JobReport` carries a `JobRequest` — what that one device was sent: the script ref, a
`Script`'s typed parameters, the **resolved** run-as, reboot choice/mode/reason, `include_offline`
and the device's own targets (`commands::actions::job_request`, which destructures
`ActionRequest` exhaustively like `request_hash`). The Jobs tab rebuilds an `ActionRequest` from it
(`util::retry_request`) and sends it through `plan_action`, so a retry gets a fresh payload-bound
token and every `plan()` guardrail re-runs against current state. Nothing re-sends a POST.

- Only `JobState::Failed` is retryable (`util::retry_blocked_reason`). `Unknown` means the
  action may already have reached the device — replaying it is what `ReplaySafety::ActOnce`
  refuses — and a running or finished job has nothing to retry.
- `override_window` is **not** recorded: an override approved for the original dispatch says
  nothing about the moment of the retry, so the maintenance window is re-evaluated.
- The recorded run-as is the resolved one, so a changed Settings default does not change who a
  retry runs as without the dialog saying so.
- "Retry N failed" rebuilds one request from a batch's failed rows; jobs whose kind, dry-run flag
  or options differ are refused rather than merged, since one would run with the other's options.
- The typed parameters are held in memory only, as the request they came from was; the audit log
  keeps its redacted copy.

## Guardrails live in `actions::plan`

`plan()` is pure with an injected clock. Adding a guardrail means extending `blockers`/`warnings`
there, not adding a dialog. The one exception is the `dry_run` check, which is *also* asserted at
the dispatch site in `send_action` — defense in depth, so a new `ActionKind` whose
`supports_dry_run()` is wrong can't send a real mutating POST while the UI says "Dry run", and a
script dispatch whose parameter string lacks the `dryRun=true` token
(`dispatch::carries_dry_run_flag`) is refused rather than run for real.

## A dry run is allowed only for a script that declares `dryRun`

NinjaOne has no preview mode. A toolkit "dry run" only appends `dryRun=true` to the composed
parameter string, so a script that never reads it **runs for real** while the Jobs tab and the
audit trail say "Dry run". `supports_dry_run()` (true for every script-running kind) is therefore
necessary but not sufficient: `build_plan` resolves the script and classifies it as a
`DryRunSupport`, and `plan()` blocks every value but `Declared`.

- `Declared` means the library entry declares a `dryRun` script variable (name match,
  case-insensitive) or a parameter line containing `dryRun` as a whole token
  (`AutomationScript::accepts_dry_run`). Stricter than `accepts_kb_allow_list`'s substring match,
  so `-NoDryRunSupport` does not count.
- A built-in action (`ScriptRef::Action`) takes no `dryRun`: blocked.
- **Hand-typed parameters with Dry run on are blocked**, whatever the script declares. They are
  sent verbatim, so no flag is added; the alternative — appending `dryRun=true` when the string
  lacks it — would rewrite what the operator typed and put a parse of free-form text between them
  and a live run. Clearing the box composes the parameters instead, which always carry the flag.
- The library is read only for a dry run of a script, on plan **and** on confirm (`run_action`
  re-plans), so a script edited to drop `dryRun` between review and confirm is refused. A library
  that cannot be read, or no longer lists the id, is `Unverified` — blocked, fail closed.
- `ScriptSummary.accepts_dry_run` lets the action bar disable the affected buttons while Dry run is
  on, and name the scripts that cannot preview; it is advisory, the planner decides.

## The maintenance window

`ActionSettings.window_days` (`0` = Sunday), `window_start_minute`, `window_end_minute` (an end
before the start wraps past midnight; the day is the day the window *opened*). `window_is_open`
reads `DateTime<Local>` — **this computer's clock**, not the devices' (NinjaOne exposes no device
time zone on this path) — so the blocker, the Settings editor and the override checkbox all say
"this computer's time", and the blocker prints the UTC offset. `save_settings` rejects times
outside a day, days outside 0–6, a zero-length window and an enforced window with no days, and
stores the days sorted and de-duplicated.

The override is two switches, and the blocker names whichever is missing:
`allow_window_override` in Settings *permits* it; the action bar's "Override the maintenance
window for this dispatch" (`override_window` on the request) *requests* it. The checkbox is shown
only while the window is enforced and overridable — not "only while it is closed", which would
need a second copy of `window_is_open` against the webview's clock that could disagree at the
boundary; an override requested inside an open window is inert. It is bound into the confirm
token, sent only while the checkbox is shown, and **cleared after every dispatch** so it cannot
silently carry over. When it actually bypasses a closed window, `ActionPlan.window_overridden` is
set and every opening audit record carries `windowOverride: true` (omitted otherwise, so ordinary
records keep their shape); the audit trail shows "Live (window override)".

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

Job state lives in `AppState.jobs` (methods in `state/jobs.rs`), mirroring `last_result` — a
tenant switch reads as a miss. The poller is single-claim (`try_claim_job_poller`) and emits `action:progress` (no capability change
needed; `core:event:default` already covers it). It retires via `release_job_poller_if_idle()`,
which re-checks for pending jobs **and** clears the claim flag under the jobs lock. Dispatch
appends its jobs before calling `try_claim_job_poller`, so a batch landing during shutdown is
either seen (the poller keeps going) or strictly after the release (its own claim succeeds).
Releasing unconditionally left jobs dispatched in that gap with no poller at all.

The tenant stamp cannot see a sign-out and sign-in on the same instance, so the write path also
carries a **`JobSession`** (tenant + jobs epoch; `clear_jobs` bumps the epoch before clearing).
`run_action` samples it before `build_plan`; `dispatch_one` checks it after acquiring its permit
and records a device still queued when the session ended as `Skipped` ("not sent") instead of
POSTing it; and `append_jobs` re-checks it under the jobs lock, refusing the batch so `run_action`
returns an error instead of the batch. All of these used to read the tenant only at store time,
after the dispatch, so a sign-out mid-batch kept POSTing the queued devices and landed the
departed session's jobs in the new one, where the poller resolved them against the new session's
API and invalidated its caches. The per-device `action:progress` emit is gated the same way
(`dispatch::device_progress`): the frontend merges each event's rows into a Jobs list that
`clear_session()` has already handed to the next session. The refusal is
`UiError::coded(ERR_PARTIAL_DISPATCH, …)`. The frontend closes the confirmation and shows it as
a toast. It never keeps it in the dialog, because the dialog's Re-plan would send to devices
that already acted, and `clear_session()` has usually closed the dialog anyway. The Jobs tab's Clear is `clear_job_history`: it empties the list
without ending the session.

The same check also runs **before every attempt of the POST**, retries included. The dispatch
client is `state.api.with_send_guard(still_current)`, and `send_with_retry` asks the guard for an
`ActOnce` request after it has the token, then fails with `api::SendRefused` (recorded as "not
sent") if the session ended. A 429 parks a POST for up to 60 s per retry and a 401 re-sends at
once, each time reading the token live from the shared `AuthState`. Before this, a sign-out and
another operator's sign-in during that wait re-sent the departed session's action under the new
operator's grant, and a tenant switch sent it to the old instance with the new grant. Every
earlier attempt was a definite rejection, so "not sent" is accurate.

**Residual window:** the guard is asked once per attempt, before `send()`. A session that ends
after that check and before the request reaches the server still sends that one attempt. It
uses the token and URL read under the departed session, so it acts with that session's own
authority. Nothing can close that gap from the client side.

The poller takes the session from `pending_jobs` alongside the rows, and `settle_tick` applies
under it: `apply_job_updates` returns the ids it applied, and only those are emitted. A tick that
spans a sign-out used to emit the old rows, which the frontend merged into the next operator's Jobs
tab. Invalidation follows the *read*, not the row. It happens when the row was applied or the
tenant is unchanged: after a same-instance sign-in the device really did change, and the next
session's caches hold that same fleet. It never happens across a tenant switch, where the verdict
came from the other instance and the caches belong to it. The tick writes a closing audit record
only for an **applied** row, labelled with the session's instance and client id (not Settings
read now). The Jobs tab's Clear keeps unsettled rows, so a row that is not applied after its
append was removed by the session ending. That means `clear_jobs` already closed it as
unresolved.

**Every opening "dispatching" audit record gets a close**, even when the session ends first. The
close is `AuditEntry::unresolved` ("unresolved: session ended before the outcome was known", with
no activity id or exit code) in two places, so each job gets at most one. `clear_jobs` returns one
for each unsettled row it drops, and the async callers write it off the runtime. Every tenant
switch runs `clear_jobs`, so this also covers a tick that spans a switch. `run_action` writes one
for each unsettled row of a batch refused at `append_jobs`. A tick racing a sign-out closes the
job exactly once either way. If the tick applies first, the row is terminal and `clear_jobs` skips
it. If the clear runs first, the row is not applied and the tick writes nothing.

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

### One read per device per tick

`poller::feed_reads` plans a tick's `/activities` reads **per device**, floored at the earliest
pending dispatch on it (less the 5 s skew allowance), and `resolve_pending` hands every job on
that device the same list. It used to be one read per *job*, so a device carrying a scan, an
apply and a reboot was asked for the same feed three times a tick. Correlation still runs in
`pending` order with `claimed` threaded through, so two jobs on one device never bind the same
activity; `commands::actions::tests` pins both with wiremock.

A read is narrowed with the documented `seriesUid` parameter only when the device has exactly one
pending job **and** that job's series uid has already been seen on an activity in its feed
(`confirmed_series`, held by the poller task). A dispatch response's uid alone is not proof:
`parse_dispatch_response` takes a bare `uid` as a last resort, which may be an echoed script uid
no activity carries, and a read narrowed to it would starve the job until its timeout. The device
`df` is always sent too, so `seriesUid` only ever narrows — a tenant that ignored it must not widen
the read to the fleet.

### `newerThan` is an activity ID, not a timestamp

The dispatch-time floor is applied **client-side** in `api::activities` against `activityTime`.
Sending a Unix timestamp there asked for activities newer than an id beyond any real one, so the
feed came back empty every poll — and an empty feed reads as "the feed lags", so every dispatch
resolved by timeout instead. The endpoint's date parameters are `after`/`before`, whose format the
spec never states; don't guess.

### The activity-type filter must list what the native endpoints emit

`scan`/`apply`/`reboot` return no correlator, so the third-tier heuristic is their only path to
resolving, and `is_action_activity(kind, type)` accepts only what *that kind* emits: an OS
scan/apply `PATCH_MANAGEMENT`, a software one `SOFTWARE_PATCH_MANAGEMENT`, a reboot `SYSTEM`, and
every script-running kind `SCRIPTING` (the spec's value; `SCRIPT` is not in the enum but is kept
anyway) or the `ACTION`/`ACTIONSET` pair for a built-in. It used to be one list for every kind,
including `CONDITION_ACTION`/`CONDITION_ACTIONSET` and `SCHEDULED_TASK` — NinjaOne's own policy and
scheduler runs — so a condition firing after a dispatch, an unrelated `SYSTEM` event, or a
software apply could resolve a job with somebody else's verdict.

## The audit log redacts credentials in every shape a script takes them

`audit::redact_parameters` redacts the value of a sensitive `key=value`, a sensitive `-Flag value`
pair, and PowerShell's inline `-Password:value` (`:` is a separator as well as `=`; it used to be
read as one bare flag, so the credential was written verbatim and the *next* token redacted
instead). A quoted value is redacted through its closing quote — split on whitespace,
`-Password "a b"` is several tokens — and an unterminated quote swallows the rest of the line.

A name is sensitive when, lowercased with punctuation dropped, it *contains* `pass`, `pw`, `key`,
`cred`, `secret`, `token`, `authorization`, `bearer` or `connectionstring`, or *ends with* `auth`
or `sas`, unless the whole name is a known benign one (`passthru`, `registrykey`, `regkey`,
`subkey`, `keypath`, `bypass`). The rule is never to redact less than before: `key` and `pass`
stay substrings because `-Key1`, `-StorageKeys` and `-AdminPass2` are credentials, and ordinary
flags they catch are exempted one whole name at a time. `auth` is a suffix so `-Author` and
`-Authentication Kerberos` survive.

The key/value split stops at the first `=` or `:`, so a credential can sit *inside* a value
whose own key is innocent: `conn=Server=a;Password=x`, `/p:Password=x`, `https://user:pass@host`
(keyed on `https`). Every token that is not already redacted is also scanned for URL userinfo
(the password after `user:` is redacted; userinfo with no `:` is a token standing in for the
user name and is redacted whole) and for `;`/`&`/`?`-separated segments. Within a segment every
name up to an `=` is judged (`conn=Pwd=x` is `conn` then `Pwd`); after a `?`/`&`/`;`, `sig` (a SAS
signature) counts too. A value that is only `=` is base64 padding and is left alone. When such a
redaction lands inside a quoted run — opened by this token or by an earlier, unredacted one
(`-Conn "Server=a; Password=a b"`), or sitting on the key itself (`"Pwd=a b;Server=x"`) — the
rest of the run is swallowed through its closing quote. A segment with an empty value
(`conn=a;Password= x`) owes the next token, as `-Password x` does, and a quoted flag name
(`"-Password" x`) is still a flag.

Redaction is by name, so some shapes reach the log as typed:

- a positional credential (`Set-Thing hunter2`, or a bare `password hunter2` with no flag
  marker or separator) carries no name the scanner can judge;
- `user:pass@host` without a `scheme://` (`-Remote user:pw@host`, `-u user:pass`) is not
  recognised as userinfo;
- a URL password containing an unencoded `/` ends the authority early, so the `@` is never seen
  (`https://u:p/ss@h`); percent-encoded passwords are fine.

Scripts that take a secret should take it as a named parameter.
