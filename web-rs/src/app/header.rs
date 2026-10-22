//! The top bar: what the app is connected to, the run controls, and the menus for
//! everything an operator needs only now and then (exports, the account, theme and
//! view links). It used to be four stacked panels — header, Filters, a run bar
//! wrapping onto two rows, and a view-link row — that pushed the first result row
//! below the fold of the default window.

use leptos::task::spawn_local;

use super::controls::{ExportMenu, QueryProgressLine, RunControls};
use super::menu::MenuButton;
use super::*;

#[component]
pub(crate) fn Header() -> impl IntoView {
    let state = expect_context::<AppState>();
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
                <span class="logo" aria-hidden="true">"◆"</span>
                <div>
                    <h1>"NinjaOne Patch Toolkit"</h1>
                    <p class="subtitle">{instance}</p>
                </div>
            </div>
            <RunControls/>
            <div class="actions">
                <ExportMenu/>
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
                    <button
                        class="btn"
                        aria-pressed=move || state.ui.show_settings.get().to_string()
                        on:click=move |_| state.ui.show_settings.update(|s| *s = !*s)
                    >
                        "Settings"
                    </button>
                    <Account/>
                </Show>
                <MoreMenu/>
            </div>
            <QueryProgressLine/>
        </header>
    }
}

/// Signed out: the Sign in button. Signed in: one control saying so, whose menu
/// holds Sign out — the status pill and the button beside it said the same thing
/// twice.
#[component]
fn Account() -> impl IntoView {
    let state = expect_context::<AppState>();
    let sign_out = move |_| {
        spawn_local(async move {
            match api::sign_out().await {
                Ok(()) => {
                    state.clear_session();
                    state.session.refresh_auth();
                    state.notify(Toast::ok("Signed out"));
                }
                Err(e) => state.notify(Toast::err(e)),
            }
        });
    };
    view! {
        <Show when=move || state.is_authed() fallback=|| view! { <SignInButton/> }>
            <MenuButton
                label=String::from("● Connected ▾")
                class="btn-ghost account"
                title=String::from("Signed in to NinjaOne")
                align_right=true
            >
                <p class="menu__note">
                    {move || {
                        state
                            .session
                            .auth
                            .get()
                            .map(|a| a.instance_base_url)
                            .unwrap_or_default()
                    }}
                </p>
                <button type="button" class="menu__item" on:click=sign_out>
                    "Sign out"
                </button>
            </MenuButton>
        </Show>
    }
}

/// Starts the OAuth sign-in. Shared by the top bar and the results area's
/// "Sign in to load your fleet" card.
#[component]
pub(crate) fn SignInButton() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <button
            class="btn btn-primary"
            prop:disabled=move || state.session.signing_in.get()
            on:click=move |_| {
                if state.session.signing_in.get_untracked() {
                    return;
                }
                state.session.signing_in.set(true);
                // Neutral wording: the backend reuses a saved sign-in silently when
                // it can, and only then opens a browser.
                state.notify(Toast::ok(
                    "Signing in… if a browser opens, complete the sign-in there",
                ));
                spawn_local(async move {
                    match api::sign_in().await {
                        Ok(()) => {
                            // The backend dropped the previous session's result and
                            // jobs; drop the rendering of them too. The lookups,
                            // scripts and jobs load off the auth status (see `App`).
                            state.clear_session();
                            state.session.refresh_auth();
                            state.notify(Toast::ok("Signed in"));
                        }
                        Err(e) => state.notify(Toast::err(e)),
                    }
                    state.session.signing_in.set(false);
                });
            }
        >
            {move || if state.session.signing_in.get() { "Signing in…" } else { "Sign in" }}
        </button>
    }
}

/// The ⋯ menu: per-machine view conveniences. None of them is used often enough to
/// earn a permanent place in the bar.
#[component]
fn MoreMenu() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <MenuButton
            label=String::from("⋯")
            class="btn-ghost"
            aria_label="More"
            title=String::from("Theme, keyboard shortcuts and view links")
            align_right=true
        >
            <div class="menu__group" role="group" aria-label="Theme">
                <span class="menu__heading">"Theme"</span>
                <div class="menu__segs">
                    {util::Theme::ALL
                        .into_iter()
                        .map(|t| {
                            let on = move || state.ui.theme.get() == t;
                            view! {
                                <button
                                    type="button"
                                    class=move || if on() { "seg seg-on" } else { "seg" }
                                    aria-pressed=move || on().to_string()
                                    on:click=move |_| state.ui.theme.set(t)
                                >
                                    {t.label()}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </div>
            <button
                type="button"
                class="menu__item"
                on:click=move |_| state.ui.shortcuts_open.set(true)
            >
                "Keyboard shortcuts"
                <kbd>"?"</kbd>
            </button>
            <button
                type="button"
                class="menu__item"
                title="Copy a link to these filters, tab, grouping and sort (no selection, no credentials)"
                on:click=move |_| state.copy_view_link()
            >
                "Copy view link"
            </button>
            <button
                type="button"
                class="menu__item"
                on:click=move |_| state.ui.opening_view.set(true)
            >
                "Open view…"
            </button>
        </MenuButton>
    }
}
