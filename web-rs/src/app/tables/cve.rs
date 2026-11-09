//! The CVE search: a dialog asking the backend which devices in the current result
//! may still be exposed to one CVE (`find_cve`). Read-only; device names are plain
//! text rather than drill-down links, so one dialog never opens over another.

use super::*;

#[derive(Clone)]
enum CveLoad {
    Idle,
    Loading,
    Ready(Box<CveExposure>),
    Failed(String),
}

#[component]
pub(super) fn CveSearch() -> impl IntoView {
    let state = expect_context::<AppState>();
    let open = RwSignal::new(false);
    let input = RwSignal::new(String::new());
    let load = RwSignal::new(CveLoad::Idle);
    // Only the newest search's reply is applied, and none from an ended session.
    let seq = RwSignal::new(0u64);
    let escape = window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape" && open.get_untracked() {
            open.set(false);
        }
    });
    // A `WindowListenerHandle` removes nothing when dropped. Left in place, the
    // listener outlived the Patches tab and every later Escape read the disposed
    // `open` and panicked — once per earlier visit to the tab.
    on_cleanup(move || escape.remove());
    let search = move || {
        if state.session.demo.get_untracked() {
            load.set(CveLoad::Failed(
                "The CVE search needs the desktop app: the browser demo has no exploit data."
                    .into(),
            ));
            return;
        }
        let mine = util::next_query_seq(seq.get_untracked());
        seq.set(mine);
        let epoch = state.session.epoch();
        let cve = input.get_untracked();
        load.set(CveLoad::Loading);
        spawn_local(async move {
            let outcome = api::find_cve(cve).await;
            // The tab may have closed during the read, which disposes `seq`.
            let Some(current) = seq.try_get_untracked() else {
                return;
            };
            if util::is_superseded(current, mine) || state.session.outlived(epoch) {
                return;
            }
            load.set(match outcome {
                Ok(e) => CveLoad::Ready(Box::new(e)),
                Err(e) => CveLoad::Failed(e),
            });
        });
    };
    view! {
        <button
            class="btn btn-ghost"
            title="Which devices in these results may still be exposed to a CVE"
            on:click=move |_| open.set(true)
        >
            "Find CVE"
        </button>
        {move || {
            open.get()
                .then(|| {
                    let (dialog, on_tab) = modal::focus_trap();
                    view! {
                        <div class="modal-overlay">
                            <div
                                class="modal modal-wide cve-search"
                                role="dialog"
                                aria-modal="true"
                                aria-labelledby="cve-search-title"
                                tabindex="-1"
                                node_ref=dialog
                                on:keydown=move |ev| on_tab(&ev)
                            >
                                <div class="device-drill-head">
                                    <h2 id="cve-search-title">"Find a CVE"</h2>
                                    <button
                                        class="x"
                                        aria-label="Close the CVE search"
                                        on:click=move |_| open.set(false)
                                    >
                                        "×"
                                    </button>
                                </div>
                                <form
                                    class="row cve-search__form"
                                    on:submit=move |ev| {
                                        ev.prevent_default();
                                        search();
                                    }
                                >
                                    <input
                                        type="search"
                                        aria-label="CVE id"
                                        placeholder="CVE-2026-21002"
                                        prop:value=move || input.get()
                                        on:input=move |ev| input.set(event_target_value(&ev))
                                    />
                                    <button
                                        class="btn btn-primary"
                                        type="submit"
                                        prop:disabled=move || matches!(load.get(), CveLoad::Loading)
                                    >
                                        "Search"
                                    </button>
                                </form>
                                {move || match load.get() {
                                    CveLoad::Idle => {
                                        view! {
                                            <p class="scope-note">
                                                "Checks the devices in the current results against Microsoft's and CISA's data."
                                            </p>
                                        }
                                            .into_any()
                                    }
                                    CveLoad::Loading => view! { <p class="empty">"Searching…"</p> }.into_any(),
                                    CveLoad::Failed(e) => {
                                        view! { <p class="modal-error" role="alert">{e}</p> }.into_any()
                                    }
                                    CveLoad::Ready(e) => view! { <CveAnswer exposure=*e/> }.into_any(),
                                }}
                            </div>
                        </div>
                    }
                })
        }}
    }
}

#[component]
fn CveAnswer(exposure: CveExposure) -> impl IntoView {
    let newer = util::newer_updates(&exposure);
    let cap = util::exposure_cap_note(exposure.devices.len(), exposure.devices_total);
    let kev = exposure.kev.as_ref().map(util::kev_line);
    let fixed = (!exposure.fixed_by.is_empty()).then(|| exposure.fixed_by.join(", "));
    view! {
        <h3 class="chart-title">
            {exposure.cve.clone()}
            {exposure
                .exploited
                .then(|| {
                    view! {
                        <span class="chip-note chip-exploited" title=util::EXPLOITED_HELP>
                            "exploited"
                        </span>
                    }
                })}
        </h3>
        {exposure.title.map(|t| view! { <p>{t}</p> })}
        {kev.map(|k| view! { <p class="scope-note">{k}</p> })}
        {fixed.map(|f| view! { <p class="scope-note">"Fixed by " {f}</p> })}
        {(!newer.is_empty())
            .then(|| {
                view! { <p class="scope-note">"Also included in " {newer.join(", ")}</p> }
            })}
        <p class="scope-note">{exposure.sentence}</p>
        {cap.map(|c| view! { <p class="scope-note">{c}</p> })}
        {(!exposure.devices.is_empty())
            .then(|| {
                view! {
                    <div class="table-wrap">
                        <table>
                            <thead>
                                <tr>
                                    <th scope="col">"Organization"</th>
                                    <th scope="col">"Device"</th>
                                    <th scope="col">"Missing"</th>
                                    <th scope="col">"Status"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {exposure
                                    .devices
                                    .into_iter()
                                    .map(|d| {
                                        let stat = status_class(&d.status);
                                        view! {
                                            <tr>
                                                <td>{d.organization}</td>
                                                // The id is what NinjaOne's own search takes.
                                                <td title=format!("NinjaOne device id {}", d.device_id)>
                                                    {d.device_name}
                                                    {d.offline.then(|| view! { <span class="chip-note">"offline"</span> })}
                                                </td>
                                                <td>
                                                    {d.kb}
                                                    {(!d.direct)
                                                        .then(|| {
                                                            view! {
                                                                <span
                                                                    class="chip-note"
                                                                    title="A later update that includes the fix; the device may already have it."
                                                                >
                                                                    "includes the fix"
                                                                </span>
                                                            }
                                                        })}
                                                </td>
                                                <td>
                                                    <span class=stat>{d.status}</span>
                                                </td>
                                            </tr>
                                        }
                                    })
                                    .collect_view()}
                            </tbody>
                        </table>
                    </div>
                }
            })}
    }
}
