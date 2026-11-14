//! docs/0084: how the PR page stays accurate without Refresh.
//!
//! - D2, event-driven refreshes (`events`, `write_lanes`, `notices`): each
//!   fires on something the user or GitHub did (a tab opened, the module
//!   shown again, a write confirmed, a list returned to, a notification
//!   that names this PR), and goes through the gates the other reads
//!   obey: age floor, backoff, per-budget pause and dedup.
//! - D3/D4, the change probe (`probe`, `probe_diff`, `lane_map`, `timer`):
//!   while the page is watched, a timer sends conditional REST requests
//!   and refetches only the lanes whose probe changed.
//! - D5-D7, how often and how much (`cadence`, `budget`, `queue`): the
//!   interval as one pure function, the page's rolling-hour ceiling, and
//!   the merge queue's detail refetch.

use std::time::Duration;

pub(crate) mod budget;
pub(crate) mod cadence;
pub(super) mod events;
pub(crate) mod lane_map;
pub(crate) mod notices;
pub(crate) mod probe;
pub(crate) mod probe_diff;
pub(crate) mod queue;
pub(crate) mod timer;
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
