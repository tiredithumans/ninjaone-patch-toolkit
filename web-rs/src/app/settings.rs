use leptos::prelude::*;

use super::*;

#[component]
pub(crate) fn SettingsPanel() -> impl IntoView {
    let state = expect_context::<AppState>();

    let save = move |_| {
        let args = SaveSettingsArgs {
            instance_base_url: state.f_instance.get_untracked(),
            client_id: non_empty(state.f_client_id.get_untracked()),
            callback_port: state.f_port.get_untracked(),
            install_window_days: state.f_install_days.get_untracked(),
            sla_days: state.f_sla.get_untracked(),
            client_secret: non_empty(state.f_client_secret.get_untracked()),
            clear_secret: false,
            auto_check_updates: state.f_auto_update.get_untracked(),
        };
        spawn_local(async move {
            match api::save_settings(args).await {
                Ok(v) => {
                    state.apply_settings_view(v);
                    state.f_client_secret.set(String::new());
                    state.refresh_auth();
                    state.notify(Toast::ok("Settings saved"));
                }
                Err(e) => state.notify(Toast::err(e)),
            }
        });
    };

    let clear_secret = move |_| {
        spawn_local(async move {
            let args = SaveSettingsArgs {
                instance_base_url: state.f_instance.get_untracked(),
                client_id: non_empty(state.f_client_id.get_untracked()),
                callback_port: state.f_port.get_untracked(),
                install_window_days: state.f_install_days.get_untracked(),
                sla_days: state.f_sla.get_untracked(),
                client_secret: None,
                clear_secret: true,
                auto_check_updates: state.f_auto_update.get_untracked(),
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
        if state.update_busy.get_untracked() {
            return;
        }
        state.update_busy.set(true);
        spawn_local(async move {
            match api::check_for_update().await {
                Ok(Some(info)) => state.update.set(Some(info)),
                Ok(None) => state.notify(Toast::ok("You're on the latest version")),
                Err(e) => state.notify(Toast::err(e)),
            }
            state.update_busy.set(false);
        });
    };

    view! {
        <section class="panel settings">
            <h2>"Connection"</h2>
            <div class="grid">
                <label>
                    "Region / Instance"
                    <select on:change=move |ev| state.f_instance.set(event_target_value(&ev))>
                        {REGIONS
                            .iter()
                            .map(|(url, label)| {
                                let url = url.to_string();
                                let sel = {
                                    let url = url.clone();
                                    move || state.f_instance.get() == url
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
                        prop:value=move || state.f_instance.get()
                        on:input=move |ev| state.f_instance.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    "Client ID"
                    <input
                        prop:value=move || state.f_client_id.get()
                        on:input=move |ev| state.f_client_id.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    {move || {
                        if state.has_secret.get() {
                            "Client secret (leave blank to keep)"
                        } else {
                            "Client secret (Native apps have none)"
                        }
                    }}
                    <input
                        type="password"
                        prop:value=move || state.f_client_secret.get()
                        on:input=move |ev| state.f_client_secret.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    "Callback port"
                    <input
                        type="number"
                        min="1024"
                        max="65535"
                        prop:value=move || state.f_port.get().to_string()
                        on:change=move |ev| {
                            let v = event_target_value(&ev)
                                .parse::<u16>()
                                .unwrap_or_else(|_| state.f_port.get_untracked());
                            state.f_port.set(v.clamp(1024, 65535));
                        }
                    />
                </label>
                <label>
                    "Install history window (days)"
                    <input
                        type="number"
                        min="1"
                        max="3650"
                        prop:value=move || state.f_install_days.get().to_string()
                        on:change=move |ev| {
                            let v = event_target_value(&ev)
                                .parse::<i64>()
                                .unwrap_or_else(|_| state.f_install_days.get_untracked());
                            state.f_install_days.set(v.clamp(1, 3650));
                        }
                    />
                </label>
                <label>
                    "SLA window for aged criticals (days)"
                    <input
                        type="number"
                        min="1"
                        max="3650"
                        prop:value=move || state.f_sla.get().to_string()
                        on:change=move |ev| {
                            let v = event_target_value(&ev)
                                .parse::<i64>()
                                .unwrap_or_else(|_| state.f_sla.get_untracked());
                            state.f_sla.set(v.clamp(1, 3650));
                        }
                    />
                </label>
                <label class="inline">
                    <input
                        type="checkbox"
                        prop:checked=move || state.f_auto_update.get()
                        on:change=move |ev| state.f_auto_update.set(event_target_checked(&ev))
                    />
                    "Automatically check for updates on launch"
                </label>
            </div>
            <div class="row">
                <button class="btn btn-primary" on:click=save>
                    "Save settings"
                </button>
                <Show when=move || state.has_secret.get()>
                    <button class="btn btn-ghost" on:click=clear_secret>
                        "Clear stored secret"
                    </button>
                </Show>
                <button
                    class="btn btn-ghost"
                    prop:disabled=move || state.update_busy.get()
                    on:click=check_now
                >
                    {move || {
                        if state.update_busy.get() { "Checking…" } else { "Check for updates" }
                    }}
                </button>
            </div>
            <p class="app-version">
                {concat!("NinjaOne Patch Toolkit v", env!("CARGO_PKG_VERSION"))}
            </p>
        </section>
    }
}
