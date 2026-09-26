use leptos::task::spawn_local;

use super::*;

/// Renders one changelog line's inline runs — plain text and `**bold**` — into views.
fn render_spans(spans: Vec<MdSpan>) -> impl IntoView {
    spans
        .into_iter()
        .map(|span| match span {
            MdSpan::Text(t) => view! { {t} }.into_any(),
            MdSpan::Strong(t) => view! { <strong>{t}</strong> }.into_any(),
        })
        .collect_view()
}

/// Modal shown when an update is available. Renders the new version + the
/// release notes (changelog) and offers to install + relaunch.
#[component]
pub(crate) fn UpdateSplash() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Escape dismisses the update splash (same as "Later"), unless an install is
    // already running.
    window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape"
            && state.updates.update.get_untracked().is_some()
            && !state.updates.update_busy.get_untracked()
        {
            state.updates.update.set(None);
        }
    });
    view! {
        {move || {
            let Some(info) = state.updates.update.get() else {
                return ().into_any();
            };
            let notes = info.notes.unwrap_or_default();
            let changelog = if notes.trim().is_empty() {
                None
            } else {
                // The notes are the CHANGELOG.md section (markdown); render the
                // headings / bullet lists / bold runs instead of dumping the source.
                let body = parse_changelog(&notes)
                    .into_iter()
                    .map(|block| match block {
                        MdBlock::Heading(h) => view! { <h4>{h}</h4> }.into_any(),
                        MdBlock::List(items) => view! {
                            <ul>
                                {items
                                    .into_iter()
                                    .map(|spans| view! { <li>{render_spans(spans)}</li> })
                                    .collect_view()}
                            </ul>
                        }
                            .into_any(),
                        MdBlock::Paragraph(spans) => {
                            view! { <p>{render_spans(spans)}</p> }.into_any()
                        }
                    })
                    .collect_view();
                Some(
                    view! {
                        <div class="changelog">
                            <h3>"What's new"</h3>
                            <div class="changelog-body">{body}</div>
                        </div>
                    },
                )
            };
            // The install relaunches the app, which would abandon a dispatch mid-batch
            // and every job the poller is still resolving; see
            // `util::update_blocked_reason`.
            let wait_reason = move || {
                util::update_blocked_reason(
                    state.actions.dispatching.get(),
                    state.actions.jobs.with(|j| util::jobs_in_flight(j)),
                )
            };
            let install = move |_| {
                if wait_reason().is_some() {
                    return;
                }
                state.updates.update_busy.set(true);
                spawn_local(async move {
                    // On success the backend installs and relaunches the app, so
                    // this never returns Ok; an Err means the install failed.
                    if let Err(e) = api::install_update().await {
                        state.updates.update_busy.set(false);
                        state.notify(Toast::err(format!("Update failed: {e}")));
                    }
                });
            };
            let dismiss = move |_| state.updates.update.set(None);
            let (dialog, on_tab) = modal::focus_trap();
            view! {
                <div class="modal-overlay">
                    <div
                        class="modal"
                        role="dialog"
                        aria-modal="true"
                        aria-labelledby="update-title"
                        tabindex="-1"
                        node_ref=dialog
                        on:keydown=move |ev| on_tab(&ev)
                    >
                        <h2 id="update-title">
                            {format!("Update available — v{}", info.version)}
                        </h2>
                        <p class="modal-sub">
                            {format!(
                                "You're on v{}. Install the new version now?",
                                info.current_version,
                            )}
                        </p>
                        {changelog}
                        <Show when=move || wait_reason().is_some()>
                            <p class="modal-sub update-wait" role="status">
                                {move || wait_reason().unwrap_or_default()}
                            </p>
                        </Show>
                        <div class="row modal-actions">
                            <button
                                class="btn btn-primary"
                                prop:disabled=move || {
                                    state.updates.update_busy.get() || wait_reason().is_some()
                                }
                                title=move || wait_reason().unwrap_or_default()
                                on:click=install
                            >
                                {move || {
                                    if state.updates.update_busy.get() {
                                        "Updating…"
                                    } else {
                                        "Update & restart"
                                    }
                                }}
                            </button>
                            <button
                                class="btn btn-ghost"
                                prop:disabled=move || state.updates.update_busy.get()
                                on:click=dismiss
                            >
                                "Later"
                            </button>
                        </div>
                    </div>
                </div>
            }
                .into_any()
        }}
    }
}
