//! What the views derive off the UI thread (docs/0082 D11 decision): a
//! read lands with the D7 default budget and the active view's data
//! already computed on the background executor, and a control change
//! re-derives there too, behind the band skeletons. Everything here is
//! pure: rows and a key in, owned data out.
//!
//! This is the seam #697 (Budget bands) and #698 (Requests bands) fill:
//! they add their outputs to [`ViewData`] and their procedure calls to
//! [`derive_views`]; the page's read lifecycle stays as it is.

use super::UsageView;
use super::range::{self, BUDGET_MODULE};
use crate::usage_analytics::surfaces::{self, Budget, Preset};
use crate::usage_records::UsageRecord;

/// Everything a view's derivation depends on besides the rows. A
/// control change that leaves the key unchanged re-derives nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ViewKey {
    pub as_of_ms: u64,
    pub view: UsageView,
    /// The preset the view reads ([`range::effective_preset`]).
    pub preset: Preset,
    pub budget: Budget,
}

impl ViewKey {
    pub(super) fn new(as_of_ms: u64, view: UsageView, preset: Preset, budget: Budget) -> Self {
        ViewKey {
            as_of_ms,
            view,
            preset: range::effective_preset(view, preset),
            budget,
        }
    }
}

/// The active view's derived data. Empty until the views land: #697
/// and #698 add their band outputs here.
#[derive(Debug, Default, PartialEq)]
pub(super) struct ViewData {}

/// The active view's band procedures over the frozen rows (D15a).
pub(super) fn derive_views(_rows: &[UsageRecord], _key: ViewKey) -> ViewData {
    ViewData::default()
}

/// The D7 default (Q6): the budget with the smaller remaining fraction,
/// the one `Current window` follows until #697's selector lands.
pub(super) fn default_budget(rows: &[UsageRecord], as_of_ms: u64) -> Budget {
    surfaces::budget_cards(
        rows,
        &surfaces::BudgetCardsQuery {
            module: BUDGET_MODULE.into(),
            as_of_ms,
            preset: Preset::CurrentWindow,
            // The per-budget pause reaches the page with #699.
            rest_paused_until_ms: None,
            graphql_paused_until_ms: None,
        },
    )
    .default_budget
}

/// A finished read, derived in the read's background task.
pub(super) struct Landed {
    pub rows: Vec<UsageRecord>,
    pub degraded: bool,
    pub budget: Budget,
    pub key: ViewKey,
    pub views: ViewData,
}

/// The read's background half after the store returns: the default
/// budget, then the views for the controls as they stood at read time.
pub(super) fn land(
    rows: Vec<UsageRecord>,
    degraded: bool,
    as_of_ms: u64,
    view: UsageView,
    preset: Preset,
) -> Landed {
    let budget = default_budget(&rows, as_of_ms);
    let key = ViewKey::new(as_of_ms, view, preset, budget);
    let views = derive_views(&rows, key);
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
        r.remaining_after = Some(remaining);
        r.limit_after = Some(5_000);
        r.cycle_reset_epoch = Some((AS_OF + 30 * MIN) / 1_000);
        r
    }

    #[test]
    fn a_landed_read_carries_the_d7_default_and_its_key() {
        let landed = land(
            vec![
                snap(Transport::Graphql, 1_000),
                snap(Transport::Rest, 4_000),
            ],
            false,
            AS_OF,
            UsageView::Requests,
            Preset::CurrentWindow,
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
            }
        );
        assert_eq!(landed.rows.len(), 2);
    }
}
