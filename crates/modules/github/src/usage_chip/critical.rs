//! The D5 Critical toast (docs/0082 D5): crossing into Critical raises
//! one `Warning` toast per budget per window through the hub (docs/0058),
//! keyed `usage-<budget>-critical-<reset_epoch>` with source `github`,
//! so the docs/0062 per-source mute covers it. Lifts docs/0050's
//! "budget warnings" non-goal.
//!
//! The hub's dedupe key *refreshes* a visible toast on a repeat raise
//! (0058 D4) rather than swallowing it, so the once-per-window rule is
//! kept here: a budget raises only for a window it hasn't raised for.

use std::collections::HashMap;

use dashboard_core::toasts::{Level, Toast};
use dashboard_core::usage_analytics::surfaces::ThresholdState;

use super::model::{BudgetChip, ChipModel, group_thousands, local_hhmm};
use crate::rate_pause::Lane;

/// The window each budget last raised for.
#[derive(Debug, Default)]
pub(crate) struct CriticalWatch {
    raised: HashMap<Lane, u64>,
}

/// One toast to raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CriticalAlert {
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) message: String,
}

impl CriticalAlert {
    pub(crate) fn toast(&self) -> Toast {
        Toast::new(Level::Warning, self.message.clone())
            .title(self.title.clone())
            .source("github")
            .dedupe_key(self.key.clone())
    }
}

fn slug(lane: Lane) -> &'static str {
    match lane {
        Lane::Rest => "rest",
        Lane::Graphql => "graphql",
    }
}

fn unit(lane: Lane) -> &'static str {
    match lane {
        Lane::Rest => "requests",
        Lane::Graphql => "points",
    }
}

impl CriticalWatch {
    /// The toasts this model owes: each budget at Critical or past it
    /// (a pause, or 0 left) whose window hasn't raised yet. A budget
    /// that drops straight past Critical still raises once; the pause
    /// arming later in the same window raises nothing more.
    pub(crate) fn observe(&mut self, model: &ChipModel) -> Vec<CriticalAlert> {
        let mut out = Vec::new();
        for (lane, chip) in model.budgets() {
            if chip.state() < Some(ThresholdState::Critical) {
                continue;
            }
            let Some(reset) = chip.window_reset() else {
                continue;
            };
            if self.raised.get(&lane) == Some(&reset) {
                continue;
            }
            self.raised.insert(lane, reset);
            out.push(alert(lane, chip, reset));
        }
        out
    }
}

fn alert(lane: Lane, chip: BudgetChip, reset: u64) -> CriticalAlert {
    let name = lane.name();
    let at = local_hhmm(reset);
    let (title, message) = match chip {
        BudgetChip::Live { d, .. } if d.remaining > 0 => (
            format!("GitHub {name} budget critical"),
            format!(
                "{} of {} {} left; the budget resets at {at}.",
                group_thousands(d.remaining),
                group_thousands(d.limit),
                unit(lane)
            ),
        ),
        _ => (
            format!("GitHub {name} budget used up"),
            format!("{name} requests pause until the budget resets at {at}."),
        ),
    };
    CriticalAlert {
        key: format!("usage-{}-critical-{reset}", slug(lane)),
        title,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate_pause::RateLimitPause;
    use crate::usage::{BucketDisplay, UsageDisplay};

    const NOW: u64 = 1_000_000;

    fn bucket(remaining: u32, resets_in_secs: u64) -> Option<BucketDisplay> {
        Some(BucketDisplay {
            remaining,
            limit: 5_000,
            resets_in_secs,
            stale: false,
            snapshot_at: Some(NOW),
        })
    }

    fn model(
        rest: Option<BucketDisplay>,
        graphql: Option<BucketDisplay>,
        pause: RateLimitPause,
        now: u64,
    ) -> ChipModel {
        ChipModel::of(&UsageDisplay::Budget { rest, graphql }, &pause, now)
    }

    fn keys(alerts: &[CriticalAlert]) -> Vec<&str> {
        alerts.iter().map(|a| a.key.as_str()).collect()
    }

    #[test]
    fn crossing_into_critical_raises_once_per_budget_per_window() {
        let mut watch = CriticalWatch::default();
        let reset = NOW + 600;
        // Low raises nothing.
        let low = model(
            bucket(1_000, 600),
            bucket(4_000, 600),
            Default::default(),
            NOW,
        );
        assert!(watch.observe(&low).is_empty());
        // Critical raises once, keyed by the window's reset epoch.
        let crit = model(
            bucket(400, 600),
            bucket(4_000, 600),
            Default::default(),
            NOW,
        );
        let alerts = watch.observe(&crit);
        assert_eq!(keys(&alerts), vec![format!("usage-rest-critical-{reset}")]);
        assert_eq!(alerts[0].title, "GitHub REST budget critical");
        assert!(alerts[0].message.starts_with("400 of 5,000 requests left"));
        // The next second, and further spend in the same window: nothing.
        let later = model(
            bucket(380, 599),
            bucket(4_000, 599),
            Default::default(),
            NOW + 1,
        );
        assert!(watch.observe(&later).is_empty());
        // The pause arming in the same window: still nothing.
        let paused = model(
            bucket(0, 590),
            bucket(4_000, 590),
            RateLimitPause {
                rest: Some(reset),
                graphql: None,
            },
            NOW + 10,
        );
        assert!(watch.observe(&paused).is_empty());
        // GraphQL is its own budget: it raises on its own crossing.
        let both = model(
            bucket(380, 590),
            bucket(300, 590),
            Default::default(),
            NOW + 10,
        );
        assert_eq!(
            keys(&watch.observe(&both)),
            vec![format!("usage-graphql-critical-{reset}")]
        );
    }

    #[test]
    fn a_new_window_raises_again() {
        let mut watch = CriticalWatch::default();
        let first = model(bucket(400, 600), None, Default::default(), NOW);
        assert_eq!(watch.observe(&first).len(), 1);
        // Next window: refilled, then critical again.
        let refilled = model(bucket(5_000, 3_600), None, Default::default(), NOW + 600);
        assert!(watch.observe(&refilled).is_empty());
        let again = model(bucket(450, 3_000), None, Default::default(), NOW + 1_200);
        assert_eq!(
            keys(&watch.observe(&again)),
            vec![format!("usage-rest-critical-{}", NOW + 4_200)]
        );
    }

    #[test]
    fn a_budget_that_skips_critical_raises_used_up() {
        let mut watch = CriticalWatch::default();
        let until = NOW + 900;
        let paused = model(
            bucket(4_000, 900),
            None,
            RateLimitPause {
                rest: None,
                graphql: Some(until),
            },
            NOW,
        );
        let alerts = watch.observe(&paused);
        assert_eq!(
            keys(&alerts),
            vec![format!("usage-graphql-critical-{until}")]
        );
        assert_eq!(alerts[0].title, "GitHub GraphQL budget used up");
        let toast = alerts[0].toast();
        assert_eq!(toast.level, Level::Warning);
        assert_eq!(toast.source.as_deref(), Some("github"));
        assert_eq!(
            toast.dedupe_key.as_deref(),
            Some(format!("usage-graphql-critical-{until}").as_str())
        );
    }

    #[test]
    fn missing_and_stale_budgets_raise_nothing() {
        let mut watch = CriticalWatch::default();
        let stale = Some(BucketDisplay {
            stale: true,
            ..bucket(10, 0).unwrap()
        });
        let m = model(stale, None, Default::default(), NOW);
        assert!(watch.observe(&m).is_empty());
    }
}
