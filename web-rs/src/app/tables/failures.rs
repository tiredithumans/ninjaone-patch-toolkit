//! The Failures tab: one row per failing patch, each drilling down to its rows.

use super::*;

#[component]
pub(super) fn FailuresTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Backend ships the failure rollup whole (one entry per failing patch) in the
    // summary, already sorted by affected-device count — render it as-is.
    let failures = move || {
        state
            .query
            .result
            .with(|r| r.as_ref().map(|r| r.failures.clone()).unwrap_or_default())
    };
    let has_failures = move || {
        state
            .query
            .result
            .with(|r| r.as_ref().is_some_and(|r| !r.failures.is_empty()))
    };

    view! {
        <PatchFilterBar/>
        <Show
            when=has_failures
            fallback=move || {
                // A query that never asked for failed installs cannot have found
                // any: say so, and offer the one click that asks.
                let looked = state.query.applied_filters.with(|a| {
                    a.as_ref().is_some_and(|a| a.statuses.iter().any(|s| s == "FAILED"))
                });
                if looked {
                    view! { <p class="empty">"No failed installs in this scope and window."</p> }
                        .into_any()
                } else {
                    view! {
                        <div class="empty">
                            <p>"These results don't include failed installs."</p>
                            <button
                                class="btn btn-primary"
                                on:click=move |_| state.include_status_and_run("FAILED")
                            >
                                "Include failed installs and run"
                            </button>
                        </div>
                    }
                        .into_any()
                }
            }
        >
            <div class="table-wrap table-wrap--fill">
                <table>
                    <thead>
                        <tr>
                            // Seven columns spelled as `rows::FailureGroup::COLUMNS`
                            // spells them. `Patch Type` was missing here while the
                            // workbook and the HTML report both carried it, so the
                            // on-screen table could not tell an OS failure from a
                            // third-party one — and third-party rows show "(no KB)",
                            // which is exactly when the distinction matters.
                            <th scope="col">"Severity"</th>
                            <th scope="col">"Patch Type"</th>
                            <th scope="col">"KB"</th>
                            <th scope="col">"Patch"</th>
                            <th scope="col">"Affected Devices"</th>
                            <th scope="col">"Latest Failure"</th>
                            <th scope="col">"Devices"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            failures()
                                .into_iter()
                                .map(|f| {
                                    let sev = sev_class(&f.severity);
                                    // Third-party patches carry no `kbNumber`, so fall
                                    // back to the name — `search_allowed` matches the
                                    // needle against KB *and* name, so one control
                                    // covers both.
                                    let needle = f
                                        .kb
                                        .clone()
                                        .filter(|k| !k.is_empty())
                                        .unwrap_or_else(|| f.name.clone());
                                    let title = format!("Show the rows for {needle}");
                                    let kb_label = f.kb.clone().unwrap_or_default();
                                    view! {
                                        <tr>
                                            <td>
                                                <span class=sev>{f.severity}</span>
                                            </td>
                                            <td>{f.patch_type}</td>
                                            <td>
                                                <button
                                                    class="drill"
                                                    title=title
                                                    on:click=move |_| {
                                                        state.drill_to_patch(needle.clone())
                                                    }
                                                >
                                                    {if kb_label.is_empty() {
                                                        "(no KB)".to_string()
                                                    } else {
                                                        kb_label.clone()
                                                    }}
                                                </button>
                                            </td>
                                            <td class="patch-name"><span class="clamp-2" title=f.name.clone()>{f.name.clone()}</span></td>
                                            <td>{f.affected_devices}</td>
                                            <td>{f.latest_failure.unwrap_or_default()}</td>
                                            <td class="device-list">
                                                {util::failure_device_list(&f.device_names, f.affected_devices)}
                                            </td>
                                        </tr>
                                    }
                                })
                                .collect_view()
                        }}
                    </tbody>
                </table>
            </div>
        </Show>
    }
}
