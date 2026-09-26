//! The Trend tab: fleet rollups over `run-history.jsonl`.

use super::*;

/// Fleet trend over the run history — the only view in this app with a time axis.
///
/// Every other surface renders *now*: each query destructively replaces the cached
/// result, so "is the backlog shrinking" and "did last night's window work" had no
/// answer. This reads `run-history.jsonl`, one rollup line per completed query.
#[component]
pub(super) fn TrendTab() -> impl IntoView {
    let state = expect_context::<AppState>();
    let history = RwSignal::new(Vec::<RunRecord>::new());
    let loaded = RwSignal::new(false);

    let load = move || {
        if !api::is_tauri() {
            loaded.set(true);
            return;
        }
        spawn_local(async move {
            match api::read_run_history().await {
                Ok(rows) => {
                    history.set(rows);
                    loaded.set(true);
                }
                Err(e) => {
                    loaded.set(true);
                    state.notify(Toast::err(e));
                }
            }
        });
    };
    load();

    // Only runs that measured the same thing as the newest one; see `trend_series`.
    let series = move || util::trend_series(&history.get(), 60);

    view! {
        <div class="trend">
            <div class="jobs-toolbar">
                <button class="btn btn-sm" on:click=move |_| load()>
                    "Reload"
                </button>
            </div>
            {move || {
                if !loaded.get() {
                    return view! { <p class="empty">"Reading run history…"</p> }.into_any();
                }
                let runs = series();
                if runs.len() < 2 {
                    return view! {
                        <p class="empty">
                            "Not enough history yet. Every completed query appends one line; "
                            "run a couple more and the trend appears here."
                        </p>
                    }
                        .into_any();
                }
                let newest = runs[runs.len() - 1].clone();
                let oldest = runs[0].clone();
                view! {
                    <p class="chips-label">
                        {format!(
                            "{} runs · {} → {} · {}{}",
                            runs.len(),
                            oldest.at.clone(),
                            newest.at.clone(),
                            newest.patch_families_label(),
                            if newest.scoped { ", filtered scope" } else { ", whole fleet" },
                        )}
                    </p>
                    <TrendLine
                        title="Compliance"
                        percent=true
                        higher_is_better=true
                        values={runs.iter().filter_map(|r| r.compliance_pct()).collect::<Vec<_>>()}
                    />
                    <TrendLine
                        title="Pending patches"
                        percent=false
                        higher_is_better=false
                        values={runs.iter().map(|r| r.rows_total as f64).collect::<Vec<_>>()}
                    />
                    <TrendLine
                        title="Aged criticals"
                        percent=false
                        higher_is_better=false
                        values={runs.iter().map(|r| r.aged_critical as f64).collect::<Vec<_>>()}
                    />
                    <TrendLine
                        title="Devices needing reboot"
                        percent=false
                        higher_is_better=false
                        values={runs.iter().map(|r| r.needs_reboot as f64).collect::<Vec<_>>()}
                    />
                }
                    .into_any()
            }}
        </div>
    }
}
