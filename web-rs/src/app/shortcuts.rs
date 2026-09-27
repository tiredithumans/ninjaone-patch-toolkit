//! The global keyboard handler and the shortcuts help dialog. Which key does what
//! is `util::shortcut_for`; this file only reads the event and the DOM.

use leptos::web_sys::{Element, HtmlElement, KeyboardEvent};
use wasm_bindgen::JsCast;

use super::*;

/// Where `/` lands: the Search filter, or — on a Fleet-health tab, where the
/// patch filters are folded away — the first control in the Filters panel.
pub(crate) const SEARCH_INPUT_ID: &str = "filter-search";
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

/// Opens the Filters panel and focuses the Search field (or the panel's first
/// control when Search is folded away). Deferred a frame: the panel may only
/// just have been told to expand.
fn focus_search(state: AppState) {
    state.ui.filters_collapsed.set(false);
    request_animation_frame(move || {
        let doc = document();
        let target = doc
            .get_element_by_id(SEARCH_INPUT_ID)
            .or_else(|| {
                doc.get_element_by_id(FILTERS_PANEL_ID).and_then(|panel| {
                    panel
                        .query_selector("input, select, button:not(.filters-toggle)")
                        .ok()
                        .flatten()
                })
            })
            .and_then(|e| e.dyn_into::<HtmlElement>().ok());
        if let Some(el) = target {
            let _ = el.focus();
        }
    });
}
