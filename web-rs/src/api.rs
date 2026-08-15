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

#[derive(serde::Deserialize)]
struct ErrShape {
    message: Option<String>,
}

fn error_message(err: JsValue) -> String {
    if let Ok(shape) = serde_wasm_bindgen::from_value::<ErrShape>(err.clone())
        && let Some(message) = shape.message
    {
        return message;
    }
    err.as_string()
        .unwrap_or_else(|| "unknown error".to_string())
}

async fn invoke<R: DeserializeOwned>(cmd: &str, args: JsValue) -> Result<R, String> {
    // In a plain browser there is no backend; calling the undefined global would
    // throw. Fail cleanly so callers degrade to demo mode instead.
    if !is_tauri() {
        return Err(format!("\"{cmd}\" is only available in the desktop app"));
    }
    match tauri_invoke(cmd, args).await {
        Ok(value) => {
            serde_wasm_bindgen::from_value(value).map_err(|e| format!("decode {cmd}: {e}"))
        }
        Err(err) => Err(error_message(err)),
    }
}

fn args_of(value: &impl Serialize) -> JsValue {
    serde_wasm_bindgen::to_value(value).unwrap_or(JsValue::UNDEFINED)
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
            invoke($cmd, args_of(&Args { $($arg),+ })).await
        }
    };
    // Command taking one or more arguments, named after the wrapper.
    ($(#[$meta:meta])* $name:ident($($arg:ident: $ty:ty),+ $(,)?) -> $ret:ty) => {
        ipc!($(#[$meta])* $name as stringify!($name), ($($arg: $ty),+) -> $ret);
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

ipc!(export_patches as "export_patches_xlsx", () -> Option<String>);

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
    /// refused rather than trusted.
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

// --- Updates -----------------------------------------------------------------

ipc!(check_for_update() -> Option<UpdateInfo>);
ipc!(install_update() -> ());

// --- Settings + presets ------------------------------------------------------

ipc!(get_settings() -> SettingsView);
ipc!(save_settings(args: SaveSettingsArgs) -> SettingsView);
ipc!(save_preset(preset: Preset) -> Vec<Preset>);
ipc!(delete_preset(name: String) -> Vec<Preset>);
