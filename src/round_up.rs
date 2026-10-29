//! Round-up: rules that send desktop items into Corrals (docs/design/round-up-spec.md).
//!
//! **Hard invariant (spec 1):** Round-up changes only which Corral an item belongs to. Nothing in
//! this module tree can move, copy, rename, delete or write a file: the evaluator and planner are
//! pure functions of facts, a clock and a time zone, and a plan is applied through `Layout`'s
//! membership functions only.
//!
//! * [`model`]: typed rules, conditions, the stored `RoundUp` block, built-in rules (spec 2, 6).
//! * [`eval`]: three-valued evaluation (spec 2, 3).
//! * [`plan`]: first-match planning, applying, run timing, the live caption (spec 3, 4).
//! * [`migrate`]: conversion of the old stored rules (spec 6).
//! * [`preview`]: the "Sort the desktop now" preview text (spec 4).

mod eval;
mod migrate;
mod model;
mod plan;
mod preview;

#[cfg(test)]
mod tests;

pub use crate::round_up_time::Clock;
pub use migrate::StoredRule;
pub use model::*;
pub use plan::*;
pub use preview::*;
