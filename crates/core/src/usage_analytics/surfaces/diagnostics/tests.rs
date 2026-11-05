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
