//! What the views derive off the UI thread (docs/0082 D11 decision): a
//! read lands with the D7 default budget and the active view's data
//! already computed on the background executor, and a control change
//! re-derives there too, behind the band skeletons. Everything here is
//! pure: rows and a key in, owned data out.
//!
//! The Budget view's four bands depend on different controls (the cards
//! always show both budgets; only band 2 reads `Group by`), so each band
//! is reused from the previous derivation when its own inputs are
//! unchanged ([`Band::stale`]): picking a budget re-derives bands 2–4,
//! a `Group by` change band 2 alone. The Requests view's bands follow
//! the same pattern in [`requests`].

use std::sync::Arc;

mod requests;

pub(super) use requests::{RequestBand, RequestQuery, RequestsData, span as request_span};

use super::UsageView;
use super::range::{self, BUDGET_MODULE};
use crate::usage_analytics::surfaces::{
    self, Avoidable, AvoidableQuery, Budget, BudgetCards, BudgetCardsQuery, BudgetScope,
    HistoryQuery, Preset, SpendDimension, WhereItWent, WhereQuery, WindowHistory,
};
use crate::usage_records::UsageRecord;

/// D7's band query: `Group by` plus an optional `Then by`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpendGroup {
    pub group_by: SpendDimension,
    pub then_by: Option<SpendDimension>,
}

impl Default for SpendGroup {
    fn default() -> Self {
        SpendGroup {
            group_by: SpendDimension::Surface,
            then_by: None,
        }
    }
}

/// Everything a view's derivation depends on besides the rows. A
/// control change that leaves the key unchanged re-derives nothing.
/// Not `Copy`: the Module facet carries a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ViewKey {
    pub as_of_ms: u64,
    pub view: UsageView,
    /// The preset the view reads ([`range::effective_preset`]).
    pub preset: Preset,
    pub budget: Budget,
    /// D7's grouping; the default on the Requests view, which never
    /// reads it, so a Budget-only control derives nothing there.
    pub spend: SpendGroup,
    /// D10's query bar and the log's sort; the default on the Budget
    /// view, for the same reason.
    pub requests: RequestQuery,
}

impl ViewKey {
    pub(super) fn new(
        as_of_ms: u64,
        view: UsageView,
        preset: Preset,
        budget: Budget,
        spend: SpendGroup,
        requests: RequestQuery,
    ) -> Self {
        ViewKey {
            as_of_ms,
            view,
            preset: range::effective_preset(view, preset),
            budget,
            spend: match view {
                UsageView::Budget => spend,
                UsageView::Requests => SpendGroup::default(),
            },
            requests: match view {
                UsageView::Budget => RequestQuery::default(),
                UsageView::Requests => requests,
            },
        }
    }
}

/// The Budget view's bands (D4–D9), in page order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Band {
    Cards,
    Where,
    History,
    Avoidable,
}

impl Band {
    /// Whether this band's data, derived for `shown`, is out of date for
    /// `target`. The cards show both budgets, so the budget never
    /// re-derives them; only band 2 reads the grouping.
    pub(super) fn stale(self, shown: &ViewKey, target: &ViewKey) -> bool {
        let window = shown.as_of_ms != target.as_of_ms || shown.preset != target.preset;
        match self {
            Band::Cards => window,
            Band::History | Band::Avoidable => window || shown.budget != target.budget,
            Band::Where => window || shown.budget != target.budget || shown.spend != target.spend,
        }
    }
}

/// The Budget view's derived bands, each shared so an unchanged band is
/// carried into the next derivation without a copy.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct BudgetData {
    /// The key the bands were derived for (a Budget-view key).
    pub key: ViewKey,
    pub cards: Arc<BudgetCards>,
    pub spend: Arc<WhereItWent>,
    pub history: Arc<WindowHistory>,
    pub avoidable: Arc<Avoidable>,
}

impl BudgetData {
    /// Whether `band` is ready to draw for `target`; a stale band shows
    /// its skeleton until the derivation in flight lands (D18).
    pub(super) fn ready(&self, band: Band, target: &ViewKey) -> bool {
        target.view == UsageView::Budget && !band.stale(&self.key, target)
    }
}

/// The active view's derived data.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct ViewData {
    /// The Budget bands, last derived for a Budget-view key. Kept while
    /// the Requests view is active, so switching back reuses them.
    pub budget: Option<BudgetData>,
    /// The Requests bands, likewise kept while Budget is active.
    pub requests: Option<RequestsData>,
}

/// The active view's band procedures over the frozen rows (D15a),
/// reusing every band of `prev` whose inputs `key` leaves unchanged.
pub(super) fn derive_views(
    rows: &[UsageRecord],
    key: ViewKey,
    prev: Option<&ViewData>,
) -> ViewData {
    let prev_budget = prev.and_then(|p| p.budget.as_ref());
    let prev_requests = prev.and_then(|p| p.requests.as_ref());
    match key.view {
        UsageView::Budget => ViewData {
            budget: Some(derive_budget(rows, key, prev_budget)),
            requests: prev_requests.cloned(),
        },
        UsageView::Requests => ViewData {
            requests: Some(requests::derive_requests(rows, &key, prev_requests)),
            budget: prev_budget.cloned(),
        },
    }
}

fn derive_budget(rows: &[UsageRecord], key: ViewKey, prev: Option<&BudgetData>) -> BudgetData {
    let keep = |band: Band| prev.filter(|p| !band.stale(&p.key, &key));
    let scope = BudgetScope {
        module: BUDGET_MODULE.into(),
        budget: key.budget,
        as_of_ms: key.as_of_ms,
        preset: key.preset,
    };
    BudgetData {
        key: key.clone(),
        cards: keep(Band::Cards).map_or_else(
            || Arc::new(cards(rows, key.as_of_ms, key.preset)),
            |p| p.cards.clone(),
        ),
        spend: keep(Band::Where).map_or_else(
            || {
                Arc::new(surfaces::where_it_went(
                    rows,
                    &WhereQuery {
                        scope: scope.clone(),
                        group_by: key.spend.group_by,
                        then_by: key.spend.then_by,
                    },
                ))
            },
            |p| p.spend.clone(),
        ),
        history: keep(Band::History).map_or_else(
            || {
                Arc::new(surfaces::window_history(
                    rows,
                    &HistoryQuery {
                        scope: scope.clone(),
                        tz_offset_ms: local_offset_ms(key.as_of_ms),
                    },
                ))
            },
            |p| p.history.clone(),
        ),
        avoidable: keep(Band::Avoidable).map_or_else(
            || {
                Arc::new(surfaces::avoidable(
                    rows,
                    &AvoidableQuery {
                        scope: scope.clone(),
                    },
                ))
            },
            |p| p.avoidable.clone(),
        ),
    }
}

/// Band 1 (D4) for `preset`: both budgets, whatever is selected.
fn cards(rows: &[UsageRecord], as_of_ms: u64, preset: Preset) -> BudgetCards {
    surfaces::budget_cards(
        rows,
        &BudgetCardsQuery {
            module: BUDGET_MODULE.into(),
            as_of_ms,
            preset,
            // The per-budget pause reaches the page with #699.
            rest_paused_until_ms: None,
            graphql_paused_until_ms: None,
        },
    )
}

/// The local UTC offset at `as_of`, for D8's per-day columns: the
/// calculator reads no clock, so the page passes it (D15a).
pub(super) fn local_offset_ms(as_of_ms: u64) -> i64 {
    use chrono::{Offset, TimeZone};
    chrono::Local
        .timestamp_millis_opt(as_of_ms as i64)
        .single()
        .map_or(0, |t| i64::from(t.offset().fix().local_minus_utc()) * 1_000)
}

/// The D7 default (Q6): the budget with the smaller remaining fraction.
pub(super) fn default_budget(rows: &[UsageRecord], as_of_ms: u64) -> Budget {
    cards(rows, as_of_ms, Preset::CurrentWindow).default_budget
}

/// A finished read, derived in the read's background task.
pub(super) struct Landed {
    pub rows: Vec<UsageRecord>,
    pub degraded: bool,
    pub budget: Budget,
    pub key: ViewKey,
    pub views: ViewData,
}

/// The controls a read derives for, as they stood when it started.
#[derive(Debug, Clone)]
pub(super) struct Controls {
    pub view: UsageView,
    pub preset: Preset,
    /// The user's Budget pick, if they made one; otherwise the read
    /// applies the D7 default (Q6).
    pub picked_budget: Option<Budget>,
    pub spend: SpendGroup,
    pub requests: RequestQuery,
}

/// The read's background half after the store returns: the budget,
/// then the views for the controls as they stood at read time.
pub(super) fn land(
    rows: Vec<UsageRecord>,
    degraded: bool,
    as_of_ms: u64,
    controls: Controls,
) -> Landed {
    let budget = controls
        .picked_budget
        .unwrap_or_else(|| default_budget(&rows, as_of_ms));
    let key = ViewKey::new(
        as_of_ms,
        controls.view,
        controls.preset,
        budget,
        controls.spend,
        controls.requests,
    );
    let views = derive_views(&rows, key.clone(), None);
    Landed {
        rows,
        degraded,
        budget,
        key,
        views,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_analytics::surfaces::Span;
    use crate::usage_records::Transport;

    const AS_OF: u64 = 10_000_000_000;
    const MIN: u64 = 60_000;

    fn snap(transport: Transport, remaining: u32) -> UsageRecord {
        let mut r = UsageRecord::new("github", "home", "home", "view-shown");
        r.ts_epoch_ms = AS_OF - MIN;
        r.transport = transport;
        r.status = 200;
        r.cost = Some(1);
        r.remaining_after = Some(remaining);
        r.limit_after = Some(5_000);
        r.cycle_reset_epoch = Some((AS_OF + 30 * MIN) / 1_000);
        r
    }

    fn controls(view: UsageView, preset: Preset) -> Controls {
        Controls {
            view,
            preset,
            picked_budget: None,
            spend: SpendGroup::default(),
            requests: RequestQuery::default(),
        }
    }

    fn rows() -> Vec<UsageRecord> {
        vec![
            snap(Transport::Graphql, 1_000),
            snap(Transport::Rest, 4_000),
        ]
    }

    fn key(budget: Budget, spend: SpendGroup) -> ViewKey {
        ViewKey::new(
            AS_OF,
            UsageView::Budget,
            Preset::Trailing(Span::H24),
            budget,
            spend,
            RequestQuery::default(),
        )
    }

    #[test]
    fn a_landed_read_carries_the_d7_default_and_its_key() {
        let landed = land(
            rows(),
            false,
            AS_OF,
            controls(UsageView::Requests, Preset::CurrentWindow),
        );
        assert_eq!(landed.budget, Budget::Graphql, "20 % left beats 80 %");
        // The key holds what the view reads: Requests under Current
        // window reads the trailing hour (D11).
        assert_eq!(
            landed.key,
            ViewKey {
                as_of_ms: AS_OF,
                view: UsageView::Requests,
                preset: Preset::Trailing(Span::H1),
                budget: Budget::Graphql,
                spend: SpendGroup::default(),
                requests: RequestQuery::default(),
            }
        );
        assert_eq!(landed.rows.len(), 2);
        assert_eq!(
            landed.views.budget, None,
            "Requests derives no Budget bands"
        );
        assert!(landed.views.requests.is_some());
    }

    #[test]
    fn a_picked_budget_survives_a_read() {
        let mut c = controls(UsageView::Budget, Preset::Trailing(Span::H24));
        c.picked_budget = Some(Budget::Rest);
        let landed = land(rows(), false, AS_OF, c);
        assert_eq!(landed.budget, Budget::Rest, "the pick beats the default");
        let b = landed.views.budget.expect("Budget bands");
        assert_eq!(
            (b.spend.budget, b.history.budget),
            (Budget::Rest, Budget::Rest)
        );
        assert_eq!(b.cards.cards.len(), 2, "the cards always show both");
    }

    #[test]
    fn a_band_re_derives_only_when_its_own_inputs_change() {
        let base = key(Budget::Rest, SpendGroup::default());
        let regroup = key(
            Budget::Rest,
            SpendGroup {
                group_by: SpendDimension::Operation,
                then_by: Some(SpendDimension::Trigger),
            },
        );
        let rebudget = key(Budget::Graphql, SpendGroup::default());
        let later = ViewKey {
            as_of_ms: AS_OF + 1,
            ..base.clone()
        };
        let all = [Band::Cards, Band::Where, Band::History, Band::Avoidable];
        let stale = |to: &ViewKey| -> Vec<Band> {
            all.into_iter().filter(|b| b.stale(&base, to)).collect()
        };
        assert_eq!(stale(&base), []);
        assert_eq!(stale(&regroup), [Band::Where]);
        assert_eq!(
            stale(&rebudget),
            [Band::Where, Band::History, Band::Avoidable]
        );
        assert_eq!(stale(&later), all);
    }

    #[test]
    fn unchanged_bands_are_carried_not_recomputed() {
        let rows = rows();
        let base = key(Budget::Rest, SpendGroup::default());
        let first = derive_views(&rows, base, None);
        let regroup = key(
            Budget::Rest,
            SpendGroup {
                group_by: SpendDimension::Operation,
                then_by: None,
            },
        );
        let second = derive_views(&rows, regroup.clone(), Some(&first));
        let (a, b) = (first.budget.unwrap(), second.budget.clone().unwrap());
        assert!(Arc::ptr_eq(&a.cards, &b.cards));
        assert!(Arc::ptr_eq(&a.history, &b.history));
        assert!(Arc::ptr_eq(&a.avoidable, &b.avoidable));
        assert!(!Arc::ptr_eq(&a.spend, &b.spend), "band 2 re-derived");
        assert_eq!(b.key, regroup);
        // Requests carries the Budget bands so a switch back reuses them.
        let req = ViewKey::new(
            AS_OF,
            UsageView::Requests,
            Preset::Trailing(Span::H24),
            Budget::Rest,
            SpendGroup::default(),
            RequestQuery::default(),
        );
        let on_requests = derive_views(&rows, req.clone(), Some(&second));
        assert_eq!(on_requests.budget.as_ref().map(|d| &d.key), Some(&regroup));
        assert!(!on_requests.budget.unwrap().ready(Band::Cards, &req));
    }

    #[test]
    fn the_requests_key_ignores_the_budget_grouping() {
        let spend = SpendGroup {
            group_by: SpendDimension::Target,
            then_by: None,
        };
        let k = ViewKey::new(
            AS_OF,
            UsageView::Requests,
            Preset::Trailing(Span::D7),
            Budget::Rest,
            spend,
            RequestQuery::default(),
        );
        assert_eq!(k.spend, SpendGroup::default());
    }
}
