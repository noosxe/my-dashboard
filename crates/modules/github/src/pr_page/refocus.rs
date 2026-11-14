//! #871: which triggers skip the reopen TTL. The TTL (docs/0072 D9)
//! decides only whether a *reopen* revalidates in the background; a
//! trigger that already passed its own age floor, or that asks for a
//! read on purpose, is never skipped by it (docs/0084 D1).

use crate::usage::Trigger;

/// `true` when a detail read for this trigger runs whatever the age of
/// the landed document. `Refocus` passes the 15 s floor in
/// `refresh_if_stale` before it dispatches.
pub(super) fn bypasses_reopen_ttl(trigger: Trigger) -> bool {
    matches!(
        trigger,
        Trigger::Manual | Trigger::MutationRefetch | Trigger::Recheck | Trigger::Refocus
    )
}

#[cfg(test)]
mod tests;
