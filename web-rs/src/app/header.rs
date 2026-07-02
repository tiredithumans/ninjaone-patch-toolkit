use leptos::prelude::*;

use super::*;

#[component]
pub(crate) fn Header() -> impl IntoView {
    let state = expect_context::<AppState>();
    let authed = move || state.is_authed();
    let instance = move || {
        state
            .session
            .auth
            .get()
            .map(|a| a.instance_base_url)
            .unwrap_or_default()
    };

    view! {
        <header class="topbar">
            <div class="brand">
                <span class="logo">"◆"</span>
                <div>
                    <h1>"NinjaOne Patch Toolkit"</h1>
                    <p class="subtitle">{instance}</p>
                </div>
            </div>
            <div class="actions">
                <Show when=move || state.session.web_mode.get()>
                    <span class="pill pill-demo">"Demo"</span>
                    <a
                        class="btn btn-primary"
                        href=RELEASES_URL
                        target="_blank"
                        rel="noreferrer"
                    >
                        "Get the app ↗"
                    </a>
                </Show>
                <Show when=move || !state.session.web_mode.get()>
                <span class=move || if authed() { "pill pill-on" } else { "pill pill-off" }>
                    {move || if authed() { "Connected" } else { "Not signed in" }}
                </span>
                <button class="btn" on:click=move |_| state.ui.show_settings.update(|s| *s = !*s)>
                    "Settings"
                </button>
                <Show
                    when=move || authed()
                    fallback=move || {
                        view! {
                            <button
                                class="btn btn-primary"
                                prop:disabled=move || state.session.signing_in.get()
                                on:click=move |_| {
                                    if state.session.signing_in.get_untracked() {
                                        return;
                                    }
                                    state.session.signing_in.set(true);
                                    state
                                        .notify(Toast::ok("Complete the sign-in in your browser…"));
                                    spawn_local(async move {
                                        match api::sign_in().await {
                                            Ok(()) => {
                                                state.session.refresh_auth();
                                                state.load_lookups();
                                                state.notify(Toast::ok("Signed in"));
                                            }
                                            Err(e) => state.notify(Toast::err(e)),
                                        }
                                        state.session.signing_in.set(false);
                                    });
                                }
                            >
                                {move || {
                                    if state.session.signing_in.get() { "Signing in…" } else { "Sign in" }
                                }}
                            </button>
                        }
                    }
                >
                    <button
                        class="btn"
                        on:click=move |_| {
                            spawn_local(async move {
                                match api::sign_out().await {
                                    Ok(()) => {
                                        state.session.refresh_auth();
                                        state.notify(Toast::ok("Signed out"));
                                    }
                                    Err(e) => state.notify(Toast::err(e)),
                                }
                            });
                        }
                    >
                        "Sign out"
                    </button>
                </Show>
                </Show>
            </div>
        </header>
    }
}
