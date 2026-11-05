//! The Requests view's derivation (docs/0082 D10, D11): its five bands
//! over the frozen rows, off the UI thread like the Budget view's, each
//! reused while its own inputs are unchanged ([`RequestBand::stale`]): a
//! `Measure` or `Group by` change re-derives the trend alone, a log sort
//! the log alone, and a facet everything but the module list.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::{UsageView, ViewKey, local_offset_ms};
use crate::usage_analytics::Dimension;
use crate::usage_analytics::surfaces::diagnostics::{
    self, Diagnostics, RequestGlance, RequestScope,
};
use crate::usage_analytics::surfaces::{
    self, LogSort, Measure, Preset, RequestFilters, RequestLog, RequestLogQuery, RequestTrend,
    RequestTrendQuery, Span,
};
use crate::usage_records::UsageRecord;

/// D10's query bar plus the log's sort: everything the Requests bands
/// read besides the range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::usage_analytics::page) struct RequestQuery {
    pub filters: RequestFilters,
    pub measure: Measure,
    pub group_by: Dimension,
    pub then_by: Option<Dimension>,
    pub log_sort: LogSort,
}

impl Default for RequestQuery {
    /// p90 (D10, 0054 D12: latency is heavy-tailed), by operation,
    /// newest first.
    fn default() -> Self {
        RequestQuery {
            filters: RequestFilters::default(),
            measure: Measure::P90,
            group_by: Dimension::Operation,
            then_by: None,
            log_sort: LogSort::default(),
        }
    }
}

/// The Requests view's bands (D10), in page order, plus the Module
/// facet's options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::usage_analytics::page) enum RequestBand {
    Glance,
    /// The trend and the ranking table that mirrors it.
    Trend,
    Diagnostics,
    Log,
    /// The modules in the read, for the Module facet.
    Modules,
}

impl RequestBand {
    /// Whether this band's data, derived for `shown`, is out of date for
    /// `target`.
    pub(in crate::usage_analytics::page) fn stale(self, shown: &ViewKey, target: &ViewKey) -> bool {
        let (a, b) = (&shown.requests, &target.requests);
        let window = shown.as_of_ms != target.as_of_ms || shown.preset != target.preset;
        let facets = window || a.filters != b.filters;
        match self {
            RequestBand::Modules => shown.as_of_ms != target.as_of_ms,
            RequestBand::Glance | RequestBand::Diagnostics => facets,
            RequestBand::Trend => {
                facets
                    || a.measure != b.measure
                    || a.group_by != b.group_by
                    || a.then_by != b.then_by
            }
            RequestBand::Log => facets || a.log_sort != b.log_sort,
        }
    }
}

/// The Requests view's derived bands, each shared so an unchanged band
/// is carried into the next derivation without a copy.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::usage_analytics::page) struct RequestsData {
    /// The key the bands were derived for (a Requests-view key).
    pub key: ViewKey,
    pub glance: Arc<RequestGlance>,
    pub trend: Arc<RequestTrend>,
    pub diagnostics: Arc<Diagnostics>,
    pub log: Arc<RequestLog>,
    /// Every module in the read, sorted.
    pub modules: Arc<Vec<String>>,
}

impl RequestsData {
    /// Whether `band` is ready to draw for `target`; a stale band shows
    /// its skeleton until the derivation in flight lands (D18).
    pub(in crate::usage_analytics::page) fn ready(
        &self,
        band: RequestBand,
        target: &ViewKey,
    ) -> bool {
        target.view == UsageView::Requests && !band.stale(&self.key, target)
    }
}

/// The span a Requests key reads: always trailing (D11; `Current
/// window` already resolved to `1h` by the key).
pub(in crate::usage_analytics::page) fn span(preset: Preset) -> Span {
    match preset {
        Preset::Trailing(s) => s,
        Preset::CurrentWindow => Span::H1,
    }
}

pub(super) fn derive_requests(
    rows: &[UsageRecord],
    key: &ViewKey,
    prev: Option<&RequestsData>,
) -> RequestsData {
    let keep = |band: RequestBand| prev.filter(|p| !band.stale(&p.key, key));
    let q = &key.requests;
    let span = span(key.preset);
    let tz_offset_ms = local_offset_ms(key.as_of_ms);
    let scope = || RequestScope {
        as_of_ms: key.as_of_ms,
        span,
        tz_offset_ms,
        filters: q.filters.clone(),
    };
    RequestsData {
        key: key.clone(),
        glance: keep(RequestBand::Glance).map_or_else(
            || Arc::new(diagnostics::request_glance(rows, &scope())),
            |p| p.glance.clone(),
        ),
        trend: keep(RequestBand::Trend).map_or_else(
            || {
                Arc::new(surfaces::request_trend(
                    rows,
                    &RequestTrendQuery {
                        as_of_ms: key.as_of_ms,
                        span,
                        tz_offset_ms,
                        filters: q.filters.clone(),
                        measure: q.measure,
                        group_by: q.group_by,
                        then_by: q.then_by,
                    },
                ))
            },
            |p| p.trend.clone(),
        ),
        diagnostics: keep(RequestBand::Diagnostics).map_or_else(
            || Arc::new(diagnostics::request_diagnostics(rows, &scope())),
            |p| p.diagnostics.clone(),
        ),
        log: keep(RequestBand::Log).map_or_else(
            || {
                Arc::new(surfaces::request_log(
                    rows,
                    &RequestLogQuery {
                        as_of_ms: key.as_of_ms,
                        span,
                        filters: q.filters.clone(),
                        sort: q.log_sort,
                    },
                ))
            },
            |p| p.log.clone(),
        ),
        modules: keep(RequestBand::Modules).map_or_else(
            || {
                let set: BTreeSet<&str> = rows.iter().map(|r| r.module.as_ref()).collect();
                Arc::new(set.into_iter().map(String::from).collect())
            },
            |p| p.modules.clone(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{SpendGroup, ViewData, derive_views};
    use super::*;
    use crate::usage_analytics::surfaces::{Budget, LogSortKey, StatusClass};
    use crate::usage_records::Transport;

    const AS_OF: u64 = 10_000_000_000;
    const MIN: u64 = 60_000;

    fn rec(module: &str, transport: Transport, duration_ms: u32) -> UsageRecord {
        let mut r = UsageRecord::new(module, "op", "home", "view-shown");
        r.ts_epoch_ms = AS_OF - MIN;
        r.transport = transport;
        r.status = 200;
        r.cost = Some(1);
        r.duration_ms = duration_ms;
        r
    }

    fn rows() -> Vec<UsageRecord> {
        vec![
            rec("github", Transport::Rest, 120),
            rec("github", Transport::Auth, 900),
            rec("favicon", Transport::Local, 1_400),
        ]
    }

    fn key(q: RequestQuery) -> ViewKey {
        ViewKey::new(
            AS_OF,
            UsageView::Requests,
            Preset::Trailing(Span::H24),
            Budget::Rest,
            SpendGroup::default(),
            q,
        )
    }

    fn requests(v: &ViewData) -> &RequestsData {
        v.requests.as_ref().expect("Requests bands")
    }

    #[test]
    fn a_requests_key_derives_every_band_including_auth_and_local() {
        let v = derive_views(&rows(), key(RequestQuery::default()), None);
        let r = requests(&v);
        assert_eq!(r.glance.requests, 3, "D16: auth and local are here");
        assert_eq!(r.trend.measure, Measure::P90);
        assert_eq!(r.log.rows.len(), 3);
        assert_eq!(r.diagnostics.histogram.requests, 3);
        assert_eq!(*r.modules, ["favicon", "github"]);
        assert_eq!(v.budget, None, "Requests derives no Budget bands");
    }

    #[test]
    fn a_band_re_derives_only_when_its_own_inputs_change() {
        let base = key(RequestQuery::default());
        let all = [
            RequestBand::Glance,
            RequestBand::Trend,
            RequestBand::Diagnostics,
            RequestBand::Log,
            RequestBand::Modules,
        ];
        let stale = |q: RequestQuery| -> Vec<RequestBand> {
            let to = key(q);
            all.into_iter().filter(|b| b.stale(&base, &to)).collect()
        };
        let measure = RequestQuery {
            measure: Measure::Count,
            ..Default::default()
        };
        assert_eq!(stale(measure), [RequestBand::Trend]);
        let then_by = RequestQuery {
            then_by: Some(Dimension::Trigger),
            ..Default::default()
        };
        assert_eq!(stale(then_by), [RequestBand::Trend]);
        let sort = RequestQuery {
            log_sort: LogSort {
                key: LogSortKey::Duration,
                descending: true,
            },
            ..Default::default()
        };
        assert_eq!(stale(sort), [RequestBand::Log]);
        let facet = RequestQuery {
            filters: RequestFilters {
                status_class: Some(StatusClass::NotModified),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(&stale(facet)[..], &all[..4], "the module list stays");
        // A budget pick (Budget view only) re-derives nothing here.
        let rebudget = ViewKey {
            budget: Budget::Graphql,
            ..base.clone()
        };
        assert!(all.iter().all(|b| !b.stale(&base, &rebudget)));
    }

    #[test]
    fn unchanged_bands_are_carried_and_budget_bands_survive() {
        let rows = rows();
        let first = derive_views(&rows, key(RequestQuery::default()), None);
        let sorted = key(RequestQuery {
            log_sort: LogSort {
                key: LogSortKey::Duration,
                descending: true,
            },
            ..Default::default()
        });
        let second = derive_views(&rows, sorted.clone(), Some(&first));
        let (a, b) = (requests(&first), requests(&second));
        assert!(Arc::ptr_eq(&a.glance, &b.glance));
        assert!(Arc::ptr_eq(&a.trend, &b.trend));
        assert!(Arc::ptr_eq(&a.diagnostics, &b.diagnostics));
        assert!(!Arc::ptr_eq(&a.log, &b.log), "the log re-sorted");
        assert_eq!(b.log.rows[0].duration_ms, 1_400, "slowest first");
        assert!(b.ready(RequestBand::Log, &sorted));
        // Switching to Budget keeps the Requests bands for the way back.
        let budget = ViewKey::new(
            AS_OF,
            UsageView::Budget,
            Preset::Trailing(Span::H24),
            Budget::Rest,
            SpendGroup::default(),
            RequestQuery::default(),
        );
        let on_budget = derive_views(&rows, budget.clone(), Some(&second));
        let kept = requests(&on_budget);
        assert_eq!(kept.key, sorted);
        assert!(!kept.ready(RequestBand::Glance, &budget));
    }

    #[test]
    fn the_budget_key_ignores_the_requests_query() {
        let q = RequestQuery {
            measure: Measure::Max,
            ..Default::default()
        };
        let k = ViewKey::new(
            AS_OF,
            UsageView::Budget,
            Preset::Trailing(Span::D7),
            Budget::Rest,
            SpendGroup::default(),
            q,
        );
        assert_eq!(k.requests, RequestQuery::default());
        assert_eq!(span(Preset::CurrentWindow), Span::H1);
    }
}
