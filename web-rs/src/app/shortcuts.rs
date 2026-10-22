//! The global keyboard handler and the shortcuts help dialog. Which key does what
//! is `util::shortcut_for`; this file only reads the event and the DOM.

use leptos::web_sys::{Element, HtmlElement, KeyboardEvent};
use wasm_bindgen::JsCast;

use super::*;

/// Where `/` lands: the Search filter in the patch filter bar.
pub(crate) const SEARCH_INPUT_ID: &str = "filter-search";
/// The device scope bar.
pub(crate) const FILTERS_PANEL_ID: &str = "filters-panel";

#[component]
pub(crate) fn KeyboardShortcuts() -> impl IntoView {
    let state = expect_context::<AppState>();

    window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape" && state.ui.shortcuts_open.get_untracked() {
            state.ui.shortcuts_open.set(false);
            return;
        }
        let cx = util::KeyContext {
            ctrl: ev.ctrl_key(),
            alt: ev.alt_key(),
            meta: ev.meta_key(),
            repeat: ev.repeat(),
            editing: editing(&ev),
            modal_open: modal::any_modal_open(),
        };
        let Some(shortcut) = util::shortcut_for(&ev.key(), cx) else {
            return;
        };
        // Every bound key is ours once matched: `/` would otherwise open the
        // browser's quick-find, and a digit could scroll a focused list.
        ev.prevent_default();
        match shortcut {
            util::Shortcut::RunQuery => state.run_query(),
            util::Shortcut::ShowTab(tab) => state.ui.active_tab.set(tab),
            util::Shortcut::FocusSearch => focus_search(state),
            util::Shortcut::PrevPage | util::Shortcut::NextPage => {
                if state.ui.active_tab.get_untracked() != Tab::Patches
                    || state.query.result.with_untracked(Option::is_none)
                {
                    return;
                }
                let page = state.query.patches_page.get_untracked();
                let count = state.patches_page_count();
                let target = if shortcut == util::Shortcut::PrevPage {
                    util::prev_page(page)
                } else {
                    util::next_page(page, count)
                };
                if target != page {
                    state.go_to_patches_page(target);
                }
            }
            util::Shortcut::ToggleAutoRefresh => state.toggle_refresh_pause(),
            util::Shortcut::ShowHelp => state.ui.shortcuts_open.set(true),
        }
    });

    view! {
        {move || {
            if !state.ui.shortcuts_open.get() {
                return ().into_any();
            }
            let (dialog, on_tab) = modal::focus_trap();
            view! {
                <div class="modal-overlay" role="presentation">
                    <div
                        class="modal shortcuts-dialog"
                        role="dialog"
                        aria-modal="true"
                        aria-labelledby="shortcuts-title"
                        tabindex="-1"
                        node_ref=dialog
                        on:keydown=move |ev| on_tab(&ev)
                    >
                        <h3 id="shortcuts-title">"Keyboard shortcuts"</h3>
                        <p class="modal-sub">
                            "Ignored while typing in a field or while a dialog is open. No key \
                             dispatches, exports or changes a device."
                        </p>
                        <table class="shortcuts-table">
                            <tbody>
                                {util::SHORTCUT_HELP
                                    .iter()
                                    .map(|(keys, what)| {
                                        view! {
                                            <tr>
                                                <th scope="row">
                                                    {keys
                                                        .split_whitespace()
                                                        .map(|k| {
                                                            if k == "–" {
                                                                view! { <span>" – "</span> }.into_any()
                                                            } else {
                                                                view! { <kbd>{k.to_string()}</kbd> }.into_any()
                                                            }
                                                        })
                                                        .collect_view()}
                                                </th>
                                                <td>{*what}</td>
                                            </tr>
                                        }
                                    })
                                    .collect_view()}
                            </tbody>
                        </table>
                        <div class="row modal-actions">
                            <button
                                class="btn btn-primary"
                                on:click=move |_| state.ui.shortcuts_open.set(false)
                            >
                                "Close"
                            </button>
                        </div>
                    </div>
                </div>
            }
                .into_any()
        }}
    }
}

/// Focus is somewhere keys are text; see `util::is_text_entry`. Read from the
/// event target, falling back to the active element.
fn editing(ev: &KeyboardEvent) -> bool {
    let target = ev
        .target()
        .and_then(|t| t.dyn_into::<Element>().ok())
        .or_else(|| document().active_element());
    let Some(el) = target else {
        return false;
    };
    let input_type = el.get_attribute("type");
    let editable = el
        .clone()
        .dyn_into::<HtmlElement>()
        .is_ok_and(|h| h.is_content_editable());
    util::is_text_entry(&el.tag_name(), input_type.as_deref(), editable)
}

/// Focuses the Search filter. It lives in the patch filter bar, which only the
/// Patches and Failures tabs render, so from any other tab `/` switches to Patches
/// first. Deferred a frame: the tab may only just have been told to mount.
fn focus_search(state: AppState) {
    if !matches!(
        state.ui.active_tab.get_untracked(),
        Tab::Patches | Tab::Failures
    ) {
        state.ui.active_tab.set(Tab::Patches);
    }
    request_animation_frame(move || {
        if let Some(el) = document()
            .get_element_by_id(SEARCH_INPUT_ID)
            .and_then(|e| e.dyn_into::<HtmlElement>().ok())
        {
            let _ = el.focus();
        }
    });
}
