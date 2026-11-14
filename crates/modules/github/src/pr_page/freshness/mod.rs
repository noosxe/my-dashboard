//! docs/0084 D2: the event-driven refreshes of the PR page. Each fires
//! on something the user or GitHub did (a tab opened, the module shown
//! again, a write confirmed, a list returned to, a notification that
//! names this PR), never on a timer, and each goes through the gates the
//! other reads obey: age floor, backoff, per-budget pause and dedup.

use std::time::Duration;

pub(super) mod events;
pub(crate) mod notices;
pub(super) mod write_lanes;

/// Entering a tab refetches its lane when the lane is older than this.
pub(crate) const TAB_ENTRY_FLOOR: Duration = Duration::from_secs(30);

/// The module shown again with the PR page open behaves like a refocus
/// (the same 15 s floor, spelled out because the two are separate
/// triggers).
pub(crate) const RESHOWN_FLOOR: Duration = crate::service::FOCUS_MIN_AGE;

/// Closing the PR page refreshes the list it returns to when the list
/// is older than this.
pub(crate) const LIST_RETURN_FLOOR: Duration = Duration::from_secs(60);
