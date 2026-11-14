//! #875: the gated-runs sweep spends REST requests, so it obeys the REST
//! pause, and it is booked under the trigger of the read that armed it.
//!
//! docs/0084 D4: a landing no longer sweeps by default. Every probe-driven
//! refetch would otherwise cost a REST run list on top of its detail.

use crate::usage::Trigger;

/// Whether a landed detail arms the gated-runs sweep: on open (the
/// `ViewShown` read), on Refresh (`Manual`), when the head moved, and when
/// the failed-check count rose (a run stopping for approval shows as
/// `action_required`). `failed_before` is `None` for the first landing the
/// page sees.
pub(super) fn runs_after_landing(
    trigger: Trigger,
    head_changed: bool,
    failed_before: Option<u32>,
    failed_now: u32,
) -> bool {
    matches!(trigger, Trigger::ViewShown | Trigger::Manual)
        || head_changed
        || failed_before.is_some_and(|before| failed_now > before)
}

#[cfg(test)]
mod tests;
