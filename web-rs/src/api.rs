//! Typed wrappers around the Tauri IPC bridge. Uses the global `window.__TAURI__`
//! object (enabled via `withGlobalTauri`) to avoid an external bindings crate.

use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::*;

use crate::types::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke, catch)]
    async fn tauri_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen)]
    fn tauri_listen(event: &str, handler: &JsValue) -> JsValue;
}

/// Whether the app is running inside the Tauri webview rather than a plain browser
/// (e.g. the GitHub Pages demo). The desktop build injects `window.__TAURI__` via
/// `withGlobalTauri`; a browser has no backend, so the frontend must skip every IPC
/// call and fall back to demo data instead of throwing on an undefined global.
pub fn is_tauri() -> bool {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__TAURI__"))
        .map(|v| !v.is_undefined() && !v.is_null())
        .unwrap_or(false)
}

/// Whether this bundle was built to be loaded by the Tauri webview, as opposed to
/// served as the browser demo.
///
/// [`is_tauri`] can only report what it *found*; it cannot report what the build
/// *expected*, and that difference is a safety property. Without it the absence of
/// `__TAURI__` is indistinguishable from "this is the demo", so the one un-retried
/// `Reflect::get` at startup is the only thing standing between a patching operator
/// and a screen of invented orgs, devices and patches. Tauri injects the global
/// before page script so the window is not currently reachable — but "not currently
/// reachable" is not a guarantee to rest a fleet decision on, and the failure is
/// silent apart from a banner.
///
/// Set by `tauri.conf.json`'s `beforeDevCommand` / `beforeBuildCommand`, which pass
/// `--features desktop` to Trunk. The browser and GitHub Pages builds
/// (`just web-build`, `just web-build-pages`) deliberately do not.
pub const fn is_desktop_build() -> bool {
    cfg!(feature = "desktop")
}

/// Whether the document is currently hidden (window minimized, or the tab in the
/// background). Used to skip auto-refresh ticks nobody is there to read. Falls back
/// to `false` — a missing `document` must never *suppress* a refresh, only ever fail
/// to skip one.
pub fn document_hidden() -> bool {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("document"))
        .and_then(|doc| js_sys::Reflect::get(&doc, &JsValue::from_str("hidden")))
        .map(|v| v.as_bool().unwrap_or(false))
        .unwrap_or(false)
}

/// Reads a remembered UI preference, `None` when the operator has never set one.
///
/// `localStorage`, not `settings.json`: this is a per-machine view convenience, not
/// configuration. Routing one boolean through the settings schema would mean a
/// backend field, a `SaveSettingsArgs` field, a `SettingsView` field, a mirror in
/// `types.rs` and an IPC round trip on every toggle. Lives here beside
/// [`document_hidden`] because it touches `js_sys`, and `util` is JS-free by rule.
///
/// Every access is fallible and every failure reads as "no preference": a webview
/// with site data blocked throws on the accessor itself, and the correct response is
/// the default view, never a panic.
pub fn ui_pref(key: &str) -> Option<bool> {
    match ui_pref_str(key)?.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Records a UI preference. Best-effort — a failure costs the operator a remembered
/// panel state and nothing else.
pub fn set_ui_pref(key: &str, value: bool) {
    set_ui_pref_str(key, if value { "true" } else { "false" });
}

/// [`ui_pref`] for a string value (the theme, the hidden-column list). Same
/// failure rule: anything that throws is "never set".
pub fn ui_pref_str(key: &str) -> Option<String> {
    let storage =
        js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("localStorage")).ok()?;
    let get = js_sys::Reflect::get(&storage, &JsValue::from_str("getItem")).ok()?;
    let f = get.dyn_ref::<js_sys::Function>()?;
    f.call1(&storage, &JsValue::from_str(key)).ok()?.as_string()
}

/// [`set_ui_pref`] for a string value.
pub fn set_ui_pref_str(key: &str, value: &str) {
    let Ok(storage) = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("localStorage"))
    else {
        return;
    };
    let Ok(set) = js_sys::Reflect::get(&storage, &JsValue::from_str("setItem")) else {
        return;
    };
    if let Some(f) = set.dyn_ref::<js_sys::Function>() {
        let _ = f.call2(&storage, &JsValue::from_str(key), &JsValue::from_str(value));
    }
}

/// Storage key for the Filters panel's collapsed state.
pub const PREF_FILTERS_COLLAPSED: &str = "npt.filtersCollapsed";
/// Storage key for the colour theme (`system` / `light` / `dark`).
pub const PREF_THEME: &str = "npt.theme";
/// Storage key for the Patches table's hidden columns (a JSON list of column ids).
pub const PREF_PATCH_COLUMNS: &str = "npt.columns.patches";

/// Pins the colour theme on `<html data-theme="…">`, or removes the attribute so
/// `prefers-color-scheme` decides.
pub fn set_root_theme(attr: Option<&str>) {
    let Some(root) = leptos::prelude::document().document_element() else {
        return;
    };
    let _ = match attr {
        Some(value) => root.set_attribute("data-theme", value),
        None => root.remove_attribute("data-theme"),
    };
}

/// Copies `text` through the async Clipboard API. `Err` when the API is missing or
/// refuses (no user gesture, permission denied) — the caller keeps the text on
/// screen in a selectable field either way, so a refusal costs one manual copy.
///
/// No Tauri capability is involved: this is the webview's own web API, and the CSP
/// governs what the page loads and fetches, not the clipboard.
pub async fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let refused = || "the clipboard is not available here".to_string();
    let clipboard = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("navigator"))
        .and_then(|nav| js_sys::Reflect::get(&nav, &JsValue::from_str("clipboard")))
        .map_err(|_| refused())?;
    let write = js_sys::Reflect::get(&clipboard, &JsValue::from_str("writeText"))
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .ok_or_else(refused)?;
    let promise = write
        .call1(&clipboard, &JsValue::from_str(text))
        .ok()
        .and_then(|p| p.dyn_into::<js_sys::Promise>().ok())
        .ok_or_else(refused)?;
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map(|_| ())
        .map_err(|_| refused())
}

/// The page URL's fragment without the `#`; `""` when there is none.
pub fn location_fragment() -> String {
    leptos::prelude::location()
        .hash()
        .unwrap_or_default()
        .trim_start_matches('#')
        .to_string()
}

/// The page URL as the browser shows it.
pub fn location_href() -> String {
    leptos::prelude::location().href().unwrap_or_default()
}

/// Replaces the URL fragment without adding a history entry — a view that changes
/// on every filter click must not turn Back into an undo stack of checkboxes.
pub fn replace_fragment(fragment: &str) {
    let global = js_sys::global();
    let Ok(history) = js_sys::Reflect::get(&global, &JsValue::from_str("history")) else {
        return;
    };
    let Some(replace) = js_sys::Reflect::get(&history, &JsValue::from_str("replaceState"))
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
    else {
        return;
    };
    let _ = replace.call3(
        &history,
        &JsValue::NULL,
        &JsValue::from_str(""),
        &JsValue::from_str(&format!("#{fragment}")),
    );
}

#[derive(serde::Deserialize)]
struct ErrShape {
    message: Option<String>,
    code: Option<String>,
}

/// A command's error with the backend's optional `code` (`UiError::code`) kept, for
/// the few callers that must branch on *which* error it was. Every other wrapper
/// returns only the message.
#[derive(Clone, Debug)]
pub struct IpcError {
    pub message: String,
    pub code: Option<String>,
}

fn ipc_error(err: JsValue) -> IpcError {
    if let Ok(shape) = serde_wasm_bindgen::from_value::<ErrShape>(err.clone())
        && let Some(message) = shape.message
    {
        return IpcError {
            message,
            code: shape.code,
        };
    }
    IpcError {
        message: err
            .as_string()
            .unwrap_or_else(|| "unknown error".to_string()),
        code: None,
    }
}

async fn invoke<R: DeserializeOwned>(cmd: &str, args: JsValue) -> Result<R, String> {
    invoke_coded(cmd, args).await.map_err(|e| e.message)
}

async fn invoke_coded<R: DeserializeOwned>(cmd: &str, args: JsValue) -> Result<R, IpcError> {
    let plain = |message: String| IpcError {
        message,
        code: None,
    };
    // In a plain browser there is no backend; calling the undefined global would
    // throw. Fail cleanly so callers degrade to demo mode instead.
    if !is_tauri() {
        return Err(plain(format!(
            "\"{cmd}\" is only available in the desktop app"
        )));
    }
    match tauri_invoke(cmd, args).await {
        Ok(value) => {
            serde_wasm_bindgen::from_value(value).map_err(|e| plain(format!("decode {cmd}: {e}")))
        }
        Err(err) => Err(ipc_error(err)),
    }
}

/// Serializes a command's arguments the way Tauri will read them: as JSON.
///
/// `json_compatible` because the default serializer turns a map into a JS `Map`,
/// which Tauri's `JSON.stringify` flattens to `{}` — every `device_targets` entry of
/// an `ActionRequest` arrived empty. A failure is surfaced rather than papered over
/// with `undefined`, which the backend would report as a confusing "missing field"
/// on a request that looked complete here.
fn args_of(cmd: &str, value: &impl Serialize) -> Result<JsValue, String> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|e| format!("encode {cmd}: {e}"))
}

fn no_args() -> JsValue {
    JsValue::from(js_sys::Object::new())
}

/// Declares a typed IPC wrapper.
///
/// Every wrapper was the same five lines — a private `Args`/`Wrap` struct, its
/// `#[serde(rename_all = "camelCase")]`, and the `invoke` call — restated per
/// command. Beyond the repetition, writing it out by hand left two things free to
/// drift that must not: the argument keys have to equal the Rust handler's
/// parameter names in camelCase, and the invoked string has to name a registered
/// command. Here the struct fields *are* the wrapper's parameters, and the command
/// name defaults to the wrapper's own name, so both hold by construction.
///
/// The two commands whose wrapper reads better under a shorter name
/// (`export_patches` → `export_patches_xlsx`) spell the target out with `as`.
macro_rules! ipc {
    // Zero-argument command, named after the wrapper.
    ($(#[$meta:meta])* $name:ident() -> $ret:ty) => {
        ipc!($(#[$meta])* $name as stringify!($name), () -> $ret);
    };
    // Zero-argument command under an explicit backend name.
    ($(#[$meta:meta])* $name:ident as $cmd:expr, () -> $ret:ty) => {
        $(#[$meta])*
        pub async fn $name() -> Result<$ret, String> {
            invoke($cmd, no_args()).await
        }
    };
    ($(#[$meta:meta])* $name:ident as $cmd:expr, ($($arg:ident: $ty:ty),+ $(,)?) -> $ret:ty) => {
        $(#[$meta])*
        pub async fn $name($($arg: $ty),+) -> Result<$ret, String> {
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Args { $($arg: $ty),+ }
            invoke($cmd, args_of($cmd, &Args { $($arg),+ })?).await
        }
    };
    // Command taking one or more arguments, named after the wrapper.
    ($(#[$meta:meta])* $name:ident($($arg:ident: $ty:ty),+ $(,)?) -> $ret:ty) => {
        ipc!($(#[$meta])* $name as stringify!($name), ($($arg: $ty),+) -> $ret);
    };
    // `coded`: the same, but the error keeps the backend's `code` ([`IpcError`]).
    ($(#[$meta:meta])* coded $name:ident($($arg:ident: $ty:ty),+ $(,)?) -> $ret:ty) => {
        $(#[$meta])*
        pub async fn $name($($arg: $ty),+) -> Result<$ret, IpcError> {
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Args { $($arg: $ty),+ }
            let cmd = stringify!($name);
            let args = args_of(cmd, &Args { $($arg),+ }).map_err(|message| IpcError {
                message,
                code: None,
            })?;
            invoke_coded(cmd, args).await
        }
    };
}

// --- Auth --------------------------------------------------------------------

ipc!(auth_status() -> AuthStatus);
ipc!(sign_in() -> ());
ipc!(sign_out() -> ());

// --- Lookups -----------------------------------------------------------------

ipc!(list_orgs() -> Vec<Organization>);
ipc!(list_locations(org_ids: Vec<i64>) -> Vec<Location>);
ipc!(list_roles() -> Vec<Role>);
ipc!(list_node_classes() -> Vec<NodeClass>);

// --- Patches + export --------------------------------------------------------

ipc!(
    /// Runs a patch query. `force_refresh` (an auto-refresh tick or the manual ↻) tells
    /// the backend to refetch the whole-fleet patch data; a normal Run query / re-filter
    /// leaves it `false` so the cached fleet is re-scoped client-side with no round trip.
    query_patches(args: PatchQueryArgs, query_id: u64, force_refresh: bool) -> QueryResult
);

ipc!(
    /// Fetches one page of detail rows from the backend's cached query result. The
    /// full row set lives in the backend cache (not shipped over IPC), so the table
    /// pages a large fleet by requesting just the visible window. `sort` re-orders
    /// the paged view backend-side; `None` is the canonical cache order.
    get_patch_rows(offset: usize, limit: usize, sort: Option<RowSort>) -> Vec<PatchRow>
);

ipc!(
    /// Fetches one page of **group headers** over the backend's cached rows. Grouping
    /// happens backend-side for the same reason paging does: the frontend only ever
    /// holds one page, so it cannot group a fleet it has never seen.
    get_patch_groups(group_by: GroupBy, offset: usize, limit: usize) -> GroupPage
);

ipc!(
    /// Fetches one page of a single group's member rows. `key` is the opaque
    /// `PatchGroup.key` the backend handed out, so an expand costs no extra state.
    get_patch_group_members(group_by: GroupBy, key: String, offset: usize, limit: usize)
        -> Vec<PatchRow>
);

ipc!(
    /// Fetches several devices' rows in one call, each capped at `limit` and flagged
    /// `truncated` past it — the post-refresh selection prune's read.
    get_device_rows(device_ids: Vec<i64>, limit: usize) -> Vec<crate::types::DeviceRows>
);

/// Subscribes to backend `query:progress` events for the lifetime of the app,
/// decoding each event's payload and handing it to `handler`. The Tauri unlisten
/// handle is intentionally dropped — the subscription lives as long as the app.
pub fn on_query_progress(mut handler: impl FnMut(QueryProgressEvent) + 'static) {
    // No Tauri event bus in a plain browser — skip the subscription rather than
    // call an undefined global at startup.
    if !is_tauri() {
        return;
    }
    let cb = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
        if let Ok(payload) = js_sys::Reflect::get(&event, &JsValue::from_str("payload"))
            && let Ok(ev) = serde_wasm_bindgen::from_value::<QueryProgressEvent>(payload)
        {
            handler(ev);
        }
    });
    let _ = tauri_listen("query:progress", cb.as_ref());
    cb.forget();
}

ipc!(
    /// One device's facts, per-device rollup and detail rows for the drill-down,
    /// read from the backend's cached result. `None` when that result no longer
    /// holds the device (a re-query or tenant switch since the click).
    device_detail(device_id: i64) -> Option<DeviceDetail>
);

ipc!(export_patches as "export_patches_xlsx", () -> Option<String>);

ipc!(
    /// Writes the cached detail rows as a formula-guarded, UTF-8 CSV. Backend-only,
    /// like the other two exports.
    export_csv() -> Option<String>
);

ipc!(
    /// Writes the cached query result as a self-contained HTML executive report
    /// (compliance/severity/age charts + failure & reboot tables) the operator can
    /// print to PDF. Like the Excel export, backend-only — inert in browser/demo mode.
    export_report as "export_report_html", () -> Option<String>
);

// --- Device actions ----------------------------------------------------------

ipc!(
    /// Forces a fresh OAuth consent so the grant can pick up the `management` scope.
    /// The refresh grant never re-sends `scope`, so an install that signed in before
    /// patch actions were enabled keeps its read-only grant until this runs.
    reauthorize() -> ()
);

ipc!(
    /// Reports what an action would do — eligible/skipped devices, warnings, hard
    /// blockers, the literal parameter string — and issues a confirmation token bound
    /// to exactly this request.
    plan_action(request: ActionRequest) -> ActionPlan
);

ipc!(
    /// Dispatches the action. The backend re-plans and re-checks every guardrail, so a
    /// request that skipped `plan_action` (or whose selection changed since) is
    /// refused rather than trusted. `coded`: a partial dispatch must not be re-planned.
    coded
    run_action(request: ActionRequest) -> ActionBatch
);

ipc!(list_jobs() -> Vec<JobReport>);
ipc!(clear_jobs() -> Vec<JobReport>);
ipc!(list_scripts() -> Vec<ScriptSummary>);
ipc!(list_run_as_options(device_id: i64) -> RunAsOptions);

/// Subscribes to backend `action:progress` events. Same lifetime and browser-mode
/// handling as [`on_query_progress`].
pub fn on_action_progress(mut handler: impl FnMut(ActionProgressEvent) + 'static) {
    if !is_tauri() {
        return;
    }
    let cb = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
        if let Ok(payload) = js_sys::Reflect::get(&event, &JsValue::from_str("payload"))
            && let Ok(ev) = serde_wasm_bindgen::from_value::<ActionProgressEvent>(payload)
        {
            handler(ev);
        }
    });
    let _ = tauri_listen("action:progress", cb.as_ref());
    cb.forget();
}

// --- Diagnostics -------------------------------------------------------------

ipc!(
    /// Reads the durable action-audit trail, newest first. The in-memory job list
    /// (`list_jobs`) is per-session and clearable; this is what survives a restart.
    read_action_audit() -> Vec<AuditRecord>
);

ipc!(
    /// Reads the run-history trend, oldest first. One rollup line per completed
    /// query — the only thing in this app with a time dimension.
    read_run_history() -> Vec<RunRecord>
);

ipc!(
    /// Reveals the rolling log directory in the platform file manager and returns
    /// its path, so a bug report can carry evidence. Resolves to the path even when
    /// the reveal itself is what the operator needs to read out.
    open_diagnostics_folder() -> String
);

// --- Updates -----------------------------------------------------------------

ipc!(check_for_update() -> Option<UpdateInfo>);
ipc!(install_update() -> ());

// --- Settings + presets ------------------------------------------------------

ipc!(get_settings() -> SettingsView);
ipc!(save_settings(args: SaveSettingsArgs) -> SettingsView);
ipc!(save_preset(preset: Preset) -> Vec<Preset>);
ipc!(delete_preset(name: String) -> Vec<Preset>);
