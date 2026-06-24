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

pub async fn query_patches(args: PatchQueryArgs) -> Result<QueryResult, String> {
    #[derive(Serialize)]
    struct Wrap {
        args: PatchQueryArgs,
    }
    invoke("query_patches", args_of(&Wrap { args })).await
}

pub async fn export_patches() -> Result<Option<String>, String> {
    invoke("export_patches_xlsx", no_args()).await
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
