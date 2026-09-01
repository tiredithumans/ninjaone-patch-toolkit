//! Focus management for the two modal dialogs (the action confirmation and the
//! update splash).
//!
//! `role="dialog" aria-modal="true"` is a *claim*: it tells assistive tech the rest
//! of the page is inert, but it moves nothing. Without this, keyboard focus stayed
//! on the button that opened the dialog — under the overlay — so Tab walked the
//! covered page (row checkboxes, filters) and Space on the still-focused opener
//! called `open_plan` again, replacing the pending plan while its dialog was on
//! screen. A dialog therefore (1) takes focus when it mounts, (2) keeps Tab inside
//! itself, and (3) hands focus back to its opener when it unmounts.
//!
//! JS-backed, so nothing here is host-testable; the logic is kept to the minimum
//! that cannot live anywhere else.

use leptos::html::Div;
use leptos::prelude::*;
use leptos::web_sys::{Element, HtmlElement, KeyboardEvent, Node};
use wasm_bindgen::JsCast;

/// What Tab can land on inside a dialog. `tabindex="-1"` is excluded so the dialog
/// container itself (which carries it to be focusable on open) is not a stop.
const FOCUSABLE: &str = "button:not([disabled]), input:not([disabled]), \
                         select:not([disabled]), textarea:not([disabled]), \
                         a[href], [tabindex]:not([tabindex='-1'])";

/// Wires a dialog's focus lifecycle. Call in the body of the component that renders
/// the dialog element; attach the returned `NodeRef` to that element (which needs
/// `tabindex="-1"`) and route its `keydown` through the returned handler.
///
/// The opener is whatever had focus when this ran — i.e. when the dialog was
/// created, which is the click that opened it.
pub(crate) fn focus_trap() -> (NodeRef<Div>, impl Fn(&KeyboardEvent) + Clone + 'static) {
    let dialog: NodeRef<Div> = NodeRef::new();
    let opener = document()
        .active_element()
        .and_then(|e| e.dyn_into::<HtmlElement>().ok());

    // Runs once the element is mounted (the ref is a signal, so the effect re-runs
    // when it is filled in).
    Effect::new(move |_| {
        if let Some(el) = dialog.get() {
            let _ = el.focus();
        }
    });
    on_cleanup(move || {
        if let Some(opener) = opener {
            let _ = opener.focus();
        }
    });

    let on_tab = move |ev: &KeyboardEvent| {
        if ev.key() != "Tab" {
            return;
        }
        let Some(el) = dialog.get_untracked() else {
            return;
        };
        let Ok(stops) = el.query_selector_all(FOCUSABLE) else {
            return;
        };
        let n = stops.length();
        if n == 0 {
            ev.prevent_default();
            return;
        }
        let stop = |i: u32| stops.get(i).and_then(|n| n.dyn_into::<HtmlElement>().ok());
        let (Some(first), Some(last)) = (stop(0), stop(n - 1)) else {
            return;
        };
        let active: Option<Element> = document().active_element();
        let at = |stop: &HtmlElement| {
            let stop: &Element = stop.as_ref();
            active.as_ref().is_some_and(|a| a == stop)
        };
        // Wrap at either end; a Tab that lands inside the dialog is left alone.
        // A focus that is nowhere inside (or on the container itself, right after
        // open) is sent to the first stop so the first Tab is never a trip outside.
        let container: &Element = el.as_ref();
        let inside = active.as_ref().is_some_and(|a| {
            let node: &Node = a.as_ref();
            container.contains(Some(node)) && a != container
        });
        if ev.shift_key() {
            if at(&first) || !inside {
                ev.prevent_default();
                let _ = last.focus();
            }
        } else if at(&last) || !inside {
            ev.prevent_default();
            let _ = first.focus();
        }
    };
    (dialog, on_tab)
}
