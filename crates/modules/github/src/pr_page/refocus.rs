//! #871: which triggers skip the reopen TTL. The TTL (docs/0072 D9)
//! decides only whether a *reopen* revalidates in the background; a
//! trigger that already passed its own age floor, or that asks for a
//! read on purpose, is never skipped by it (docs/0084 D1).

use crate::usage::Trigger;

/// `true` when a detail read for this trigger runs whatever the age of
/// the landed document. `Refocus` passes the 15 s floor in
/// `refresh_if_stale` and `TabEntry` the 30 s one (docs/0084 D2) before
/// they dispatch; a notification that names the page is a deliberate read, and a refetch
/// a probe asked for (`Changed`) is the answer to a change GitHub reported.
pub(super) fn bypasses_reopen_ttl(trigger: Trigger) -> bool {
    matches!(
        trigger,
        Trigger::Manual
            | Trigger::MutationRefetch
            | Trigger::Recheck
            | Trigger::Refocus
            | Trigger::TabEntry
            | Trigger::NotificationLanding
            | Trigger::Changed
    )
}

#[cfg(test)]
mod tests;
