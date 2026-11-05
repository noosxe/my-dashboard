//! Hand-made record fixtures (exact by construction): the seeded
//! ledger generator lives in the GitHub crate, which core can't depend
//! on, and exact numbers are what these assertions need.

use super::*;

const MIN: u64 = MINUTE_MS;
/// 13:47 UTC on day 20 000 — mid-day, minute-aligned.
const AS_OF: u64 = 20_000 * DAY_MS + 13 * HOUR_MS + 47 * MIN;

/// A costed REST 200 at `ts`, no snapshot.
fn rec(ts: u64, transport: Transport, op: &str) -> UsageRecord {
    let mut r = UsageRecord::new("github", op, "pr-page", "view-shown");
    r.ts_epoch_ms = ts;
    r.transport = transport;
    r.status = 200;
    r.cost = Some(1);
    r
}

/// `r` carrying a snapshot: `remaining` of `limit`, resetting at
/// `reset_ms` (stored as epoch seconds, as the module records it).
fn snap(mut r: UsageRecord, remaining: u32, limit: u32, reset_ms: u64) -> UsageRecord {
    r.remaining_after = Some(remaining);
    r.limit_after = Some(limit);
    r.cycle_reset_epoch = Some(reset_ms / 1_000);
    r
}

fn rest_snap(ts: u64, remaining: u32, reset_ms: u64) -> UsageRecord {
    snap(
        rec(ts, Transport::Rest, "pr-detail"),
        remaining,
        5_000,
        reset_ms,
    )
}

fn gql_snap(ts: u64, remaining: u32, reset_ms: u64) -> UsageRecord {
    snap(
        rec(ts, Transport::Graphql, "home"),
        remaining,
        5_000,
        reset_ms,
    )
}

fn scope(budget: Budget, preset: Preset) -> BudgetScope {
    BudgetScope {
        module: "github".into(),
        budget,
        as_of_ms: AS_OF,
        preset,
    }
}

fn cards_query(preset: Preset) -> BudgetCardsQuery {
    BudgetCardsQuery {
        module: "github".into(),
        as_of_ms: AS_OF,
        preset,
        rest_paused_until_ms: None,
        graphql_paused_until_ms: None,
    }
}

fn card(cards: &BudgetCards, budget: Budget) -> &BudgetCard {
    cards.cards.iter().find(|c| c.budget == budget).unwrap()
}

const DAY: Preset = Preset::Trailing(Span::H24);

// --- window maths -----------------------------------------------------------

#[test]
fn current_window_follows_the_selected_budget() {
    // Q5: REST and GraphQL reset at different times, so `Current
    // window` resolves per budget, from `reset − 1 h` to `as_of`.
    let rest_reset = AS_OF + 13 * MIN;
    let gql_reset = AS_OF + 5 * MIN;
    let rows = vec![
        // The previous REST window: outside the current one.
        rest_snap(rest_reset - WINDOW_MS - MIN, 4_000, rest_reset - WINDOW_MS),
        rest_snap(rest_reset - WINDOW_MS + MIN, 4_999, rest_reset),
        rest_snap(AS_OF - MIN, 4_990, rest_reset),
        gql_snap(AS_OF - 2 * MIN, 1_140, gql_reset),
    ];
    let rest = scope(Budget::Rest, Preset::CurrentWindow);
    assert_eq!(
        rest.bounds(&rows),
        Some(Bounds {
            from_ms: rest_reset - WINDOW_MS,
            to_ms: AS_OF
        })
    );
    let gql = scope(Budget::Graphql, Preset::CurrentWindow);
    assert_eq!(gql.bounds(&rows).unwrap().from_ms, gql_reset - WINDOW_MS);

    let w = where_it_went(
        &rows,
        &WhereQuery {
            scope: rest,
            group_by: SpendDimension::Operation,
            then_by: None,
        },
    );
    assert_eq!(w.totals.requests, 2, "the previous window stays out");
    assert_eq!(w.unit, "requests");
}

#[test]
fn current_window_without_an_open_window_is_empty_not_guessed() {
    // The only REST snapshot's window reset before `as_of`: the new
    // window's start is unknown.
    let rows = vec![rest_snap(AS_OF - 30 * MIN, 4_000, AS_OF - 10 * MIN)];
    let s = scope(Budget::Rest, Preset::CurrentWindow);
    assert_eq!(s.bounds(&rows), None);
    let h = window_history(
        &rows,
        &HistoryQuery {
            scope: s.clone(),
            tz_offset_ms: 0,
        },
    );
    assert_eq!(h.view, HistoryView::NoWindow);
    let cards = budget_cards(&rows, &cards_query(Preset::CurrentWindow));
    assert_eq!(
        card(&cards, Budget::Rest).projection,
        Projection::NoSnapshot
    );
}

// --- D4/D6 cards and projection -----------------------------------------------

/// Five GraphQL snapshots in a window that started 40 min ago, the
/// latest showing `remaining` of 5,000. Each response costs 2 points
/// in this app's records.
fn gql_window(remaining: u32, n: u64) -> (Vec<UsageRecord>, u64) {
    let reset = AS_OF + 20 * MIN;
    let rows = (0..n)
        .map(|i| {
            let mut r = gql_snap(AS_OF - (n - i) * MIN, remaining + (n - 1 - i) as u32, reset);
            r.cost = Some(2);
            r
        })
        .collect();
    (rows, reset)
}

#[test]
fn projection_guard_needs_five_snapshots() {
    let (rows, _) = gql_window(1_000, 4);
    let cards = budget_cards(&rows, &cards_query(DAY));
    assert_eq!(
        card(&cards, Budget::Graphql).projection,
        Projection::NotEnoughData {
            responses: 4,
            elapsed_ms: 40 * MIN
        }
    );
}

#[test]
fn projection_guard_needs_five_minutes() {
    // Five snapshots, but the window started 4 minutes ago.
    let reset = AS_OF + 56 * MIN;
    let rows: Vec<_> = (1..=5)
        .map(|i| gql_snap(AS_OF - i * 30_000, 4_000, reset))
        .collect();
    let cards = budget_cards(&rows, &cards_query(DAY));
    assert_eq!(
        card(&cards, Budget::Graphql).projection,
        Projection::NotEnoughData {
            responses: 5,
            elapsed_ms: 4 * MIN
        }
    );
}

#[test]
fn projection_paces_on_the_window_drop_not_recorded_cost() {
    // Q4: GitHub reports 4,000 spent in 40 min (100/min) while this app
    // recorded only 10 points: other clients drain the token.
    let (rows, reset) = gql_window(1_000, 5);
    let cards = budget_cards(&rows, &cards_query(DAY));
    let c = card(&cards, Budget::Graphql);
    // 1,000 left at 100/min → empty in 10 min, before the reset.
    assert_eq!(
        c.projection,
        Projection::RunsOut {
            at_ms: AS_OF + 10 * MIN,
            pace_per_min: 100.0
        }
    );
    let d = c.window_drop.as_ref().unwrap();
    assert_eq!(d.window.reset_ms, reset);
    assert_eq!(d.window.start_ms, reset - WINDOW_MS);
    assert_eq!((d.spent, d.recorded_cost, d.external), (4_000, 10, 3_990));
    assert_eq!(d.responses, 5);
    assert_eq!(c.state, Some(ThresholdState::Low), "1,000 of 5,000 is 20 %");
}

#[test]
fn projection_on_track_when_the_reset_comes_first() {
    // 1,000 spent in 40 min → 4,000 left lasts 160 min > 20 min.
    let (rows, _) = gql_window(4_000, 5);
    let cards = budget_cards(&rows, &cards_query(DAY));
    let c = card(&cards, Budget::Graphql);
    assert_eq!(c.projection, Projection::OnTrack { pace_per_min: 25.0 });
    assert_eq!(c.state, Some(ThresholdState::Normal));
    let s = c.snapshot.as_ref().unwrap();
    assert_eq!((s.remaining, s.limit), (4_000, Some(5_000)));
    assert_eq!(s.remaining_pct, Some(80.0));
    assert_eq!(s.snapshot_age_ms, MIN);
    assert!(s.window_open);
}

#[test]
fn exhausted_from_the_snapshot_or_the_pause() {
    let reset = AS_OF + 7 * MIN;
    let rows = vec![
        rest_snap(AS_OF - MIN, 0, reset),
        gql_snap(AS_OF - MIN, 4_000, AS_OF + 30 * MIN),
    ];
    let mut q = cards_query(DAY);
    let cards = budget_cards(&rows, &q);
    let rest = card(&cards, Budget::Rest);
    assert_eq!(
        rest.projection,
        Projection::Exhausted {
            resumes_at_ms: reset
        }
    );
    assert_eq!(rest.state, Some(ThresholdState::Exhausted));

    // The per-budget pause (Q10) outranks a healthy snapshot, and only
    // for its own budget; a pause already over is ignored.
    q.graphql_paused_until_ms = Some(AS_OF + 3 * MIN);
    q.rest_paused_until_ms = Some(AS_OF - MIN);
    let cards = budget_cards(&rows, &q);
    let gql = card(&cards, Budget::Graphql);
    assert_eq!(
        gql.projection,
        Projection::Exhausted {
            resumes_at_ms: AS_OF + 3 * MIN
        }
    );
    assert_eq!(gql.state, Some(ThresholdState::Exhausted));
    assert_eq!(
        card(&cards, Budget::Rest).projection,
        Projection::Exhausted {
            resumes_at_ms: reset
        },
        "REST keeps its own state"
    );
}

#[test]
fn no_snapshot_is_its_own_result_never_zero() {
    // GraphQL answered nothing usable: a network error, no snapshot.
    let mut failed = rec(AS_OF - MIN, Transport::Graphql, "home");
    failed.network_error = true;
    failed.status = 0;
    failed.cost = None;
    let rows = vec![failed, rest_snap(AS_OF - MIN, 4_000, AS_OF + 30 * MIN)];
    let cards = budget_cards(&rows, &cards_query(DAY));
    let gql = card(&cards, Budget::Graphql);
    assert_eq!(gql.snapshot, None);
    assert_eq!(gql.state, None);
    assert_eq!(gql.projection, Projection::NoSnapshot);
    assert_eq!(gql.window_drop, None);
    assert_eq!(
        gql.recorded,
        SpendTotals {
            requests: 1,
            known_cost: 0,
            unknown: 1
        }
    );
}

#[test]
fn a_closed_window_keeps_verbatim_numbers_but_no_state() {
    let rows = vec![rest_snap(AS_OF - 30 * MIN, 400, AS_OF - 10 * MIN)];
    let cards = budget_cards(&rows, &cards_query(DAY));
    let rest = card(&cards, Budget::Rest);
    let s = rest.snapshot.as_ref().unwrap();
    assert_eq!(s.remaining, 400);
    assert!(!s.window_open);
    assert_eq!(rest.projection, Projection::WindowClosed);
    assert_eq!(rest.state, None, "GitHub has refilled it since");
}

#[test]
fn threshold_ladder_edges() {
    let of = |r| ThresholdState::of(r, Some(5_000));
    assert_eq!(of(1_251), Some(ThresholdState::Normal));
    assert_eq!(of(1_250), Some(ThresholdState::Low));
    assert_eq!(of(501), Some(ThresholdState::Low));
    assert_eq!(of(500), Some(ThresholdState::Critical));
    assert_eq!(of(0), Some(ThresholdState::Exhausted));
    assert_eq!(ThresholdState::of(10, None), None);
    assert_eq!(ThresholdState::of(10, Some(0)), None);
}

// --- D7 default budget ----------------------------------------------------------

#[test]
fn default_budget_is_the_smaller_fraction_rest_on_tie_or_missing() {
    let open = AS_OF + 30 * MIN;
    let pick = |rows: Vec<UsageRecord>| budget_cards(&rows, &cards_query(DAY)).default_budget;
    assert_eq!(
        pick(vec![
            rest_snap(AS_OF - MIN, 3_712, open),
            gql_snap(AS_OF - MIN, 1_140, open)
        ]),
        Budget::Graphql
    );
    assert_eq!(
        pick(vec![
            rest_snap(AS_OF - MIN, 1_000, open),
            gql_snap(AS_OF - MIN, 4_000, open)
        ]),
        Budget::Rest
    );
    assert_eq!(
        pick(vec![
            rest_snap(AS_OF - MIN, 2_500, open),
            gql_snap(AS_OF - MIN, 2_500, open)
        ]),
        Budget::Rest,
        "tie"
    );
    assert_eq!(
        pick(vec![gql_snap(AS_OF - MIN, 10, open)]),
        Budget::Rest,
        "no REST snapshot"
    );
    assert_eq!(pick(Vec::new()), Budget::Rest, "no snapshot at all");
    assert_eq!(
        pick(vec![
            rest_snap(AS_OF - MIN, 4_000, open),
            // A lower fraction from a window that has since reset.
            gql_snap(AS_OF - 30 * MIN, 100, AS_OF - 10 * MIN)
        ]),
        Budget::Rest,
        "a closed window's fraction is stale, so unknown"
    );

    // A paused budget counts as 0 % even over a stale healthy snapshot.
    let rows = vec![
        rest_snap(AS_OF - MIN, 1_000, open),
        gql_snap(AS_OF - MIN, 4_900, open),
    ];
    let mut q = cards_query(DAY);
    q.graphql_paused_until_ms = Some(AS_OF + 5 * MIN);
    assert_eq!(budget_cards(&rows, &q).default_budget, Budget::Graphql);
}

// --- D7 where it went -----------------------------------------------------------

#[test]
fn where_it_went_never_pools_budgets_and_skips_auth_local() {
    let t = AS_OF - HOUR_MS;
    let mut gql = rec(t, Transport::Graphql, "home");
    gql.cost = Some(50);
    let mut auth = rec(t, Transport::Auth, "device-flow");
    auth.cost = None;
    let mut local = rec(t, Transport::Local, "export");
    local.cost = None;
    let mut other_module = rec(t, Transport::Rest, "issues");
    other_module.module = "forgejo".into();
    let mut free = rec(t, Transport::Rest, "pr-detail");
    free.etag_304 = true;
    free.status = 304;
    free.cost = Some(0);
    let mut failed = rec(t, Transport::Rest, "pr-detail");
    failed.network_error = true;
    failed.status = 0;
    failed.cost = None;
    let mut forbidden = rec(t, Transport::Rest, "pr-detail");
    forbidden.status = 403;
    let rows = vec![
        rec(t, Transport::Rest, "pr-detail"),
        rec(t, Transport::Rest, "pr-detail"),
        free,
        failed,
        forbidden,
        gql,
        auth,
        local,
        other_module,
    ];
    let q = |budget| WhereQuery {
        scope: scope(budget, DAY),
        group_by: SpendDimension::Operation,
        then_by: None,
    };
    let rest = where_it_went(&rows, &q(Budget::Rest));
    assert_eq!(
        rest.totals,
        SpendTotals {
            requests: 5,
            known_cost: 3,
            unknown: 1
        }
    );
    assert_eq!(rest.groups.len(), 1);
    let g = &rest.groups[0];
    assert_eq!(g.key, "pr-detail");
    assert_eq!(
        (g.cost, g.requests, g.free_304, g.errors, g.unknown),
        (3, 5, 1, 2, 1)
    );
    assert_eq!(g.avg_cost, Some(0.75), "3 over the 4 costed requests");
    assert_eq!(g.share_pct, Some(100.0));
    assert_eq!(rest.other, None);

    let gql = where_it_went(&rows, &q(Budget::Graphql));
    assert_eq!(gql.totals.known_cost, 50);
    assert_eq!(gql.unit, "points");
    assert_eq!(gql.groups.len(), 1);
}

#[test]
fn where_it_went_top_eight_plus_other_and_then_by() {
    let t = AS_OF - HOUR_MS;
    let mut rows = Vec::new();
    // Ten surfaces; surface i costs i + 1, each spread over two
    // triggers.
    for i in 0..10u32 {
        for trigger in ["poll", "manual"] {
            let mut r = rec(t, Transport::Rest, "pr-detail");
            r.surface = format!("s{i}").into();
            r.trigger = trigger.into();
            r.cost = Some(i + 1);
            rows.push(r);
        }
    }
    let w = where_it_went(
        &rows,
        &WhereQuery {
            scope: scope(Budget::Rest, DAY),
            group_by: SpendDimension::Surface,
            then_by: Some(SpendDimension::Trigger),
        },
    );
    assert_eq!(w.groups.len(), 10);
    assert_eq!(w.groups[0].key, "s9");
    assert_eq!(w.groups[0].cost, 20);
    assert_eq!(w.groups[0].children.len(), 2);
    // Equal-cost children: requests tie, so key asc.
    assert_eq!(w.groups[0].children[0].key, "manual");
    let total = w.totals.known_cost;
    assert_eq!(total, 110);
    let other = w.other.as_ref().unwrap();
    // s1 and s0 fold: (2 + 1) × 2.
    assert_eq!(
        (other.key.as_str(), other.cost, other.requests),
        ("other", 6, 4)
    );
    assert!(other.children.is_empty());
    let cost_sum: u64 = w.groups[..WHERE_TOP_N].iter().map(|g| g.cost).sum::<u64>() + other.cost;
    assert_eq!(cost_sum, total, "top 8 plus other is the whole budget");
}

// --- D8 history -------------------------------------------------------------------

#[test]
fn per_window_columns_rise_to_the_reported_drop() {
    let r1 = AS_OF - 3 * HOUR_MS;
    let r2 = AS_OF - HOUR_MS;
    let mut costly = rest_snap(r1 - 30 * MIN, 1_000, r1);
    costly.cost = Some(5);
    // The open window's response carried no ceiling, and it is the
    // costliest by this app's records.
    let mut no_ceiling = rest_snap(AS_OF - 10 * MIN, 4_000, AS_OF + 30 * MIN);
    no_ceiling.limit_after = None;
    no_ceiling.cost = Some(9);
    let rows = vec![
        no_ceiling,
        rest_snap(r1 - 50 * MIN, 4_000, r1),
        costly,
        // Between the windows: no snapshot of its own window.
        rec(r1 + 10 * MIN, Transport::Rest, "pr-detail"),
        rest_snap(r2 - 20 * MIN, 4_900, r2),
        // GraphQL never lands in REST history.
        gql_snap(r2 - 10 * MIN, 10, r2 + 5 * MIN),
    ];
    let h = window_history(
        &rows,
        &HistoryQuery {
            scope: scope(Budget::Rest, DAY),
            tz_offset_ms: 0,
        },
    );
    let HistoryView::Windows { columns, scale } = &h.view else {
        panic!("≤ 7 days draws per-window columns: {:?}", h.view);
    };
    assert_eq!(columns.len(), 3);
    let (a, b, c) = (&columns[0], &columns[1], &columns[2]);
    assert_eq!((a.start_ms, a.reset_ms), (r1 - WINDOW_MS, r1));
    // Heights are GitHub's drops; no ceiling is a gap.
    assert_eq!(
        columns.iter().map(|c| c.peak_drop).collect::<Vec<_>>(),
        vec![Some(4_000), Some(100), None]
    );
    assert_eq!(a.state, Some(ThresholdState::Low), "reached 20 % left");
    assert_eq!(b.state, Some(ThresholdState::Normal));
    assert_eq!(c.state, None, "no ceiling, no fraction, no state");
    // The recorded figures stay for the tooltip and a11y summary.
    assert_eq!(
        columns
            .iter()
            .map(|c| (c.cost, c.requests))
            .collect::<Vec<_>>(),
        vec![(6, 2), (1, 1), (9, 1)]
    );
    assert_eq!(
        h.unwindowed,
        SpendTotals {
            requests: 1,
            known_cost: 1,
            unknown: 0
        }
    );
    // Auto-scale inputs (Q3): the tallest drop (not the costliest
    // recorded window) and the guides in spent terms (75 % and 90 %).
    assert_eq!(scale.data_max, 4_000);
    assert_eq!(scale.limit, Some(5_000));
    assert_eq!(scale.low_guide, Some(3_750.0));
    assert_eq!(scale.critical_guide, Some(4_500.0));
}

#[test]
fn per_day_columns_show_each_days_peak_window() {
    let day0 = 20_000 * DAY_MS - 5 * DAY_MS;
    let window = |reset: u64, cost: u32, min_remaining: u32| {
        let mut r = rest_snap(reset - 30 * MIN, min_remaining, reset);
        r.cost = Some(cost);
        r
    };
    // Day 2 has one window whose response carried no ceiling.
    let mut no_ceiling = window(day0 + 2 * DAY_MS + 4 * HOUR_MS, 7, 4_000);
    no_ceiling.limit_after = None;
    let rows = vec![
        // The cheap window drained most: other clients share the token.
        window(day0 + 3 * HOUR_MS, 5, 2_000),
        window(day0 + 9 * HOUR_MS, 12, 3_000),
        window(day0 + DAY_MS + 2 * HOUR_MS, 3, 400),
        no_ceiling,
    ];
    let h = window_history(
        &rows,
        &HistoryQuery {
            scope: scope(Budget::Rest, Preset::Trailing(Span::D30)),
            tz_offset_ms: 0,
        },
    );
    let HistoryView::Days { columns, scale } = &h.view else {
        panic!("30 days draws per-day columns: {:?}", h.view);
    };
    // 30 days back from 13:47 touches 31 calendar days; every one is
    // present, empty ones included.
    assert_eq!(columns.len(), 31);
    assert!(
        columns
            .windows(2)
            .all(|w| w[1].day_start_ms == w[0].day_end_ms)
    );
    let d0 = columns.iter().find(|d| d.day_start_ms == day0).unwrap();
    // The height is the reported drop (owner decision on #709), and it
    // names its own window — not the costlier one by recorded spend.
    assert_eq!(d0.peak_drop, Some(3_000));
    assert_eq!(d0.peak_window_reset_ms, Some(day0 + 3 * HOUR_MS));
    assert_eq!(
        (d0.peak_window_cost, d0.total_cost, d0.windows),
        (12, 17, 2)
    );
    assert_eq!(d0.state, Some(ThresholdState::Normal));
    let day = |ix: u64| {
        columns
            .iter()
            .find(|d| d.day_start_ms == day0 + ix * DAY_MS)
            .unwrap()
    };
    assert_eq!(day(1).peak_drop, Some(4_600));
    assert_eq!(day(1).state, Some(ThresholdState::Critical));
    let d2 = day(2);
    assert_eq!((d2.peak_drop, d2.peak_window_reset_ms), (None, None));
    assert_eq!((d2.windows, d2.total_cost, d2.state), (1, 7, None));
    let empty = columns.iter().find(|d| d.windows == 0).unwrap();
    assert_eq!((empty.peak_drop, empty.state), (None, None));
    // Auto-scale data max is the tallest drop, not the costliest window.
    assert_eq!(scale.data_max, 4_600);
    assert_eq!(scale.limit, Some(5_000));
    assert_eq!(scale.critical_guide, Some(4_500.0));
}

#[test]
fn per_day_columns_start_at_local_midnight() {
    // A window starting 23:30 UTC belongs to the next day at UTC+1.
    let midnight = 20_000 * DAY_MS - 2 * DAY_MS;
    let rows = vec![rest_snap(midnight - 20 * MIN, 4_000, midnight + 30 * MIN)];
    let q = |tz_offset_ms| HistoryQuery {
        scope: scope(Budget::Rest, Preset::Trailing(Span::D30)),
        tz_offset_ms,
    };
    let start_of_peak = |h: WindowHistory| match h.view {
        HistoryView::Days { columns, .. } => {
            columns
                .into_iter()
                .find(|d| d.windows == 1)
                .unwrap()
                .day_start_ms
        }
        v => panic!("{v:?}"),
    };
    assert_eq!(
        start_of_peak(window_history(&rows, &q(0))),
        midnight - DAY_MS
    );
    assert_eq!(
        start_of_peak(window_history(&rows, &q(HOUR_MS as i64))),
        midnight - HOUR_MS,
        "local midnight at UTC+1 is 23:00 UTC"
    );
}

#[test]
fn burn_down_steps_carry_to_as_of_with_remaining_guides() {
    let reset = AS_OF + 20 * MIN;
    let rows = vec![
        gql_snap(AS_OF - 30 * MIN, 4_800, reset),
        gql_snap(AS_OF - 10 * MIN, 4_500, reset),
    ];
    let h = window_history(
        &rows,
        &HistoryQuery {
            scope: scope(Budget::Graphql, Preset::CurrentWindow),
            tz_offset_ms: 0,
        },
    );
    let HistoryView::BurnDown(b) = &h.view else {
        panic!("{:?}", h.view);
    };
    assert_eq!(b.window.reset_ms, reset);
    assert_eq!(
        b.steps
            .iter()
            .map(|s| (s.ts_ms, s.remaining))
            .collect::<Vec<_>>(),
        vec![
            (AS_OF - 30 * MIN, 4_800),
            (AS_OF - 10 * MIN, 4_500),
            (AS_OF, 4_500)
        ]
    );
    // The burn-down axis is the whole budget; guides in remaining terms.
    assert_eq!(b.scale.data_max, 5_000);
    assert_eq!(b.scale.low_guide, Some(1_250.0));
    assert_eq!(b.scale.critical_guide, Some(500.0));
}

// --- D9 avoidable -------------------------------------------------------------------

#[test]
fn avoidable_categories_are_exclusive_and_ranked() {
    let t = AS_OF - HOUR_MS;
    let op = |name: &str, trigger: &str, status: u16, cost: Option<u32>| {
        let mut r = rec(t, Transport::Rest, name);
        r.trigger = trigger.into();
        r.status = status;
        r.etag_304 = status == 304;
        r.cost = cost;
        r
    };
    let mut rows = Vec::new();
    // notifications polls: mostly 304, two paid 200s, one paid error.
    for _ in 0..6 {
        rows.push(op("notifications", "poll", 304, Some(0)));
    }
    rows.push(op("notifications", "poll", 200, Some(1)));
    rows.push(op("notifications", "poll", 200, Some(1)));
    rows.push(op("notifications", "poll", 500, Some(1)));
    // pr-detail refocus spend: no 304s, so only the timer category.
    for _ in 0..3 {
        rows.push(op("pr-detail", "refocus", 200, Some(1)));
    }
    // A manual fetch is not avoidable; GraphQL is another budget.
    rows.push(op("home", "manual", 200, Some(4)));
    let mut gql = op("home", "poll", 500, Some(30));
    gql.transport = Transport::Graphql;
    rows.push(gql);

    let a = avoidable(
        &rows,
        &AvoidableQuery {
            scope: scope(Budget::Rest, DAY),
        },
    );
    assert_eq!(a.total_cost, 10);
    let got: Vec<_> = a
        .entries
        .iter()
        .map(|e| (e.op.as_str(), e.category, e.cost, e.requests))
        .collect();
    assert_eq!(
        got,
        vec![
            ("pr-detail", AvoidableCategory::TimerAndRefocus, 3, 3),
            ("notifications", AvoidableCategory::RevalidationGap, 2, 2),
            ("notifications", AvoidableCategory::Errors, 1, 1),
        ],
        "each request counted once: the error and the 200s never reappear as poll spend"
    );
    let gap = &a.entries[1];
    assert_eq!((gap.op_304s, gap.op_200s), (6, 2));
    assert_eq!(gap.share_pct, Some(20.0));
}

#[test]
fn avoidable_keeps_the_top_five() {
    let t = AS_OF - HOUR_MS;
    let rows: Vec<_> = (0..7u32)
        .map(|i| {
            let mut r = rec(t, Transport::Rest, &format!("op{i}"));
            r.trigger = "poll".into();
            r.cost = Some(i + 1);
            r
        })
        .collect();
    let a = avoidable(
        &rows,
        &AvoidableQuery {
            scope: scope(Budget::Rest, DAY),
        },
    );
    assert_eq!(a.total_entries, 7);
    assert_eq!(
        a.entries.iter().map(|e| e.op.as_str()).collect::<Vec<_>>(),
        vec!["op6", "op5", "op4", "op3", "op2"]
    );
}

// --- D10 Requests ---------------------------------------------------------------------

fn trend_query(filters: RequestFilters, measure: Measure) -> RequestTrendQuery {
    RequestTrendQuery {
        as_of_ms: AS_OF,
        span: Span::H24,
        tz_offset_ms: 0,
        filters,
        measure,
        group_by: Dimension::Transport,
        then_by: None,
    }
}

fn mixed_rows() -> Vec<UsageRecord> {
    let mut gql = rec(AS_OF - 2 * HOUR_MS, Transport::Graphql, "home");
    gql.cost = Some(40);
    gql.duration_ms = 900;
    let mut auth = rec(AS_OF - 3 * HOUR_MS, Transport::Auth, "device-flow");
    auth.cost = None;
    auth.duration_ms = 50;
    let mut local = rec(AS_OF - 3 * HOUR_MS, Transport::Local, "export");
    local.cost = None;
    local.duration_ms = 2_000;
    let mut slow = rec(AS_OF - 30 * MIN, Transport::Rest, "pr-detail");
    slow.duration_ms = 1_500;
    let mut failed = rec(AS_OF - 20 * MIN, Transport::Rest, "pr-detail");
    failed.network_error = true;
    failed.status = 0;
    failed.cost = None;
    failed.duration_ms = 3_000;
    let mut free = rec(AS_OF - 10 * MIN, Transport::Rest, "pr-detail");
    free.etag_304 = true;
    free.status = 304;
    free.cost = Some(0);
    free.duration_ms = 100;
    vec![gql, auth, local, slow, failed, free]
}

#[test]
fn trend_offers_cost_only_for_one_quota_transport() {
    let rows = mixed_rows();
    let all = request_trend(
        &rows,
        &trend_query(RequestFilters::default(), Measure::Cost),
    );
    assert!(!all.cost_allowed);
    assert!(all.buckets.iter().all(|b| b.value.is_none()));
    assert!(all.groups.iter().all(|g| g.cost.is_none()));
    assert_eq!(all.requests, 6, "auth and local are in the Requests view");

    let rest_only = RequestFilters {
        transport: Some(Transport::Rest),
        ..Default::default()
    };
    let rest = request_trend(&rows, &trend_query(rest_only, Measure::Cost));
    assert!(rest.cost_allowed);
    let total: f64 = rest.buckets.iter().filter_map(|b| b.value).sum();
    assert_eq!(total, 1.0, "the REST request; the 304 is free");
    assert_eq!(rest.groups[0].cost, Some(1));
    assert_eq!(rest.groups[0].unknown, 1);
}

#[test]
fn trend_buckets_align_to_the_clock() {
    let rows = mixed_rows();
    let t = request_trend(&rows, &trend_query(RequestFilters::default(), Measure::P90));
    assert_eq!(t.bucket_ms, HOUR_MS);
    // 13:47 yesterday → the 13:00 bucket opens the series; the 13:00
    // bucket today closes it.
    assert_eq!(
        t.buckets.first().unwrap().start_ms,
        AS_OF - DAY_MS - 47 * MIN
    );
    assert_eq!(t.buckets.len(), 25);
    let last = t.buckets.last().unwrap();
    assert_eq!(last.start_ms, AS_OF - 47 * MIN);
    // 13:00–14:00 holds the slow, failed and free REST requests.
    assert_eq!(last.requests, 3);
    assert_eq!(last.value, Some(3_000.0));
    assert!(
        t.buckets
            .iter()
            .any(|b| b.requests == 0 && b.value.is_none()),
        "an empty bucket has no percentile"
    );
    assert_eq!(t.errors, 1);
    // Ranked by p90 desc: local (2,000) … rest (3,000) leads.
    assert_eq!(t.groups[0].key, "rest");
    assert_eq!(t.groups[1].key, "local");
}

#[test]
fn request_log_cells_sorts_and_status_facet() {
    let rows = mixed_rows();
    let q = |filters, sort| RequestLogQuery {
        as_of_ms: AS_OF,
        span: Span::H24,
        filters,
        sort,
    };
    let log = request_log(&rows, &q(RequestFilters::default(), LogSort::default()));
    // Newest first by default.
    assert!(log.rows.windows(2).all(|w| w[0].ts_ms >= w[1].ts_ms));
    let cell = |op: &str, transport| {
        log.rows
            .iter()
            .find(|r| r.op == op && r.transport == transport)
            .map(|r| r.cost)
    };
    assert_eq!(cell("home", Transport::Graphql), Some(CostCell::Points(40)));
    assert_eq!(
        cell("device-flow", Transport::Auth),
        Some(CostCell::NoBudget)
    );
    assert_eq!(cell("export", Transport::Local), Some(CostCell::NoBudget));
    let rest_cells: Vec<_> = log
        .rows
        .iter()
        .filter(|r| r.transport == Transport::Rest)
        .map(|r| (r.cost, r.status_class))
        .collect();
    assert_eq!(
        rest_cells,
        vec![
            (CostCell::Free304, StatusClass::NotModified),
            (CostCell::Unknown, StatusClass::Network),
            (CostCell::Requests(1), StatusClass::Success),
        ]
    );

    // Sorting by duration gives the old "slowest requests" view.
    let slowest = request_log(
        &rows,
        &q(
            RequestFilters::default(),
            LogSort {
                key: LogSortKey::Duration,
                descending: true,
            },
        ),
    );
    assert_eq!(
        slowest
            .rows
            .iter()
            .map(|r| r.duration_ms)
            .collect::<Vec<_>>(),
        vec![3_000, 2_000, 1_500, 900, 100, 50]
    );

    let network = request_log(
        &rows,
        &q(
            RequestFilters {
                status_class: Some(StatusClass::Network),
                ..Default::default()
            },
            LogSort::default(),
        ),
    );
    assert_eq!(network.rows.len(), 1);
    assert_eq!(network.rows[0].status, 0);
}

// --- D16 -------------------------------------------------------------------------------

#[test]
fn auth_and_local_never_enter_budget_figures() {
    // Only auth and local records: every Budget-view procedure is empty
    // while the Requests view still sees them.
    let mut auth = rec(AS_OF - MIN, Transport::Auth, "device-flow");
    auth.remaining_after = Some(10);
    auth.limit_after = Some(5_000);
    auth.cycle_reset_epoch = Some((AS_OF + 10 * MIN) / 1_000);
    let local = rec(AS_OF - MIN, Transport::Local, "export");
    let rows = vec![auth, local];
    let cards = budget_cards(&rows, &cards_query(DAY));
    for c in &cards.cards {
        assert_eq!(c.snapshot, None);
        assert_eq!(c.recorded, SpendTotals::default());
    }
    for budget in Budget::all() {
        let s = scope(budget, DAY);
        let w = where_it_went(
            &rows,
            &WhereQuery {
                scope: s.clone(),
                group_by: SpendDimension::Module,
                then_by: None,
            },
        );
        assert!(w.groups.is_empty());
        let h = window_history(
            &rows,
            &HistoryQuery {
                scope: s.clone(),
                tz_offset_ms: 0,
            },
        );
        assert_eq!(h.unwindowed, SpendTotals::default());
        assert!(matches!(&h.view, HistoryView::Windows { columns, .. } if columns.is_empty()));
        assert!(
            avoidable(&rows, &AvoidableQuery { scope: s })
                .entries
                .is_empty()
        );
    }
    let log = request_log(
        &rows,
        &RequestLogQuery {
            as_of_ms: AS_OF,
            span: Span::H1,
            filters: RequestFilters::default(),
            sort: LogSort::default(),
        },
    );
    assert_eq!(log.rows.len(), 2);
}

// --- serde (0054 D13: the CLI/MCP carries these verbatim) -----------------------

#[test]
fn outputs_round_trip_through_serde() {
    fn round_trip<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(v: T) {
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<T>(&json).unwrap(), v);
    }
    let (mut rows, _) = gql_window(1_000, 5);
    rows.extend(mixed_rows());
    let s = scope(Budget::Graphql, Preset::CurrentWindow);
    round_trip(cards_query(Preset::CurrentWindow));
    round_trip(budget_cards(&rows, &cards_query(Preset::CurrentWindow)));
    round_trip(where_it_went(
        &rows,
        &WhereQuery {
            scope: s.clone(),
            group_by: SpendDimension::Surface,
            then_by: Some(SpendDimension::Operation),
        },
    ));
    for preset in [Preset::CurrentWindow, DAY, Preset::Trailing(Span::D90)] {
        round_trip(window_history(
            &rows,
            &HistoryQuery {
                scope: scope(Budget::Graphql, preset),
                tz_offset_ms: 0,
            },
        ));
    }
    round_trip(avoidable(&rows, &AvoidableQuery { scope: s }));
    round_trip(request_trend(
        &rows,
        &trend_query(RequestFilters::default(), Measure::P50),
    ));
    round_trip(request_log(
        &rows,
        &RequestLogQuery {
            as_of_ms: AS_OF,
            span: Span::D7,
            filters: RequestFilters::default(),
            sort: LogSort::default(),
        },
    ));
}
