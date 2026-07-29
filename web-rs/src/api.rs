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

// --- Auth --------------------------------------------------------------------

pub async fn auth_status() -> Result<AuthStatus, String> {
    invoke("auth_status", no_args()).await
}

pub async fn sign_in() -> Result<(), String> {
    invoke("sign_in", no_args()).await
}

pub async fn sign_out() -> Result<(), String> {
    invoke("sign_out", no_args()).await
}

// --- Lookups -----------------------------------------------------------------

pub async fn list_orgs() -> Result<Vec<Organization>, String> {
    invoke("list_orgs", no_args()).await
}

pub async fn list_locations(org_id: i64) -> Result<Vec<Location>, String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        org_id: i64,
    }
    invoke("list_locations", args_of(&Args { org_id })).await
}

pub async fn list_roles() -> Result<Vec<Role>, String> {
    invoke("list_roles", no_args()).await
}

pub async fn list_node_classes() -> Result<Vec<NodeClass>, String> {
    invoke("list_node_classes", no_args()).await
}

// --- Patches + export --------------------------------------------------------

/// Runs a patch query. `force_refresh` (an auto-refresh tick or the manual ↻) tells
/// the backend to refetch the whole-fleet patch data; a normal Run query / re-filter
/// leaves it `false` so the cached fleet is re-scoped client-side with no round trip.
pub async fn query_patches(
    args: PatchQueryArgs,
    query_id: u64,
    force_refresh: bool,
) -> Result<QueryResult, String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Wrap {
        args: PatchQueryArgs,
        query_id: u64,
        force_refresh: bool,
    }
    invoke(
        "query_patches",
        args_of(&Wrap {
            args,
            query_id,
            force_refresh,
        }),
    )
    .await
}

/// Fetches one page of detail rows from the backend's cached query result. The
/// full row set lives in the backend cache (not shipped over IPC), so the table
/// pages a large fleet by requesting just the visible window. `sort` re-orders
/// the paged view backend-side; `None` is the canonical cache order.
pub async fn get_patch_rows(
    offset: usize,
    limit: usize,
    sort: Option<RowSort>,
) -> Result<Vec<PatchRow>, String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Wrap {
        offset: usize,
        limit: usize,
        sort: Option<RowSort>,
    }
    invoke(
        "get_patch_rows",
        args_of(&Wrap {
            offset,
            limit,
            sort,
        }),
    )
    .await
}

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

pub async fn export_patches() -> Result<Option<String>, String> {
    invoke("export_patches_xlsx", no_args()).await
}

/// Writes the cached query result as a self-contained HTML executive report
/// (compliance/severity/age charts + failure & reboot tables) the operator can
/// print to PDF. Like the Excel export, backend-only — inert in browser/demo mode.
pub async fn export_report() -> Result<Option<String>, String> {
    invoke("export_report_html", no_args()).await
}

// --- Device actions ----------------------------------------------------------

/// Forces a fresh OAuth consent so the grant can pick up the `management` scope.
/// The refresh grant never re-sends `scope`, so an install that signed in before
/// patch actions were enabled keeps its read-only grant until this runs.
pub async fn reauthorize() -> Result<(), String> {
    invoke("reauthorize", no_args()).await
}

/// Reports what an action would do — eligible/skipped devices, warnings, hard
/// blockers, the literal parameter string — and issues a confirmation token bound
/// to exactly this request.
pub async fn plan_action(request: ActionRequest) -> Result<ActionPlan, String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        request: ActionRequest,
    }
    invoke("plan_action", args_of(&Args { request })).await
}

/// Dispatches the action. The backend re-plans and re-checks every guardrail, so a
/// request that skipped `plan_action` (or whose selection changed since) is
/// refused rather than trusted.
pub async fn run_action(request: ActionRequest) -> Result<ActionBatch, String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        request: ActionRequest,
    }
    invoke("run_action", args_of(&Args { request })).await
}

pub async fn list_jobs() -> Result<Vec<JobReport>, String> {
    invoke("list_jobs", no_args()).await
}

pub async fn clear_jobs() -> Result<Vec<JobReport>, String> {
    invoke("clear_jobs", no_args()).await
}

pub async fn list_scripts() -> Result<Vec<ScriptSummary>, String> {
    invoke("list_scripts", no_args()).await
}

pub async fn list_run_as_options(device_id: i64) -> Result<RunAsOptions, String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        device_id: i64,
    }
    invoke("list_run_as_options", args_of(&Args { device_id })).await
}

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

pub async fn check_for_update() -> Result<Option<UpdateInfo>, String> {
    invoke("check_for_update", no_args()).await
}

pub async fn install_update() -> Result<(), String> {
    invoke("install_update", no_args()).await
}

// --- Settings + presets ------------------------------------------------------

pub async fn get_settings() -> Result<SettingsView, String> {
    invoke("get_settings", no_args()).await
}

pub async fn save_settings(args: SaveSettingsArgs) -> Result<SettingsView, String> {
    #[derive(Serialize)]
    struct Wrap {
        args: SaveSettingsArgs,
    }
    invoke("save_settings", args_of(&Wrap { args })).await
}

pub async fn save_preset(preset: Preset) -> Result<Vec<Preset>, String> {
    #[derive(Serialize)]
    struct Wrap {
        preset: Preset,
    }
    invoke("save_preset", args_of(&Wrap { preset })).await
}

pub async fn delete_preset(name: String) -> Result<Vec<Preset>, String> {
    #[derive(Serialize)]
    struct Wrap {
        name: String,
    }
    invoke("delete_preset", args_of(&Wrap { name })).await
}
