//! What a results tab shows while there is no result to show: the idle hint, or
//! what the running query has loaded so far.

use super::*;

/// A tab with no result yet. Idle, it says what to do (`idle`). While a query runs
/// it shows that query instead: these panels used to go on saying "Run a query…"
/// under one that was already running, for the minute or more a first whole-fleet
/// query takes, with the only sign of life a counter in the top bar.
#[component]
pub(super) fn NoResult(idle: &'static str) -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <Show
            when=move || state.run.busy.get()
            fallback=move || view! { <p class="empty">{idle}</p> }
        >
            <QueryLoading/>
        </Show>
    }
}

/// The running query, feed by feed: the records in so far and, where NinjaOne
/// announced the feed's size, how much of it that is (`util::progress_lines`).
/// Not a live region — the counts tick twice a second, and the top bar's status
/// already announces that a query is running.
#[component]
fn QueryLoading() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <div class="query-loading">
            <p class="query-loading__title">
                {move || {
                    let secs = state.run.elapsed_secs();
                    if state.run.progress.get().joining {
                        format!("Computing the results… {secs:.0}s")
                    } else {
                        format!("Loading from NinjaOne… {secs:.0}s")
                    }
                }}
            </p>
            <ul class="query-loading__feeds">
                {move || {
                    util::progress_lines(&state.run.progress.get())
                        .into_iter()
                        .map(|line| {
                            view! {
                                <li>
                                    <span>{line.label}</span>
                                    <span class="query-loading__count">{line.count}</span>
                                    {line
                                        .fraction
                                        .map(|done| {
                                            view! {
                                                <span class="query-loading__meter" aria-hidden="true">
                                                    <span style=format!("width:{:.1}%", done * 100.0)></span>
                                                </span>
                                            }
                                        })}
                                </li>
                            }
                        })
                        .collect_view()
                }}
            </ul>
            <p class="query-loading__note">
                "The first query of a session reads the whole fleet. The ones after it reuse what is already loaded."
            </p>
            <button class="btn btn-ghost" on:click=move |_| state.cancel_query()>
                "Cancel"
            </button>
        </div>
    }
}
