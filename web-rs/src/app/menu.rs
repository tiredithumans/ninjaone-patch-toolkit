//! Drop-down menus and popovers: the top bar's Export / account / ⋯ menus and the
//! scope pickers.
//!
//! A menu is a disclosure, not a dialog: it carries no `aria-modal`, takes no focus
//! trap and leaves the page usable behind it. `modal::any_modal_open` reads
//! `aria-modal` to hold auto-refresh and silence the keyboard shortcuts, and an open
//! Export menu is no reason for either.
//!
//! JS-backed, so nothing here is host-testable; the logic is the minimum that
//! cannot live anywhere else.

use leptos::html::Div;
use leptos::prelude::*;
use leptos::web_sys::{Element, HtmlElement, Node};
use wasm_bindgen::JsCast;

/// Closes `open` on a pointer press outside `root` and on Escape, returning focus
/// to the first button inside `root` (the trigger) after an Escape.
///
/// A press, not a click: a click lands after the press has already moved focus,
/// and a drag that starts inside a picker's list and ends outside it is not a
/// dismissal. Listening on the window rather than on focus loss because macOS's
/// WebKit view does not focus a button that is clicked, so the trigger never
/// "loses" a focus it never had.
pub(crate) fn dismiss_on_outside(open: RwSignal<bool>, root: NodeRef<Div>) {
    let inside = move |target: Option<leptos::web_sys::EventTarget>| {
        let (Some(root), Some(target)) = (root.get_untracked(), target) else {
            return false;
        };
        let root: &Element = root.as_ref();
        target
            .dyn_ref::<Node>()
            .is_some_and(|node| root.contains(Some(node)))
    };
    let press = window_event_listener(leptos::ev::pointerdown, move |ev| {
        if open.get_untracked() && !inside(ev.target()) {
            open.set(false);
        }
    });
    let key = window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() != "Escape" || !open.get_untracked() {
            return;
        }
        open.set(false);
        if let Some(trigger) = root
            .get_untracked()
            .and_then(|r| r.query_selector("button").ok().flatten())
            .and_then(|b| b.dyn_into::<HtmlElement>().ok())
        {
            let _ = trigger.focus();
        }
    });
    on_cleanup(move || {
        press.remove();
        key.remove();
    });
}

/// A button that opens a panel of actions below it.
///
/// Choosing an item closes the menu: a press on any `button` inside the panel
/// does, unless the button carries `data-keep-open` (a two-click delete, say).
/// Inputs inside the panel keep it open.
#[component]
pub(crate) fn MenuButton(
    /// The trigger's text, e.g. `"Export ▾"`.
    #[prop(into)]
    label: Signal<String>,
    /// Extra classes on the trigger, after `btn`.
    #[prop(optional)]
    class: &'static str,
    /// The trigger's accessible name when its text alone says too little (`⋯`).
    #[prop(optional)]
    aria_label: Option<&'static str>,
    #[prop(optional, into)] title: Option<Signal<String>>,
    /// Anchor the panel to the trigger's right edge, for menus at the end of a row.
    #[prop(optional)]
    align_right: bool,
    children: ChildrenFn,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let root: NodeRef<Div> = NodeRef::new();
    dismiss_on_outside(open, root);
    let children = StoredValue::new(children);
    let close_on_item = move |ev: leptos::ev::MouseEvent| {
        let item = ev
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|el| el.closest("button").ok().flatten());
        if item.is_some_and(|b| !b.has_attribute("data-keep-open")) {
            open.set(false);
        }
    };
    view! {
        <div class="menu" node_ref=root>
            <button
                type="button"
                class=format!("btn {class}")
                aria-haspopup="true"
                aria-expanded=move || open.get().to_string()
                aria-label=aria_label
                title=move || title.map(|t| t.get())
                on:click=move |_| open.update(|o| *o = !*o)
            >
                {move || label.get()}
            </button>
            <Show when=move || open.get()>
                <div
                    class=if align_right { "menu__panel menu__panel--right" } else { "menu__panel" }
                    on:click=close_on_item
                >
                    {children.with_value(|c| c())}
                </div>
            </Show>
        </div>
    }
}
