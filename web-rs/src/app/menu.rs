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

/// A button that opens a panel of controls below it, the panel **always mounted**
/// and only shown or hidden. For panels holding state a remount would lose or
/// refetch — the action bar's run options (its Run-as menu fetches the sampled
/// device's roles when it mounts) and the reboot mode and reason. Never closes on
/// a press inside it; outside it and Escape close it as for a menu.
#[component]
pub(crate) fn Popover(
    #[prop(into)] label: Signal<String>,
    /// Classes on the trigger, after `btn`.
    #[prop(into)]
    class: Signal<String>,
    #[prop(optional, into)] title: Option<Signal<String>>,
    #[prop(into)] disabled: Signal<bool>,
    /// The caller's handle on the open state, to close it after an action.
    open: RwSignal<bool>,
    children: Children,
) -> impl IntoView {
    let root: NodeRef<Div> = NodeRef::new();
    dismiss_on_outside(open, root);
    // Which way the panel opens, decided as it opens: toward the side of the
    // window with room. The action bar wraps, so the same button sits at the left
    // edge of one window and the right edge of another, and a panel opening the
    // wrong way was clipped by the scrolling tab it lives in.
    let to_left = RwSignal::new(false);
    let place = move || {
        let Some(el) = root.get_untracked() else {
            return;
        };
        let rect = el.get_bounding_client_rect();
        let width = window()
            .inner_width()
            .ok()
            .and_then(|w| w.as_f64())
            .unwrap_or_default();
        to_left.set(rect.left() + rect.width() / 2.0 > width / 2.0);
    };
    view! {
        <div class="menu" node_ref=root>
            <button
                type="button"
                class=move || format!("btn {}", class.get())
                aria-haspopup="true"
                aria-expanded=move || open.get().to_string()
                title=move || title.map(|t| t.get())
                prop:disabled=move || disabled.get()
                on:click=move |_| {
                    if !open.get_untracked() {
                        place();
                    }
                    open.update(|o| *o = !*o)
                }
            >
                {move || label.get()}
            </button>
            <div
                class=move || {
                    if to_left.get() {
                        "menu__panel popover__panel menu__panel--right"
                    } else {
                        "menu__panel popover__panel"
                    }
                }
                prop:hidden=move || !open.get()
            >
                {children()}
            </div>
        </div>
    }
}
