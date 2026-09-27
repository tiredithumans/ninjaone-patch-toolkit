//! Small, standalone view helpers shared across the app components: option/date
//! parsing, number formatting, and CSS-class pickers. They touch no `AppState`, so
//! they live here rather than bloating `app.rs`. Every helper here is JS-free and
//! unit-tests on the host target — the date pair used to reach for `js_sys::Date`
//! and so could not be tested at all; it is now plain arithmetic.
//!
//! Split by concern; every submodule is re-exported here so callers keep
//! writing `util::name`. Anything worth asserting from a component or from
//! `state.rs` lands in one of these files, never inline in a `#[component]`.

mod changelog;
mod columns;
mod filters;
mod format;
mod jobs;
mod pager;
mod query;
mod refresh;
mod selection;
mod shortcuts;
mod sort;
mod theme;
mod view_link;

pub(crate) use changelog::*;
pub(crate) use columns::*;
pub(crate) use filters::*;
pub(crate) use format::*;
pub(crate) use jobs::*;
pub(crate) use pager::*;
pub(crate) use query::*;
pub(crate) use refresh::*;
pub(crate) use selection::*;
pub(crate) use shortcuts::*;
pub(crate) use sort::*;
pub(crate) use theme::*;
pub(crate) use view_link::*;

#[cfg(test)]
mod tests;
