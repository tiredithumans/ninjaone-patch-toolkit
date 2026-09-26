use super::*;

#[component]
pub(crate) fn Toaster() -> impl IntoView {
    let state = expect_context::<AppState>();
    // Only the toast of this region's kind renders into it.
    let toast_of = move |error: bool| {
        move || {
            state.ui.toast.get().filter(|t| t.error == error).map(|t| {
                let cls = if t.error {
                    "toast toast-err"
                } else {
                    "toast toast-ok"
                };
                view! {
                    <div class=cls>
                        <span>{t.msg}</span>
                        <button
                            class="x"
                            aria-label="Dismiss notification"
                            on:click=move |_| state.ui.toast.set(None)
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
        <div class="toaster" role="status" aria-live="polite" aria-atomic="true">
            {toast_of(false)}
        </div>
        <div class="toaster" role="alert" aria-atomic="true">
            {toast_of(true)}
        </div>
    }
}
