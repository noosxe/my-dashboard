//! Rate-limit data on fixture responses (docs/0081 §12.1, docs/0084 §8).
//!
//! Two sources, one effective budget per bucket:
//! - a **variant** (`rate/<name>.json`, `DASHBOARD_GITHUB_FIXTURE_RATE`):
//!   a static snapshot of both buckets, set before launch;
//! - a **runtime override** (`fixture budget <remaining> [--budget
//!   rest|graphql]`): sets one bucket's `remaining` while the app runs,
//!   so a receipt can walk the docs/0082 thresholds (Low ≤ 25 %,
//!   Critical ≤ 10 %, Exhausted 0). Over a variant it keeps that
//!   bucket's limit and reset; with no variant it starts from a full
//!   5000-point window that resets in an hour.
//!
//! With neither, no response carries rate data (the chip stays
//! `REST — · GraphQL —`), as before. Numbers never decrement on their
//! own: request order isn't deterministic, a static budget is. The
//! `RateLimit` stamped on a response is what the transport turns into
//! `x-ratelimit-limit|remaining|reset` (and `retry-after` when the
//! variant's bucket carries none, which fixtures never do).

use std::path::PathBuf;
use std::sync::Mutex;

use crate::http::RateLimit;

/// The rate-limit variant variable (`healthy` | `low` | `critical` |
/// `exhausted` | `graphql-exhausted`, or any `<dir>/rate/<name>.json`).
pub(crate) const RATE_ENV: &str = "DASHBOARD_GITHUB_FIXTURE_RATE";

/// One bucket of a rate variant file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct RateBucket {
    pub(crate) limit: u32,
    pub(crate) remaining: u32,
    /// Seconds from the budget clock's "now" to the window reset.
    pub(crate) reset_in_secs: u64,
}

impl RateBucket {
    pub(crate) fn at(self, now: u64) -> RateLimit {
        RateLimit {
            limit: self.limit,
            remaining: self.remaining,
            reset_epoch: now + self.reset_in_secs,
            retry_after: None,
        }
    }

    /// A full window, for a runtime override with no variant under it.
    const FULL: RateBucket = RateBucket {
        limit: 5000,
        remaining: 5000,
        reset_in_secs: 3600,
    };
}

/// A rate variant (`rate/<name>.json`): the REST `core` bucket, the
/// GraphQL bucket, and the per-query GraphQL `cost` the bodies report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct RateVariant {
    pub(crate) rest: RateBucket,
    pub(crate) graphql: RateBucket,
    #[serde(default = "one")]
    pub(crate) graphql_cost: u32,
}

fn one() -> u32 {
    1
}

/// Load the variant `RATE_ENV` names from `<dir>/rate/`. Unset → `None`
/// (no rate data, today's captures); a named variant that doesn't
/// load is logged loudly and also yields `None`.
pub(crate) fn rate_variant(dir: &std::path::Path) -> Option<RateVariant> {
    let name = std::env::var(RATE_ENV).ok().filter(|v| !v.is_empty())?;
    let rel = PathBuf::from("rate").join(format!("{name}.json"));
    let parsed = std::fs::read_to_string(dir.join(&rel))
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str::<RateVariant>(&t).map_err(|e| e.to_string()));
    match parsed {
        Ok(v) => {
            log::info!("github fixture: rate variant {name}");
            Some(v)
        }
        Err(e) => {
            log::warn!(
                "github fixture: rate variant {} unusable ({e})",
                rel.display()
            );
            None
        }
    }
}

/// The two budgets GitHub keeps per token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lane {
    Rest,
    Graphql,
}

impl Lane {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "rest" => Some(Self::Rest),
            "graphql" => Some(Self::Graphql),
            _ => None,
        }
    }
}

/// The effective budgets: variant plus runtime overrides.
#[derive(Debug, Default)]
pub(crate) struct Budgets {
    variant: Option<RateVariant>,
    /// `remaining` set by `fixture budget`, per lane.
    rest: Mutex<Option<u32>>,
    graphql: Mutex<Option<u32>>,
}

impl Budgets {
    pub(crate) fn new(variant: Option<RateVariant>) -> Self {
        Self {
            variant,
            ..Self::default()
        }
    }

    fn slot(&self, lane: Lane) -> &Mutex<Option<u32>> {
        match lane {
            Lane::Rest => &self.rest,
            Lane::Graphql => &self.graphql,
        }
    }

    /// `fixture budget`: pin one lane's `remaining` (clamped to its limit).
    pub(crate) fn set(&self, lane: Lane, remaining: u32) {
        *self.slot(lane).lock().unwrap_or_else(|e| e.into_inner()) = Some(remaining);
    }

    /// The lane's bucket: override over variant over nothing.
    pub(crate) fn bucket(&self, lane: Lane) -> Option<RateBucket> {
        let over = *self.slot(lane).lock().unwrap_or_else(|e| e.into_inner());
        let base = match (self.variant, lane) {
            (Some(v), Lane::Rest) => Some(v.rest),
            (Some(v), Lane::Graphql) => Some(v.graphql),
            (None, _) => over.map(|_| RateBucket::FULL),
        }?;
        Some(RateBucket {
            remaining: over.map_or(base.remaining, |r| r.min(base.limit)),
            ..base
        })
    }

    /// The `cost` GraphQL bodies report per query.
    pub(crate) fn graphql_cost(&self) -> u32 {
        self.variant.map_or(1, |v| v.graphql_cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashboard_core::usage_analytics::surfaces::ThresholdState;

    const VARIANT: RateVariant = RateVariant {
        rest: RateBucket {
            limit: 5000,
            remaining: 4000,
            reset_in_secs: 100,
        },
        graphql: RateBucket {
            limit: 5000,
            remaining: 3000,
            reset_in_secs: 200,
        },
        graphql_cost: 3,
    };

    #[test]
    fn nothing_set_means_no_rate_data() {
        let b = Budgets::new(None);
        assert_eq!(
            (b.bucket(Lane::Rest), b.bucket(Lane::Graphql)),
            (None, None)
        );
    }

    #[test]
    fn an_override_without_a_variant_starts_from_a_full_window() {
        let b = Budgets::new(None);
        b.set(Lane::Rest, 400);
        let rest = b.bucket(Lane::Rest).unwrap();
        assert_eq!(
            (rest.limit, rest.remaining, rest.reset_in_secs),
            (5000, 400, 3600)
        );
        // The other lane stays dark: setting REST must not invent GraphQL data.
        assert_eq!(b.bucket(Lane::Graphql), None);
        assert_eq!(b.graphql_cost(), 1);
    }

    #[test]
    fn an_override_keeps_the_variants_limit_and_reset_and_one_lane_only() {
        let b = Budgets::new(Some(VARIANT));
        b.set(Lane::Graphql, 120);
        assert_eq!(b.bucket(Lane::Rest), Some(VARIANT.rest));
        let gql = b.bucket(Lane::Graphql).unwrap();
        assert_eq!((gql.remaining, gql.reset_in_secs), (120, 200));
        assert_eq!(b.graphql_cost(), 3);
        // Above the limit clamps to it.
        b.set(Lane::Graphql, 9999);
        assert_eq!(b.bucket(Lane::Graphql).unwrap().remaining, 5000);
    }

    /// The numbers a receipt sets land on the docs/0082 ladder.
    #[test]
    fn thresholds_are_reachable_per_lane() {
        let b = Budgets::new(None);
        for (remaining, want) in [
            (5000, ThresholdState::Normal),
            (1250, ThresholdState::Low),
            (500, ThresholdState::Critical),
            (0, ThresholdState::Exhausted),
        ] {
            for lane in [Lane::Rest, Lane::Graphql] {
                b.set(lane, remaining);
                let bucket = b.bucket(lane).unwrap();
                assert_eq!(
                    ThresholdState::of(bucket.remaining, Some(bucket.limit)),
                    Some(want),
                    "{lane:?} at {remaining}"
                );
            }
        }
    }

    #[test]
    fn lanes_parse() {
        assert_eq!(Lane::parse("rest"), Some(Lane::Rest));
        assert_eq!(Lane::parse("graphql"), Some(Lane::Graphql));
        assert_eq!(Lane::parse("core"), None);
    }
}
