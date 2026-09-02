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
mod filters;
mod format;
mod pager;
mod query;
mod selection;
mod sort;

pub(crate) use changelog::*;
pub(crate) use filters::*;
pub(crate) use format::*;
pub(crate) use pager::*;
pub(crate) use query::*;
pub(crate) use selection::*;
pub(crate) use sort::*;

#[cfg(test)]
mod tests;
