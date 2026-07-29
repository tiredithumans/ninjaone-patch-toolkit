use leptos::prelude::*;

use super::*;

#[component]
pub(crate) fn SettingsPanel() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Two-click confirm for "Clear stored secret": the first click arms the
    // button, the second fires; leaving or blurring it disarms.
    let clear_armed = RwSignal::new(false);

    let save = move |_| {
        let args = SaveSettingsArgs {
            instance_base_url: state.settings.f_instance.get_untracked(),
            client_id: non_empty(state.settings.f_client_id.get_untracked()),
            callback_port: state.settings.f_port.get_untracked(),
            install_window_days: state.settings.f_install_days.get_untracked(),
            sla_days: state.settings.f_sla.get_untracked(),
            client_secret: non_empty(state.settings.f_client_secret.get_untracked()),
            clear_secret: false,
            auto_check_updates: state.settings.f_auto_update.get_untracked(),
            actions: state.settings.f_actions.get_untracked(),
        };
        spawn_local(async move {
            match api::save_settings(args).await {
                Ok(v) => {
                    state.apply_settings_view(v);
                    state.settings.f_client_secret.set(String::new());
                    state.session.refresh_auth();
                    state.notify(Toast::ok("Settings saved"));
                }
                Err(e) => state.notify(Toast::err(e)),
            }
        });
    };

    let clear_secret = move |_| {
        spawn_local(async move {
            let args = SaveSettingsArgs {
                instance_base_url: state.settings.f_instance.get_untracked(),
                client_id: non_empty(state.settings.f_client_id.get_untracked()),
                callback_port: state.settings.f_port.get_untracked(),
                install_window_days: state.settings.f_install_days.get_untracked(),
                sla_days: state.settings.f_sla.get_untracked(),
                client_secret: None,
                clear_secret: true,
                auto_check_updates: state.settings.f_auto_update.get_untracked(),
                actions: state.settings.f_actions.get_untracked(),
            };
            match api::save_settings(args).await {
                Ok(v) => {
                    state.apply_settings_view(v);
                    state.notify(Toast::ok("Cleared stored secret"));
                }
                Err(e) => state.notify(Toast::err(e)),
            }
        });
    };

    let check_now = move |_| {
        if state.updates.update_busy.get_untracked() {
            return;
        }
        state.updates.update_busy.set(true);
        spawn_local(async move {
            match api::check_for_update().await {
                Ok(Some(info)) => state.updates.update.set(Some(info)),
                Ok(None) => state.notify(Toast::ok("You're on the latest version")),
                Err(e) => state.notify(Toast::err(e)),
            }
            state.updates.update_busy.set(false);
        });
    };

    view! {
        <section class="panel settings">
            <h2>"Connection"</h2>
            <div class="grid">
                <label>
                    "Region / Instance"
                    <select on:change=move |ev| state.settings.f_instance.set(event_target_value(&ev))>
                        {REGIONS
                            .iter()
                            .map(|(url, label)| {
                                let url = url.to_string();
                                let sel = {
                                    let url = url.clone();
                                    move || state.settings.f_instance.get() == url
                                };
                                view! {
                                    <option value=url.clone() selected=sel>
                                        {label.to_string()}
                                    </option>
                                }
                            })
                            .collect_view()}
                        <option value="">"Custom…"</option>
                    </select>
                </label>
                <label>
                    "Instance URL"
                    <input
                        prop:value=move || state.settings.f_instance.get()
                        on:input=move |ev| state.settings.f_instance.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    "Client ID"
                    <input
                        prop:value=move || state.settings.f_client_id.get()
                        on:input=move |ev| state.settings.f_client_id.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    {move || {
                        if state.settings.has_secret.get() {
                            "Client secret (leave blank to keep)"
                        } else {
                            "Client secret (Native apps have none)"
                        }
                    }}
                    <input
                        type="password"
                        prop:value=move || state.settings.f_client_secret.get()
                        on:input=move |ev| state.settings.f_client_secret.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    "Callback port"
                    <input
                        type="number"
                        min="1024"
                        max="65535"
                        prop:value=move || state.settings.f_port.get().to_string()
                        on:change=move |ev| {
                            let v = event_target_value(&ev)
                                .parse::<u16>()
                                .unwrap_or_else(|_| state.settings.f_port.get_untracked());
                            state.settings.f_port.set(v.clamp(1024, 65535));
                        }
                    />
                </label>
                <label>
                    "Install history window (days)"
                    <input
                        type="number"
                        min="1"
                        max="3650"
                        prop:value=move || state.settings.f_install_days.get().to_string()
                        on:change=move |ev| {
                            let v = event_target_value(&ev)
                                .parse::<i64>()
                                .unwrap_or_else(|_| state.settings.f_install_days.get_untracked());
                            state.settings.f_install_days.set(v.clamp(1, 3650));
                        }
                    />
                </label>
                <label>
                    "SLA window for aged criticals (days)"
                    <input
                        type="number"
                        min="1"
                        max="3650"
                        prop:value=move || state.settings.f_sla.get().to_string()
                        on:change=move |ev| {
                            let v = event_target_value(&ev)
                                .parse::<i64>()
                                .unwrap_or_else(|_| state.settings.f_sla.get_untracked());
                            state.settings.f_sla.set(v.clamp(1, 3650));
                        }
                    />
                </label>
                <label class="inline">
                    <input
                        type="checkbox"
                        prop:checked=move || state.settings.f_auto_update.get()
                        on:change=move |ev| state.settings.f_auto_update.set(event_target_checked(&ev))
                    />
                    "Automatically check for updates on launch"
                </label>
            </div>
            <ActionSettingsFields/>
            <div class="row">
                <button class="btn btn-primary" on:click=save>
                    "Save settings"
                </button>
                <Show when=move || state.settings.has_secret.get()>
                    <button
                        class=move || {
                            if clear_armed.get() { "btn btn-ghost btn-armed" } else { "btn btn-ghost" }
                        }
                        on:click=move |ev| {
                            if clear_armed.get_untracked() {
                                clear_armed.set(false);
                                clear_secret(ev);
                            } else {
                                clear_armed.set(true);
                            }
                        }
                        on:mouseleave=move |_| clear_armed.set(false)
                        on:blur=move |_| clear_armed.set(false)
                    >
                        {move || {
                            if clear_armed.get() {
                                "Really clear? Click again"
                            } else {
                                "Clear stored secret"
                            }
                        }}
                    </button>
                </Show>
                <button
                    class="btn btn-ghost"
                    prop:disabled=move || state.updates.update_busy.get()
                    on:click=check_now
                >
                    {move || {
                        if state.updates.update_busy.get() { "Checking…" } else { "Check for updates" }
                    }}
                </button>
            </div>
            <p class="app-version">
                {concat!("NinjaOne Patch Toolkit v", env!("CARGO_PKG_VERSION"))}
            </p>
        </section>
    }
}

/// The write-path guardrail knobs.
///
/// Every value here is re-validated and re-enforced by the backend; the panel only
/// decides what the operator is offered. Enabling actions also changes the OAuth
/// scope requested at the *next* sign-in, which is why the hint below points at
/// re-authorization rather than implying the toggle alone is enough.
#[component]
fn ActionSettingsFields() -> impl IntoView {
    let state = expect_context::<AppState>();
    let a = state.settings.f_actions;

    // Each control edits one field of the block, leaving the rest untouched, so a
    // setting the panel doesn't surface survives a save.
    let enabled = move || a.with(|s| s.enabled);

    view! {
        <fieldset class="settings-actions">
            <legend>"Patch actions"</legend>
            <p class="settings-hint">
                "Off by default. Turning this on makes the next sign-in request the "
                <strong>"Management"</strong>
                " scope, which the patch/reboot/script endpoints require — enable that scope on the API app in NinjaOne, then Re-authorize."
            </p>

            <label class="inline">
                <input
                    type="checkbox"
                    prop:checked=enabled
                    on:change=move |ev| {
                        let on = event_target_checked(&ev);
                        a.update(|s| s.enabled = on);
                    }
                />
                "Enable patch actions (apply, reboot, run scripts)"
            </label>

            <div class="row" class:settings-disabled=move || !enabled()>
                <label>
                    "Max devices per action"
                    <input
                        type="number"
                        min="1"
                        max="500"
                        prop:disabled=move || !enabled()
                        prop:value=move || a.with(|s| s.max_devices_per_action).to_string()
                        on:change=move |ev| {
                            if let Ok(v) = event_target_value(&ev).parse::<usize>() {
                                a.update(|s| s.max_devices_per_action = v.clamp(1, 500));
                            }
                        }
                    />
                </label>
                <label>
                    "Max organizations per action"
                    <input
                        type="number"
                        min="1"
                        max="50"
                        prop:disabled=move || !enabled()
                        prop:value=move || a.with(|s| s.max_orgs_per_action).to_string()
                        on:change=move |ev| {
                            if let Ok(v) = event_target_value(&ev).parse::<usize>() {
                                a.update(|s| s.max_orgs_per_action = v.max(1));
                            }
                        }
                    />
                </label>
                <label>
                    "Dispatch concurrency"
                    <input
                        type="number"
                        min="1"
                        max="16"
                        prop:disabled=move || !enabled()
                        prop:value=move || a.with(|s| s.concurrency).to_string()
                        on:change=move |ev| {
                            if let Ok(v) = event_target_value(&ev).parse::<usize>() {
                                a.update(|s| s.concurrency = v.clamp(1, 16));
                            }
                        }
                    />
                </label>
                <label>
                    "Run as"
                    <input
                        type="text"
                        placeholder="system"
                        prop:disabled=move || !enabled()
                        prop:value=move || a.with(|s| s.run_as.clone())
                        on:input=move |ev| {
                            let v = event_target_value(&ev);
                            a.update(|s| s.run_as = v);
                        }
                    />
                </label>
            </div>

            <div class="row" class:settings-disabled=move || !enabled()>
                <label>
                    "OS remediation script ID"
                    <input
                        type="number"
                        min="1"
                        placeholder="from the library URL"
                        prop:disabled=move || !enabled()
                        prop:value=move || {
                            a.with(|s| s.os_patch_script_id).map(|v| v.to_string()).unwrap_or_default()
                        }
                        on:change=move |ev| {
                            let v = event_target_value(&ev).parse::<i64>().ok();
                            a.update(|s| s.os_patch_script_id = v);
                        }
                    />
                </label>
                <label>
                    "Software remediation script ID"
                    <input
                        type="number"
                        min="1"
                        prop:disabled=move || !enabled()
                        prop:value=move || {
                            a.with(|s| s.software_patch_script_id)
                                .map(|v| v.to_string())
                                .unwrap_or_default()
                        }
                        on:change=move |ev| {
                            let v = event_target_value(&ev).parse::<i64>().ok();
                            a.update(|s| s.software_patch_script_id = v);
                        }
                    />
                </label>
            </div>
            <p class="settings-hint">
                "NinjaOne has no script-upload API, so add the script by hand under Administration → Library → Automation and copy the numeric ID out of its URL."
            </p>

            <label class="inline" class:settings-disabled=move || !enabled()>
                <input
                    type="checkbox"
                    prop:disabled=move || !enabled()
                    prop:checked=move || a.with(|s| s.allow_offline_targets)
                    on:change=move |ev| {
                        let on = event_target_checked(&ev);
                        a.update(|s| s.allow_offline_targets = on);
                    }
                />
                "Allow offline devices as targets (NinjaOne queues the action until they reconnect)"
            </label>
            <label class="inline" class:settings-disabled=move || !enabled()>
                <input
                    type="checkbox"
                    prop:disabled=move || !enabled()
                    prop:checked=move || a.with(|s| s.require_maintenance_window)
                    on:change=move |ev| {
                        let on = event_target_checked(&ev);
                        a.update(|s| s.require_maintenance_window = on);
                    }
                />
                "Only allow changes inside a maintenance window"
            </label>
            <label class="inline" class:settings-disabled=move || !enabled()>
                <input
                    type="checkbox"
                    prop:disabled=move || !enabled()
                    prop:checked=move || a.with(|s| s.allow_window_override)
                    on:change=move |ev| {
                        let on = event_target_checked(&ev);
                        a.update(|s| s.allow_window_override = on);
                    }
                />
                "Allow overriding the maintenance window"
            </label>
        </fieldset>
    }
}
