use leptos::prelude::*;

use super::*;

#[component]
pub(crate) fn Results() -> impl IntoView {
    let state = expect_context::<AppState>();
    let tab = state.active_tab;

    let summary = move || {
        state.result.with(|r| {
            r.as_ref().map(|r| {
                format!(
                    "{} patch rows · {} devices · generated {}",
                    r.rows_total, r.devices_total, r.generated_at
                )
            })
        })
    };

    view! {
        <section class="panel results">
            <div class="tabs">
                <button
                    class=move || tab_class(tab.get(), Tab::Patches)
                    on:click=move |_| tab.set(Tab::Patches)
                >
                    "Patches"
                </button>
                <button
                    class=move || tab_class(tab.get(), Tab::Compliance)
                    on:click=move |_| tab.set(Tab::Compliance)
                >
                    "Compliance"
                </button>
                <button
                    class=move || tab_class(tab.get(), Tab::Reboot)
                    on:click=move |_| tab.set(Tab::Reboot)
                >
                    "Needs Reboot"
                </button>
                <button
                    class=move || tab_class(tab.get(), Tab::Failures)
                    on:click=move |_| tab.set(Tab::Failures)
                >
                    "Failures"
                </button>
                <span class="result-summary">{summary}</span>
            </div>
            {move || match tab.get() {
                Tab::Patches => view! { <PatchesTable/> }.into_any(),
                Tab::Compliance => view! { <ComplianceTab/> }.into_any(),
                Tab::Reboot => view! { <RebootTable/> }.into_any(),
                Tab::Failures => view! { <FailuresTable/> }.into_any(),
            }}
        </section>
    }
}

#[component]
fn PatchesTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    // The total row count comes from the summary; the visible page lives in
    // `page_rows`, fetched from the backend cache rather than held in full here.
    let total = move || {
        state
            .result
            .with(|r| r.as_ref().map_or(0, |r| r.rows_total))
    };
    let page_count = move || total().div_ceil(PATCHES_PAGE_SIZE).max(1);
    // Clamp the stored page so a shorter result (e.g. after an auto-refresh) can't
    // leave us past the last page.
    let page = move || state.patches_page.get().min(page_count() - 1);
    let rows = move || state.page_rows.get();
    let pager_summary = move || {
        let t = total();
        let start = page() * PATCHES_PAGE_SIZE;
        let end = (start + PATCHES_PAGE_SIZE).min(t);
        format!(
            "Rows {}\u{2013}{} of {} \u{00b7} Page {} of {}",
            start + 1,
            end,
            group_thousands(t),
            page() + 1,
            page_count(),
        )
    };
    // Page navigation updates the index and fetches that page's rows on demand.
    let go_to = move |target: usize| {
        state.patches_page.set(target);
        state.fetch_page(target);
    };
    let go_prev = move |_| go_to(page().saturating_sub(1));
    let go_next = move |_| {
        let last = page_count().saturating_sub(1);
        go_to((page().min(last) + 1).min(last));
    };

    view! {
        <Show
            when=move || state.result.with(|r| r.is_some())
            fallback=|| view! { <p class="empty">"Run a query to list patches."</p> }
        >
            <p class="scope-note">"Every patch matching your filters (device scope + patch filters)."</p>
            <Show
                when=move || { total() > 0 }
                fallback=|| {
                    view! {
                        <p class="empty">
                            "No patches matched your filters. Try widening the organization, severity, or status selection."
                        </p>
                    }
                }
            >
                <Show when=move || { page_count() > 1 }>
                    <div class="pager">
                        <button
                            class="btn"
                            prop:disabled=move || page() == 0
                            on:click=go_prev
                        >
                            "‹ Prev"
                        </button>
                        <span class="pager-info">{pager_summary}</span>
                        <button
                            class="btn"
                            prop:disabled=move || { page() + 1 >= page_count() }
                            on:click=go_next
                        >
                            "Next ›"
                        </button>
                    </div>
                </Show>
                <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Organization"</th>
                            <th scope="col">"Location"</th>
                            <th scope="col">"Role"</th>
                            <th scope="col">"Device"</th>
                            <th scope="col">"OS"</th>
                            <th scope="col">"Type"</th>
                            <th scope="col">"KB"</th>
                            <th scope="col">"Patch"</th>
                            <th scope="col">"Severity"</th>
                            <th scope="col">"Status"</th>
                            <th scope="col">"Release"</th>
                            <th scope="col">"Installed"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            rows()
                                .into_iter()
                                .map(|r| {
                                    let sev = sev_class(&r.severity);
                                    let stat = status_class(&r.status);
                                    view! {
                                        <tr>
                                            <td>{r.organization}</td>
                                            <td>{r.location.unwrap_or_default()}</td>
                                            <td>{r.device_role.unwrap_or_default()}</td>
                                            <td>{r.device_name}</td>
                                            <td>{r.os_name.unwrap_or_default()}</td>
                                            <td>{r.patch_type}</td>
                                            <td>{r.kb.unwrap_or_default()}</td>
                                            <td class="patch-name">{r.name}</td>
                                            <td>
                                                <span class=sev>{r.severity}</span>
                                            </td>
                                            <td>
                                                <span class=stat>{r.status}</span>
                                            </td>
                                            <td>{r.release_date.unwrap_or_default()}</td>
                                            <td>{r.installed_date.unwrap_or_default()}</td>
                                        </tr>
                                    }
                                })
                                .collect_view()
                        }}
                    </tbody>
                </table>
                </div>
            </Show>
        </Show>
    }
}

#[component]
fn ComplianceTab() -> impl IntoView {
    let state = expect_context::<AppState>();
    let has_result = move || state.result.with(|r| r.is_some());
    view! {
        <Show
            when=has_result
            fallback=|| view! { <p class="empty">"Run a query to see compliance."</p> }
        >
            <p class="scope-note">
                "Fleet compliance for the selected device scope (organization / location / role / OS type). "
                "Reflects the whole pending backlog — not narrowed by status, severity, KB search, or the date window."
            </p>
            <ComplianceCharts/>
            <ComplianceTable/>
        </Show>
    }
}

#[component]
fn ComplianceTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    let buckets = move || {
        state
            .result
            .with(|r| r.as_ref().map(|r| r.compliance.clone()).unwrap_or_default())
    };
    let has_buckets = move || {
        state
            .result
            .with(|r| r.as_ref().is_some_and(|r| !r.compliance.is_empty()))
    };

    view! {
        <Show
            when=has_buckets
            fallback=|| view! { <p class="empty">"No compliance data yet."</p> }
        >
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Organization"</th>
                            <th scope="col">"Devices"</th>
                            <th scope="col">"Compliant"</th>
                            <th scope="col">"Compliance"</th>
                            <th scope="col">"Pending Critical/Important Patches"</th>
                            <th scope="col">"Aged (past SLA)"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            buckets()
                                .into_iter()
                                .map(|b| {
                                    let pct = format!("{:.0}%", b.compliance_pct);
                                    let aged = b.aged_critical;
                                    let aged_class = if aged > 0 { "sev-critical" } else { "" };
                                    // Prefix a warning glyph so the aged backlog is
                                    // distinguishable without relying on color.
                                    let aged_label = if aged > 0 {
                                        format!("⚠ {aged}")
                                    } else {
                                        aged.to_string()
                                    };
                                    let aged_title = if aged > 0 { "Past SLA — needs attention" } else { "" };
                                    view! {
                                        <tr>
                                            <td>{b.organization}</td>
                                            <td>{b.devices_total}</td>
                                            <td>{b.devices_compliant}</td>
                                            <td>{pct}</td>
                                            <td>{b.pending_critical}</td>
                                            <td>
                                                <span class=aged_class title=aged_title>
                                                    {aged_label}
                                                </span>
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

#[component]
fn FailuresTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Backend ships the failure rollup whole (one entry per failing patch) in the
    // summary, already sorted by affected-device count — render it as-is.
    let failures = move || {
        state
            .result
            .with(|r| r.as_ref().map(|r| r.failures.clone()).unwrap_or_default())
    };
    let has_failures = move || {
        state
            .result
            .with(|r| r.as_ref().is_some_and(|r| !r.failures.is_empty()))
    };

    view! {
        <Show
            when=has_failures
            fallback=|| {
                view! {
                    <p class="empty">
                        "No patch failures. Select the FAILED status and Run query to analyze failures."
                    </p>
                }
            }
        >
            <p class="scope-note">"Failed installs matching your filters (device scope + patch filters)."</p>
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Severity"</th>
                            <th scope="col">"KB"</th>
                            <th scope="col">"Patch"</th>
                            <th scope="col">"Affected devices"</th>
                            <th scope="col">"Devices"</th>
                            <th scope="col">"Latest failure"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            failures()
                                .into_iter()
                                .map(|f| {
                                    let sev = sev_class(&f.severity);
                                    view! {
                                        <tr>
                                            <td>
                                                <span class=sev>{f.severity}</span>
                                            </td>
                                            <td>{f.kb.unwrap_or_default()}</td>
                                            <td class="patch-name">{f.name}</td>
                                            <td>{f.affected_devices}</td>
                                            <td class="device-list">{f.device_names.join(", ")}</td>
                                            <td>{f.latest_failure.unwrap_or_default()}</td>
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

#[component]
fn RebootTable() -> impl IntoView {
    let state = expect_context::<AppState>();
    // The backend already trimmed the device list to the needs-reboot subset, so
    // clone that directly; the emptiness check clones nothing.
    let devices = move || {
        state.result.with(|r| {
            r.as_ref()
                .map(|r| r.reboot_devices.clone())
                .unwrap_or_default()
        })
    };
    let has_devices = move || {
        state
            .result
            .with(|r| r.as_ref().is_some_and(|r| !r.reboot_devices.is_empty()))
    };

    view! {
        <Show
            when=has_devices
            fallback=|| view! { <p class="empty">"No devices flagged for reboot."</p> }
        >
            <p class="scope-note">
                "Devices in the selected device scope flagged for reboot — not narrowed by status, severity, or search."
            </p>
            <div class="table-wrap">
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Organization"</th>
                            <th scope="col">"Location"</th>
                            <th scope="col">"Role"</th>
                            <th scope="col">"Device"</th>
                            <th scope="col">"OS"</th>
                            <th scope="col">"Pending patches"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            devices()
                                .into_iter()
                                .map(|d| {
                                    view! {
                                        <tr>
                                            <td>{d.organization}</td>
                                            <td>{d.location.unwrap_or_default()}</td>
                                            <td>{d.device_role.unwrap_or_default()}</td>
                                            <td>{d.device_name}</td>
                                            <td>{d.os_name.unwrap_or_default()}</td>
                                            <td>{d.pending_count}</td>
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
