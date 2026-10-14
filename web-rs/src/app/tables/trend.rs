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
                let older = util::older_method_note(util::older_method_runs(&history.get()))
                    .map(|n| view! { <p class="chips-label">{n}</p> });
                if runs.len() < 2 {
                    return view! {
                        {older}
                        <p class="empty">
                            "Not enough history yet. Every completed query appends one line; "
                            "run a couple more and the trend appears here."
                        </p>
                    }
                        .into_any();
                }
                let newest = runs[runs.len() - 1].clone();
                let previous = runs[runs.len() - 2].clone();
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
                    {older}
                    // Every run here shares the newest one's method, so a method-1
                    // figure is present on each; `filter_map` only drops an empty scope.
                    <TrendLine
                        title="Within SLA"
                        percent=true
                        higher_is_better=true
                        values={runs.iter().filter_map(|r| r.within_sla_pct()).collect::<Vec<_>>()}
                    />
                    <TrendLine
                        title="Fully patched"
                        percent=true
                        higher_is_better=true
                        values={runs.iter().filter_map(|r| r.fully_patched_pct()).collect::<Vec<_>>()}
                    />
                    <TrendLine
                        title="Pending patches"
                        percent=false
                        higher_is_better=false
                        values={runs.iter().filter_map(|r| r.pending_total.map(|n| n as f64)).collect::<Vec<_>>()}
                    />
                    <TrendLine
                        title="Urgent past SLA"
                        percent=false
                        higher_is_better=false
                        values={runs.iter().map(|r| r.urgent_past_sla as f64).collect::<Vec<_>>()}
                    />
                    <TrendLine
                        title="Devices needing reboot"
                        percent=false
                        higher_is_better=false
                        values={runs.iter().map(|r| r.needs_reboot as f64).collect::<Vec<_>>()}
                    />
                    <OrgDeltaTable newest=newest previous=previous/>
                }
                    .into_any()
            }}
        </div>
    }
}

/// Per-organization movement against the previous comparable run, the biggest
/// regression first. The fleet cards above can hold still while one organization
/// slides and another improves; this is where that shows.
#[component]
fn OrgDeltaTable(newest: RunRecord, previous: RunRecord) -> impl IntoView {
    let note = util::org_delta_note(&newest, &previous);
    let deltas = util::org_deltas(&newest, &previous);
    let pct = |o: &Option<OrgRun>| {
        o.as_ref()
            .and_then(OrgRun::within_sla_pct)
            .map(util::format_pct)
            .unwrap_or_else(|| "\u{2014}".to_string())
    };
    let count = |o: &Option<OrgRun>, get: fn(&OrgRun) -> usize| {
        o.as_ref()
            .map(|o| group_thousands(get(o)))
            .unwrap_or_else(|| "\u{2014}".to_string())
    };
    let table = (!deltas.is_empty()).then(|| {
        view! {
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            // Spelled as the Compliance tab spells them
                            // (`rows::ComplianceBucket::COLUMNS`).
                            <th scope="col">"Organization"</th>
                            <th scope="col">"Within SLA %"</th>
                            <th scope="col">"Change"</th>
                            <th scope="col">"Pending Patches"</th>
                            <th scope="col">"Change"</th>
                            <th scope="col">"Urgent Pending"</th>
                            <th scope="col">"Change"</th>
                            <th scope="col">"Urgent Past SLA"</th>
                            <th scope="col">"Change"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {deltas
                            .into_iter()
                            .map(|d| {
                                let (now, before) = (d.now.is_some(), d.previous.is_some());
                                let cell = move |delta, percent, higher_is_better| {
                                    let (class, label) = util::org_delta_cell(
                                        delta,
                                        percent,
                                        higher_is_better,
                                        now,
                                        before,
                                    );
                                    view! { <td class=class>{label}</td> }
                                };
                                view! {
                                    <tr>
                                        <td>{d.organization.clone()}</td>
                                        <td>{pct(&d.now)}</td>
                                        {cell(d.within_sla_delta(), true, true)}
                                        <td>{count(&d.now, |o| o.pending)}</td>
                                        {cell(d.pending_delta(), false, false)}
                                        <td>{count(&d.now, |o| o.urgent_pending)}</td>
                                        {cell(d.urgent_pending_delta(), false, false)}
                                        <td>{count(&d.now, |o| o.urgent_past_sla)}</td>
                                        {cell(d.aged_delta(), false, false)}
                                    </tr>
                                }
                            })
                            .collect_view()}
                    </tbody>
                </table>
            </div>
        }
    });
    view! {
        <h3 class="trend-subhead">
            {format!("By organization \u{2014} since {}", previous.at)}
        </h3>
        {note.map(|n| view! { <p class="chips-label">{n}</p> })}
        {table}
    }
}
