//! Device actions: what may be dispatched, what the guardrails allow, and how a
//! dispatched job is tracked to a terminal state.
//!
//! The API layer (`api::actions`) knows how to *send* an action. This module owns
//! everything around that: the [`plan`] function that decides whether an action is
//! allowed to go out at all, the [`JobReport`] rows the UI watches, the parameter
//! string handed to a library script, and the audit trail.
//!
//! [`plan`] is deliberately pure — the clock is injected — so every guardrail is
//! unit-testable without a tenant, a network, or a wall clock.

/// How long a dispatched job may stay unresolved before the poller gives up.
pub const JOB_TIMEOUT_MINUTES: i64 = 45;
/// Most recent jobs kept in memory. Terminal rows are evicted first.
pub const MAX_JOBS: usize = 500;

pub mod activity;
pub mod audit;
pub mod job;
pub mod kind;
pub mod parameters;
pub mod planning;

pub use activity::*;
pub use job::*;
pub use kind::*;
pub use parameters::*;
pub use planning::*;

#[cfg(test)]
mod tests;
