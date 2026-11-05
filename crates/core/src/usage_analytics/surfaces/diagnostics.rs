//! The Requests view's glance strip and Diagnostics (docs/0082 D10
//! bands 1 and 4): per-transport latency at a glance, then the duration
//! histogram, the payload × duration grid, contention and the
//! 304-vs-200 comparison. The Diagnostics content is docs/0054 §6's
//! (#337, #410), unchanged; #696 removed those procedures with the old
//! tabs, and they return here on the Requests view's inputs: a trailing
//! [`Span`] from `as_of` plus the view's [`RequestFilters`], buckets
//! aligned to the local clock like [`request_trend`](super::request_trend).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Bounds, RequestFilters, Span, request_rows, trend_bucket_ms};
use crate::usage_analytics::{is_error, percentile_of};
use crate::usage_records::{Transport, UsageRecord};

/// "Slower than 1 s" (D21): the same boundary as the histogram's
/// `1–2 s` band, so the two count the same requests.
pub const SLOW_MS: u32 = 1_000;

/// The histogram's duration bands: upper bounds, half-open below
/// (100 ms lands in `100–250 ms`).
const DURATION_BOUNDS_MS: [u32; 5] = [100, 250, 500, 1_000, 2_000];

/// Band labels in band order, never re-sorted by count: the tail's
/// position is the information (spike or plateau).
pub const DURATION_LABELS: [&str; 6] = [
    "<100 ms",
    "100–250 ms",
    "250–500 ms",
    "0.5–1 s",
    "1–2 s",
    ">2 s",
];

/// The payload bands: binary thresholds, so a body that formats as
/// `1.0 KB` is never in `<1 KB`.
const SIZE_BOUNDS_BYTES: [u32; 4] = [1_024, 10_240, 102_400, 1_048_576];

pub const SIZE_LABELS: [&str; 5] = ["<1 KB", "1–10 KB", "10–100 KB", "100 KB–1 MB", ">1 MB"];

/// Queue waits at or above this read as contention evidence; below it
/// a wait is ordinary scheduling jitter.
pub const QUEUE_NOTABLE_MS: u32 = 100;

/// The glance strip's transports, in tile order (D10, D16: `auth` and
/// `local` are reachable here and only here).
pub const GLANCE_TRANSPORTS: [Transport; 4] = [
    Transport::Rest,
    Transport::Graphql,
    Transport::Auth,
    Transport::Local,
];

fn duration_band(ms: u32) -> usize {
    DURATION_BOUNDS_MS
        .iter()
        .position(|&upper| ms < upper)
        .unwrap_or(DURATION_LABELS.len() - 1)
}

fn size_band(bytes: u32) -> usize {
    SIZE_BOUNDS_BYTES
        .iter()
        .position(|&upper| bytes < upper)
        .unwrap_or(SIZE_LABELS.len() - 1)
}

/// The Requests view's selection: a trailing span plus the facets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestScope {
    pub as_of_ms: u64,
    pub span: Span,
    /// Local offset from UTC, ms: contention buckets align to the
    /// local clock, like the trend's.
    pub tz_offset_ms: i64,
    pub filters: RequestFilters,
}

impl RequestScope {
    fn rows<'a>(&self, rows: &'a [UsageRecord]) -> (Bounds, Vec<&'a UsageRecord>) {
        let b = Bounds::trailing(self.as_of_ms, self.span);
        (b, request_rows(rows, b, &self.filters))
    }
}

// ---------------------------------------------------------------------------
// Band 1 — the glance strip
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransportGlance {
    pub transport: Transport,
    pub requests: u64,
    /// `None` with no requests: the tile reads "—", never 0 ms.
    pub p50_ms: Option<u32>,
    pub p90_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestGlance {
    /// Every transport in [`GLANCE_TRANSPORTS`] order, empty ones kept.
    pub transports: Vec<TransportGlance>,
    pub requests: u64,
    /// Requests at or above [`SLOW_MS`].
    pub slow: u64,
    /// 4xx, 5xx and network failures.
    pub errors: u64,
}

/// Band 1 (D10): p90, p50 and the count per transport, then the slow
/// and error counts, all over the selection.
pub fn request_glance(rows: &[UsageRecord], q: &RequestScope) -> RequestGlance {
    let (_, scoped) = q.rows(rows);
    let transports = GLANCE_TRANSPORTS
        .iter()
        .map(|&t| {
            let durations: Vec<u32> = scoped
                .iter()
                .filter(|r| r.transport == t)
                .map(|r| r.duration_ms)
                .collect();
            let some = !durations.is_empty();
            TransportGlance {
                transport: t,
                requests: durations.len() as u64,
                p50_ms: some.then(|| percentile_of(durations.clone(), 50)),
                p90_ms: some.then(|| percentile_of(durations, 90)),
            }
        })
        .collect();
    RequestGlance {
        transports,
        requests: scoped.len() as u64,
        slow: scoped.iter().filter(|r| r.duration_ms >= SLOW_MS).count() as u64,
        errors: scoped.iter().filter(|r| is_error(r)).count() as u64,
    }
}

// ---------------------------------------------------------------------------
// Band 4 — Diagnostics
// ---------------------------------------------------------------------------

/// The histogram: requests per duration band, all six in band order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DurationHistogram {
    /// Counts in [`DURATION_LABELS`] order, empty bands kept.
    pub counts: Vec<u64>,
    pub requests: u64,
}

/// The payload × duration grid: the histogram's bands crossed with the
/// size bands, so "slow because heavy" separates from "slow because
/// far".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PayloadGrid {
    /// One row per duration band, one cell per size band (label
    /// orders), empty cells kept.
    pub cells: Vec<Vec<u64>>,
    /// Requests with no measured body (304s, network errors): counted
    /// beside the grid, never guessed into a band.
    pub unmeasured: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentionBucket {
    pub start_ms: u64,
    pub end_ms: u64,
    /// The most requests in flight at once during the bucket.
    pub max_in_flight: u64,
}

/// How many requests were in flight at once (self-inflicted
/// contention) and the queue waits it caused. A request spans
/// `[ts − duration, ts)`: half-open, so touching spans never overlap
/// and a zero-duration span occupies nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contention {
    pub peak_in_flight: u64,
    /// The first instant the peak was reached; `None` when nothing was
    /// ever in flight.
    pub peak_at_ms: Option<u64>,
    /// Distinct ops in flight at that instant, sorted.
    pub peak_ops: Vec<String>,
    /// Clock-aligned buckets ([`trend_bucket_ms`]), empties kept.
    pub buckets: Vec<ContentionBucket>,
    /// Requests that recorded a queue wait at all.
    pub queued: u64,
    /// …of those, waits ≥ [`QUEUE_NOTABLE_MS`].
    pub queued_notable: u64,
    pub queue_p50_ms: Option<u32>,
    pub queue_p90_ms: Option<u32>,
}

/// One op's 304-vs-200 comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevalidationOp {
    pub op: String,
    pub full: u64,
    pub full_median_ms: u32,
    pub revalidated: u64,
    pub revalidated_median_ms: u32,
    /// Full median − revalidated median: positive means revalidating
    /// was faster.
    pub delta_ms: i64,
    /// ≈ mean full-response body × revalidations: an estimate from the
    /// 200 side (a 304 has no body), which the view marks `≈`.
    pub bytes_saved_est: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostics {
    pub histogram: DurationHistogram,
    pub payload: PayloadGrid,
    pub contention: Contention,
    /// Ops with both a 200 and a 304 in range, Δ median desc, then
    /// full responses desc, op asc.
    pub revalidation: Vec<RevalidationOp>,
    pub requests: u64,
}

/// Band 4 (D10): the four Diagnostics views over one selection.
pub fn request_diagnostics(rows: &[UsageRecord], q: &RequestScope) -> Diagnostics {
    let (bounds, scoped) = q.rows(rows);
    Diagnostics {
        histogram: histogram(&scoped),
        payload: payload(&scoped),
        contention: contention(&scoped, bounds, q),
        revalidation: revalidation(&scoped),
        requests: scoped.len() as u64,
    }
}

fn histogram(scoped: &[&UsageRecord]) -> DurationHistogram {
    let mut counts = vec![0u64; DURATION_LABELS.len()];
    for r in scoped {
        counts[duration_band(r.duration_ms)] += 1;
    }
    DurationHistogram {
        counts,
        requests: scoped.len() as u64,
    }
}

fn payload(scoped: &[&UsageRecord]) -> PayloadGrid {
    let mut cells = vec![vec![0u64; SIZE_LABELS.len()]; DURATION_LABELS.len()];
    let mut unmeasured = 0;
    for r in scoped {
        match r.resp_bytes {
            Some(b) => cells[duration_band(r.duration_ms)][size_band(b)] += 1,
            None => unmeasured += 1,
        }
    }
    PayloadGrid { cells, unmeasured }
}

fn contention(scoped: &[&UsageRecord], bounds: Bounds, q: &RequestScope) -> Contention {
    let spans: Vec<(u64, u64, &UsageRecord)> = scoped
        .iter()
        .filter(|r| r.duration_ms > 0)
        .map(|r| {
            let end = r.ts_epoch_ms;
            (end.saturating_sub(u64::from(r.duration_ms)), end, *r)
        })
        .collect();
    // Ends sort before starts at the same instant: half-open spans.
    let mut events: Vec<(u64, i64)> = spans
        .iter()
        .flat_map(|(s, e, _)| [(*s, 1i64), (*e, -1)])
        .collect();
    events.sort_unstable();

    let (mut running, mut peak, mut peak_at) = (0i64, 0u64, None);
    for &(t, d) in &events {
        running += d;
        if running > peak as i64 {
            peak = running as u64;
            peak_at = Some(t);
        }
    }
    let mut peak_ops: Vec<String> = peak_at.map_or_else(Vec::new, |at| {
        spans
            .iter()
            .filter(|(s, e, _)| *s <= at && at < *e)
            .map(|(_, _, r)| r.op.to_string())
            .collect()
    });
    peak_ops.sort();
    peak_ops.dedup();

    // Per bucket: the events up to and including its start are its
    // entry state, then every event strictly inside it.
    let len = trend_bucket_ms(q.span);
    let first = align(bounds.from_ms, len, q.tz_offset_ms);
    let count = bounds.to_ms.saturating_sub(first).div_ceil(len) as usize;
    let mut buckets = Vec::with_capacity(count);
    let (mut ix, mut running) = (0usize, 0i64);
    for i in 0..count as u64 {
        let (b0, b1) = (first + i * len, first + (i + 1) * len);
        while ix < events.len() && events[ix].0 <= b0 {
            running += events[ix].1;
            ix += 1;
        }
        let mut max = running.max(0) as u64;
        while ix < events.len() && events[ix].0 < b1 {
            running += events[ix].1;
            max = max.max(running.max(0) as u64);
            ix += 1;
        }
        buckets.push(ContentionBucket {
            start_ms: b0,
            end_ms: b1,
            max_in_flight: max,
        });
    }

    let waits: Vec<u32> = scoped.iter().filter_map(|r| r.queue_ms).collect();
    let some = !waits.is_empty();
    Contention {
        peak_in_flight: peak,
        peak_at_ms: peak_at,
        peak_ops,
        buckets,
        queued: waits.len() as u64,
        queued_notable: waits.iter().filter(|w| **w >= QUEUE_NOTABLE_MS).count() as u64,
        queue_p50_ms: some.then(|| percentile_of(waits.clone(), 50)),
        queue_p90_ms: some.then(|| percentile_of(waits, 90)),
    }
}

/// The local-clock bucket start at or before `ts` (as the trend aligns).
fn align(ts: u64, len: u64, tz_offset_ms: i64) -> u64 {
    let local = ts as i64 + tz_offset_ms;
    (local.div_euclid(len as i64) * len as i64 - tz_offset_ms).max(0) as u64
}

fn revalidation(scoped: &[&UsageRecord]) -> Vec<RevalidationOp> {
    let mut by_op: BTreeMap<&str, (Vec<&UsageRecord>, Vec<&UsageRecord>)> = BTreeMap::new();
    for r in scoped {
        let sides = by_op.entry(r.op.as_ref()).or_default();
        if r.etag_304 {
            sides.1.push(r);
        } else if r.status == 200 {
            sides.0.push(r);
        }
    }
    let median =
        |rs: &[&UsageRecord]| percentile_of(rs.iter().map(|r| r.duration_ms).collect(), 50);
    let mut ops: Vec<RevalidationOp> = by_op
        .into_iter()
        .filter(|(_, (full, reval))| !full.is_empty() && !reval.is_empty())
        .map(|(op, (full, reval))| {
            let (fm, rm) = (median(&full), median(&reval));
            let bytes: Vec<u64> = full
                .iter()
                .filter_map(|r| r.resp_bytes)
                .map(u64::from)
                .collect();
            let mean = (!bytes.is_empty()).then(|| bytes.iter().sum::<u64>() / bytes.len() as u64);
            RevalidationOp {
                op: op.to_string(),
                full: full.len() as u64,
                full_median_ms: fm,
                revalidated: reval.len() as u64,
                revalidated_median_ms: rm,
                delta_ms: i64::from(fm) - i64::from(rm),
                bytes_saved_est: mean.map(|m| m * reval.len() as u64),
            }
        })
        .collect();
    ops.sort_by(|a, b| {
        b.delta_ms
            .cmp(&a.delta_ms)
            .then(b.full.cmp(&a.full))
            .then_with(|| a.op.cmp(&b.op))
    });
    ops
}

#[cfg(test)]
mod tests;
