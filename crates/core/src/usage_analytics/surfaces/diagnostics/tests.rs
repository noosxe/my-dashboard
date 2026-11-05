//! Hand-made fixtures, exact by construction (as `surfaces/tests.rs`).

use super::*;

const MIN: u64 = 60_000;
const HOUR: u64 = 60 * MIN;
/// 13:47 UTC on day 20 000.
const AS_OF: u64 = 20_000 * 24 * HOUR + 13 * HOUR + 47 * MIN;

fn rec(ts: u64, transport: Transport, op: &str, duration_ms: u32) -> UsageRecord {
    let mut r = UsageRecord::new("github", op, "pr-page", "view-shown");
    r.ts_epoch_ms = ts;
    r.transport = transport;
    r.status = 200;
    r.cost = Some(1);
    r.duration_ms = duration_ms;
    r
}

fn scope(span: Span, filters: RequestFilters) -> RequestScope {
    RequestScope {
        as_of_ms: AS_OF,
        span,
        tz_offset_ms: 0,
        filters,
    }
}

fn all(span: Span) -> RequestScope {
    scope(span, RequestFilters::default())
}

#[test]
fn the_glance_keeps_every_transport_and_counts_slow_and_failed_requests() {
    let mut failed = rec(AS_OF - MIN, Transport::Rest, "a", 1_000);
    failed.status = 502;
    let rows = vec![
        rec(AS_OF - MIN, Transport::Rest, "a", 100),
        failed,
        rec(AS_OF - MIN, Transport::Auth, "login", 999),
        // Outside the hour.
        rec(AS_OF - 2 * HOUR, Transport::Graphql, "q", 5_000),
    ];
    let g = request_glance(&rows, &all(Span::H1));
    let order: Vec<_> = g.transports.iter().map(|t| t.transport).collect();
    assert_eq!(order, GLANCE_TRANSPORTS);
    let rest = &g.transports[0];
    assert_eq!(
        (rest.requests, rest.p50_ms, rest.p90_ms),
        (2, Some(100), Some(1_000))
    );
    assert_eq!(g.transports[1].p90_ms, None, "no GraphQL in range: —");
    assert_eq!(g.transports[2].requests, 1);
    assert_eq!(
        (g.requests, g.slow, g.errors),
        (3, 1, 1),
        "1 000 ms is slow"
    );
    // The facets narrow it.
    let rest_only = request_glance(
        &rows,
        &scope(
            Span::H1,
            RequestFilters {
                transport: Some(Transport::Auth),
                ..Default::default()
            },
        ),
    );
    assert_eq!((rest_only.requests, rest_only.slow), (1, 0));
    assert_eq!(rest_only.transports[0].requests, 0);
}

#[test]
fn the_histogram_keeps_all_six_bands_half_open_below() {
    let rows: Vec<_> = [99, 100, 250, 999, 1_000, 2_000, 2_001]
        .into_iter()
        .map(|d| rec(AS_OF - MIN, Transport::Rest, "a", d))
        .collect();
    let d = request_diagnostics(&rows, &all(Span::H24));
    assert_eq!(d.histogram.counts, [1, 1, 1, 1, 1, 2]);
    assert_eq!(d.histogram.requests, 7);
    let empty = request_diagnostics(&[], &all(Span::H24));
    assert_eq!(empty.histogram.counts, [0; 6], "empty bands kept");
}

#[test]
fn the_payload_grid_counts_unmeasured_bodies_beside_it() {
    let mut small = rec(AS_OF - MIN, Transport::Rest, "a", 50);
    small.resp_bytes = Some(1_023);
    let mut edge = rec(AS_OF - MIN, Transport::Rest, "a", 1_500);
    edge.resp_bytes = Some(1_024);
    let mut huge = rec(AS_OF - MIN, Transport::Rest, "a", 3_000);
    huge.resp_bytes = Some(2_000_000);
    let mut revalidated = rec(AS_OF - MIN, Transport::Rest, "a", 40);
    revalidated.etag_304 = true;
    let d = request_diagnostics(&[small, edge, huge, revalidated], &all(Span::H24));
    let g = &d.payload;
    assert_eq!(g.cells.len(), 6);
    assert_eq!(g.cells[0], [1, 0, 0, 0, 0]);
    assert_eq!(g.cells[4], [0, 1, 0, 0, 0], "1 024 bytes is 1–10 KB");
    assert_eq!(g.cells[5], [0, 0, 0, 0, 1]);
    assert_eq!(g.unmeasured, 1);
}

#[test]
fn contention_counts_overlapping_spans_and_never_touching_ones() {
    // Spans [t−d, t): a = [0, 10 s), b = [5 s, 15 s), c = [10 s, 20 s)
    // touches a's end. Peak 2, first at 5 s, ops a and b.
    let base = AS_OF - 30 * MIN;
    let mut a = rec(base + 10_000, Transport::Rest, "a", 10_000);
    a.queue_ms = Some(150);
    let mut b = rec(base + 15_000, Transport::Rest, "b", 10_000);
    b.queue_ms = Some(20);
    let c = rec(base + 20_000, Transport::Rest, "c", 10_000);
    let zero = rec(base + 5_000, Transport::Rest, "z", 0);
    let d = request_diagnostics(&[a, b, c, zero], &all(Span::H1));
    let k = &d.contention;
    assert_eq!(k.peak_in_flight, 2);
    assert_eq!(k.peak_at_ms, Some(base + 5_000));
    assert_eq!(k.peak_ops, ["a", "b"]);
    assert_eq!((k.queued, k.queued_notable), (2, 1));
    assert_eq!((k.queue_p50_ms, k.queue_p90_ms), (Some(20), Some(150)));
    // 1h buckets by 5 minutes, clock-aligned: 13 buckets from 12:45.
    assert_eq!(k.buckets.len(), 13);
    assert_eq!(k.buckets[0].start_ms % (5 * MIN), 0);
    let busy: Vec<_> = k
        .buckets
        .iter()
        .filter(|b| b.max_in_flight > 0)
        .map(|b| b.max_in_flight)
        .collect();
    assert_eq!(busy, [2]);
    let idle = request_diagnostics(&[], &all(Span::H1)).contention;
    assert_eq!((idle.peak_in_flight, idle.peak_at_ms), (0, None));
}

#[test]
fn revalidation_compares_ops_with_both_sides_only() {
    let full = |d: u32, bytes: u32| {
        let mut r = rec(AS_OF - MIN, Transport::Rest, "notifications", d);
        r.resp_bytes = Some(bytes);
        r
    };
    let reval = |op: &str, d: u32| {
        let mut r = rec(AS_OF - MIN, Transport::Rest, op, d);
        r.status = 304;
        r.etag_304 = true;
        r
    };
    let rows = vec![
        full(300, 6_000),
        full(500, 4_000),
        reval("notifications", 100),
        reval("notifications", 120),
        // One side only: no comparison.
        reval("pr-detail", 80),
    ];
    let ops = request_diagnostics(&rows, &all(Span::H24)).revalidation;
    assert_eq!(ops.len(), 1);
    let n = &ops[0];
    assert_eq!((n.full, n.full_median_ms), (2, 300));
    assert_eq!((n.revalidated, n.revalidated_median_ms), (2, 100));
    assert_eq!(n.delta_ms, 200);
    assert_eq!(n.bytes_saved_est, Some(10_000), "5 000 mean × 2");
}

#[test]
fn every_output_round_trips_through_serde() {
    let rows = vec![rec(AS_OF - MIN, Transport::Local, "favicon", 1_200)];
    let d = request_diagnostics(&rows, &all(Span::D7));
    let back: Diagnostics = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
    assert_eq!(back, d);
    let g = request_glance(&rows, &all(Span::D7));
    let back: RequestGlance = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
    assert_eq!(back, g);
}

// Ported from the old Request latency tab's tests (#337, #410; deleted
// with its procedures in #696), onto the Requests inputs. Content is
// unchanged; the two deliberate differences are noted where they land.

#[test]
fn ported_histogram_bands_are_fixed_boundaries_and_keep_empties() {
    let durations = [99u32, 100, 249, 250, 499, 500, 999, 1000, 1999, 2000];
    let rows: Vec<_> = durations
        .iter()
        .enumerate()
        .map(|(i, d)| rec(AS_OF - 10 - i as u64, Transport::Rest, "s", *d))
        .collect();
    let h = request_diagnostics(&rows, &all(Span::H1)).histogram;
    assert_eq!(h.counts, [1, 2, 2, 2, 2, 1]);
    assert_eq!(h.requests, 10);
    assert_eq!(DURATION_LABELS[0], "<100 ms");
    assert_eq!(DURATION_LABELS[5], ">2 s");
    let single = rec(AS_OF - 5, Transport::Rest, "s", 3_000);
    let h = request_diagnostics(&[single], &all(Span::H1)).histogram;
    assert_eq!(h.counts, [0, 0, 0, 0, 0, 1], "band order is fixed");
}

#[test]
fn ported_glance_splits_per_transport_without_pooling() {
    let mut rows = Vec::new();
    // REST 100, 100, 900 → p50 100, p90 900.
    for (i, d) in [100u32, 100, 900].iter().enumerate() {
        rows.push(rec(AS_OF - 10 - i as u64, Transport::Rest, "s", *d));
    }
    // GraphQL 1200, 1500: fewer and heavier, what pooling would hide.
    for (i, d) in [1200u32, 1500].iter().enumerate() {
        rows.push(rec(AS_OF - 20 - i as u64, Transport::Graphql, "s", *d));
    }
    rows.push(rec(AS_OF - 30, Transport::Auth, "s", 200));
    let g = request_glance(&rows, &all(Span::H1));
    assert_eq!((g.requests, g.slow), (6, 2));
    let t = |i: usize| {
        let t = &g.transports[i];
        (t.requests, t.p50_ms, t.p90_ms)
    };
    assert_eq!(t(0), (3, Some(100), Some(900)));
    assert_eq!(t(1), (2, Some(1200), Some(1500)));
    assert_eq!(t(2), (1, Some(200), Some(200)));
    // Changed (#698, owner decision): every transport keeps its tile,
    // in fixed order; one with nothing in range reads empty.
    assert_eq!(t(3), (0, None, None));
    // The facet narrows the strip to its own transport's figures.
    let gql = request_glance(
        &rows,
        &scope(
            Span::H1,
            RequestFilters {
                transport: Some(Transport::Graphql),
                ..Default::default()
            },
        ),
    );
    assert_eq!(gql.requests, 2);
    assert_eq!(gql.transports[0].requests, 0);
    assert_eq!(gql.transports[1].requests, 2);
}

#[test]
fn ported_revalidation_compares_medians_counts_and_bytes() {
    let mk = |ts: u64, op: &str, d: u32, bytes: Option<u32>, revalidated: bool| {
        let mut r = rec(ts, Transport::Rest, op, d);
        r.resp_bytes = bytes;
        if revalidated {
            r.status = 304;
            r.etag_304 = true;
        }
        r
    };
    let mut rows = vec![
        mk(AS_OF - 10, "op-a", 100, Some(1_000), false),
        mk(AS_OF - 11, "op-a", 300, Some(2_000), false),
        mk(AS_OF - 20, "op-a", 50, None, true),
        mk(AS_OF - 21, "op-a", 70, None, true),
        // One side only: neither compares.
        mk(AS_OF - 30, "op-b", 900, None, false),
        mk(AS_OF - 40, "op-c", 40, None, true),
    ];
    let ops = request_diagnostics(&rows, &all(Span::H1)).revalidation;
    assert_eq!(ops.len(), 1, "both sides or nothing");
    let a = &ops[0];
    assert_eq!(a.op, "op-a");
    // Nearest-rank medians: [100, 300] → 100, [50, 70] → 50.
    assert_eq!((a.full, a.full_median_ms), (2, 100));
    assert_eq!((a.revalidated, a.revalidated_median_ms), (2, 50));
    assert_eq!(a.delta_ms, 50);
    assert_eq!(a.bytes_saved_est, Some(3_000), "1.5 KB × 2 revalidations");
    // A bigger win with no bodies recorded sorts first, no estimate.
    rows.push(mk(AS_OF - 50, "op-d", 1_000, None, false));
    rows.push(mk(AS_OF - 51, "op-d", 100, None, true));
    let ops = request_diagnostics(&rows, &all(Span::H1)).revalidation;
    let names: Vec<_> = ops.iter().map(|o| o.op.as_str()).collect();
    assert_eq!(names, ["op-d", "op-a"], "Δ median desc");
    assert_eq!((ops[0].delta_ms, ops[0].bytes_saved_est), (900, None));
}

#[test]
fn ported_payload_split_crosses_bands_and_excludes_unmeasured() {
    let mk = |ts: u64, d: u32, bytes: Option<u32>| {
        let mut r = rec(ts, Transport::Rest, "s", d);
        r.resp_bytes = bytes;
        r
    };
    let mut revalidated = mk(AS_OF - 50, 40, None);
    revalidated.status = 304;
    revalidated.etag_304 = true;
    let rows = vec![
        mk(AS_OF - 10, 99, Some(500)),
        mk(AS_OF - 20, 100, Some(1_024)),
        mk(AS_OF - 30, 1_500, Some(5_000)),
        mk(AS_OF - 40, 1_500, Some(500_000)),
        mk(AS_OF - 60, 3_000, Some(2_000_000)),
        revalidated,
    ];
    let d = request_diagnostics(&rows, &all(Span::H1));
    let p = &d.payload;
    assert_eq!(d.requests, 6);
    assert_eq!(p.unmeasured, 1, "the 304 has no body to measure");
    assert_eq!(
        SIZE_LABELS,
        ["<1 KB", "1–10 KB", "10–100 KB", "100 KB–1 MB", ">1 MB"]
    );
    assert_eq!(p.cells.len(), 6, "all six duration bands, always");
    assert_eq!(p.cells[0], [1, 0, 0, 0, 0]);
    assert_eq!(p.cells[1], [0, 1, 0, 0, 0], "1 024 B is 1–10 KB");
    assert_eq!(p.cells[2], [0, 0, 0, 0, 0], "empty bands kept");
    assert_eq!(p.cells[4], [0, 1, 0, 1, 0]);
    assert_eq!(p.cells[5], [0, 0, 0, 0, 1]);
}

#[test]
fn ported_contention_peak_touching_spans_and_queue_evidence() {
    // Spans are [ts − duration, ts): one ending where another starts
    // never overlaps it.
    let t = AS_OF - 30 * MIN;
    let mk = |ts: u64, d: u32, op: &str, queue: Option<u32>| {
        let mut r = rec(ts, Transport::Rest, op, d);
        r.queue_ms = queue;
        r
    };
    // a [t−100, t), b [t−150, t−50), c [t−90, t−60): all three overlap
    // around t−70, so the peak is 3 from t−90. d starts where b ends.
    let rows = vec![
        mk(t, 100, "pr-detail", Some(30)),
        mk(t - 50, 100, "pr-list", Some(120)),
        mk(t - 60, 30, "issue-list", None),
        mk(t - 10, 40, "pr-files", Some(200)),
        mk(t - 70, 0, "zero", None),
    ];
    let c = request_diagnostics(&rows, &all(Span::H1)).contention;
    assert_eq!(c.peak_in_flight, 3);
    assert_eq!(
        c.peak_at_ms,
        Some(t - 90),
        "first instant the peak is reached"
    );
    assert_eq!(c.peak_ops, ["issue-list", "pr-detail", "pr-list"]);
    // Changed (#698): buckets are the trend's clock-aligned ones (D10),
    // not 40 equal slices; the burst still lands in exactly one.
    let busy: Vec<_> = c
        .buckets
        .iter()
        .map(|b| b.max_in_flight)
        .filter(|m| *m > 0)
        .collect();
    assert_eq!(busy, [3]);
    assert_eq!(c.queued, 3);
    assert_eq!(c.queued_notable, 2, "120 ms and 200 ms; 30 ms is jitter");
    assert_eq!((c.queue_p50_ms, c.queue_p90_ms), (Some(120), Some(200)));
}
