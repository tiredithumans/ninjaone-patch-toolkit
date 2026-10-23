//! Live NinjaOne job status: the "job running" chip on device cells, and the
//! running-jobs table on the Jobs tab and in the device drill-down. The jobs are
//! read by `AppState::tick_live_jobs`; these only render them.

use super::*;

/// "job running" beside a device that NinjaOne reports a patching, script or
/// reboot job running on. Reactive on its own, so a poll repaints the chip and not
/// the row around it.
#[component]
pub(super) fn RunningJobChip(device_id: i64) -> impl IntoView {
    let state = expect_context::<AppState>();
    move || {
        state
            .live_jobs
            .jobs
            .with(|j| {
                j.as_ref()
                    .and_then(|j| util::running_jobs_title(&j.jobs, device_id))
            })
            .map(|title| view! { <span class="chip-note chip-live" title=title>"job running"</span> })
    }
}

/// One row per running job. The Device column is left out where the device is
/// already the subject (the drill-down).
#[component]
fn RunningJobsTable(jobs: Vec<RunningJob>, show_device: bool) -> impl IntoView {
    view! {
        <div class="table-wrap">
            <table>
                <thead>
                    <tr>
                        {show_device.then(|| view! {
                            <th scope="col">"Device"</th>
                            <th scope="col">"Organization"</th>
                        })}
                        <th scope="col">"Job"</th>
                        <th scope="col">"Status"</th>
                        <th scope="col">"Detail"</th>
                        <th scope="col">"Started by"</th>
                        <th scope="col">"Started"</th>
                        <th scope="col">"Updated"</th>
                    </tr>
                </thead>
                <tbody>
                    {jobs
                        .into_iter()
                        .map(|j| {
                            view! {
                                <tr>
                                    {show_device.then(|| view! {
                                        <td><DeviceLink device_id=j.device_id name=j.device_name.clone()/></td>
                                        <td>{j.organization.clone()}</td>
                                    })}
                                    <td>{j.job_type}</td>
                                    <td>{j.status}</td>
                                    <td class="patch-name"><span class="clamp-2">{j.detail.unwrap_or_default()}</span></td>
                                    <td>{j.source_name.unwrap_or_default()}</td>
                                    <td>{j.started_at.unwrap_or_default()}</td>
                                    <td>{j.updated_at.unwrap_or_default()}</td>
                                </tr>
                            }
                        })
                        .collect_view()}
                </tbody>
            </table>
        </div>
    }
}

/// The Jobs tab's "Running in NinjaOne" section: every running patch-ops job on
/// the result's devices, whoever started it — a scheduled patch window as much as
/// a dispatch from this app.
#[component]
pub(super) fn LiveJobsPanel() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        <section class="live-jobs">
            <div class="jobs-toolbar">
                <h3 class="chart-title">"Running in NinjaOne"</h3>
                <span class="live-jobs-asof">
                    {move || {
                        state
                            .live_jobs
                            .jobs
                            .with(|j| j.as_ref().map(|j| format!("as of {}", j.fetched_at)))
                    }}
                </span>
                <Show when=move || !state.session.demo.get()>
                    <button
                        class="btn btn-sm"
                        title="Ask NinjaOne again now. It is also asked every 30 seconds while this window is open."
                        prop:disabled=move || {
                            state.live_jobs.in_flight.get()
                                || state.query.result.with(Option::is_none)
                        }
                        on:click=move |_| state.refresh_live_jobs()
                    >
                        "Refresh"
                    </button>
                </Show>
            </div>
            {move || {
                state
                    .live_jobs
                    .error
                    .get()
                    .map(|e| view! { <p class="scope-note">{format!("Live job status is unavailable: {e}")}</p> })
            }}
            {move || match state.live_jobs.jobs.get() {
                None => view! {
                    <p class="empty">"Run a query to see the patching jobs NinjaOne is running on its devices."</p>
                }
                    .into_any(),
                Some(j) if j.jobs.is_empty() => view! {
                    <p class="empty">"No patching, script or reboot jobs are running on the devices in this result."</p>
                }
                    .into_any(),
                Some(j) => view! { <RunningJobsTable jobs=j.jobs show_device=true/> }.into_any(),
            }}
        </section>
    }
}

/// The drill-down's "Running in NinjaOne" section; nothing when no job is running
/// on the device.
#[component]
pub(super) fn DeviceRunningJobs(device_id: i64) -> impl IntoView {
    let state = expect_context::<AppState>();
    move || {
        let jobs: Vec<RunningJob> = state.live_jobs.jobs.with(|j| {
            j.as_ref()
                .map(|j| {
                    j.jobs
                        .iter()
                        .filter(|x| x.device_id == device_id)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        });
        (!jobs.is_empty()).then(|| {
            view! {
                <section class="live-jobs">
                    <h3 class="chart-title">"Running in NinjaOne"</h3>
                    <RunningJobsTable jobs show_device=false/>
                </section>
            }
        })
    }
}
