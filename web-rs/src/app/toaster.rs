use super::*;

#[component]
pub(crate) fn Toaster() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Only the toast of this region's kind renders into it.
    let toast_of = move |error: bool| {
        move || {
            let slot = if error {
                state.ui.toast_err
            } else {
                state.ui.toast
            };
            slot.get().map(|t| {
                let cls = if t.error {
                    "toast toast-err"
                } else {
                    "toast toast-ok"
                };
                let action = t.action.map(|a| match a {
                    ToastAction::ViewJobs => view! {
                        <button
                            class="link-btn"
                            on:click=move |_| {
                                state.ui.active_tab.set(Tab::Jobs);
                                state.ui.dismiss_toast(error);
                            }
                        >
                            "View jobs"
                        </button>
                    },
                });
                view! {
                    <div
                        class=cls
                        on:mouseenter=move |_| state.ui.hold_toast(error)
                        on:mouseleave=move |_| state.ui.arm_toast(error)
                        on:focusin=move |_| state.ui.hold_toast(error)
                        on:focusout=move |_| state.ui.arm_toast(error)
                    >
                        <span>{t.msg}</span>
                        {action}
                        <button
                            class="x"
                            aria-label="Dismiss notification"
                            on:click=move |_| state.ui.dismiss_toast(error)
                        >
                            "×"
                        </button>
                    </div>
                }
            })
        }
    };
    view! {
        // Two always-present live regions: a screen reader announces a toast as it
        // appears, and a region created at the same moment as its content is not
        // reliably announced, so both stay mounted. An error interrupts (`alert`);
        // everything else waits its turn (`status`, polite). One assertive region
        // for both made every "Preset saved" cut off whatever was being read.
        // The error sits above the notice, so both can be read at once.
        <div class="toasts">
            <div class="toaster" role="alert" aria-atomic="true">
                {toast_of(true)}
            </div>
            <div class="toaster" role="status" aria-live="polite" aria-atomic="true">
                {toast_of(false)}
            </div>
        </div>
    }
}
