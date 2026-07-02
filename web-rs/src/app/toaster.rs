use leptos::prelude::*;

use super::*;

#[component]
pub(crate) fn Toaster() -> impl IntoView {
    let state = expect_context::<AppState>();
    view! {
        // Always-present live region: a screen reader announces the toast as it
        // appears. An aria-live region created at the same moment as its content
        // is not reliably announced, so the wrapper stays mounted.
        <div class="toaster" role="status" aria-live="assertive" aria-atomic="true">
            {move || {
                state.ui.toast
                    .get()
                    .map(|t| {
                        let cls = if t.error { "toast toast-err" } else { "toast toast-ok" };
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
            }}
        </div>
    }
}
