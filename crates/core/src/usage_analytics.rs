//! Usage analytics (docs/0082): the calculator and the usage page.
//!
//! - [`surfaces`] — the pure per-surface procedures (0082 D15a): rows
//!   plus a query in, serde structs of plain owned types out; nothing
//!   there touches storage, the clock or gpui.
//! - [`page`] — the usage page shell (0082 D1, D3, D11, D18–D20) and
//!   one module per view.
//!
//! This root keeps the vocabulary both share. Honesty rules carried
//! from the design: unknown costs are counted, never priced as 0;
//! REST and GraphQL points are separate pools, never summed;
//! percentiles are nearest-rank (docs/0054 D12).

use serde::{Deserialize, Serialize};

use crate::usage_records::UsageRecord;

/// The usage page (0082 D1–D3, D18–D20).
pub mod page;
/// The redesigned surfaces' procedures (docs/0082 D15).
pub mod surfaces;

pub use page::UsagePage;

/// The agent `api_usage` window vocabulary (`1h|1d|1w|1m`, docs/0068
/// §6). The page's presets are [`surfaces::Preset`] since docs/0082
/// D11.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Range {
    H1,
    D1,
    W1,
    M1,
}

impl Range {
    pub const fn ms(self) -> u64 {
        match self {
            Range::H1 => 3_600_000,
            Range::D1 => 86_400_000,
            Range::W1 => 604_800_000,
            Range::M1 => 2_592_000_000,
        }
    }

    /// Shortest first: 1h · 1d · 1w · 1m.
    pub fn all() -> [Range; 4] {
        [Range::H1, Range::D1, Range::W1, Range::M1]
    }

    pub fn label(self) -> &'static str {
        match self {
            Range::H1 => "1h",
            Range::D1 => "1d",
            Range::W1 => "1w",
            Range::M1 => "1m",
        }
    }
}

/// An errored request: a network failure or a failed status (docs/0054
/// D12).
fn is_error(rec: &UsageRecord) -> bool {
    rec.network_error || rec.status >= 400
}

/// The six categoricals of the fact table (docs/0054 D4). `Target` is
/// `ident`, rendered `—` when `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dimension {
    Module,
    Transport,
    Surface,
    Trigger,
    Operation,
    Target,
}

impl Dimension {
    fn value(&self, rec: &UsageRecord) -> String {
        match self {
            Dimension::Module => rec.module.to_string(),
            Dimension::Transport => rec.transport.as_str().to_string(),
            Dimension::Surface => rec.surface.to_string(),
            Dimension::Trigger => rec.trigger.to_string(),
            Dimension::Operation => rec.op.to_string(),
            Dimension::Target => rec
                .ident
                .as_ref()
                .map(|i| i.to_string())
                .unwrap_or_else(|| "—".to_string()),
        }
    }

    /// Picker and table-header label (D4).
    pub fn label(self) -> &'static str {
        match self {
            Dimension::Module => "Module",
            Dimension::Transport => "Transport",
            Dimension::Surface => "Surface",
            Dimension::Trigger => "Trigger",
            Dimension::Operation => "Operation",
            Dimension::Target => "Target",
        }
    }

    /// The six categoricals in picker order (D4). Selector count is
    /// part of the contract; this list must not grow silently.
    pub fn all() -> [Dimension; 6] {
        [
            Dimension::Surface,
            Dimension::Trigger,
            Dimension::Operation,
            Dimension::Target,
            Dimension::Transport,
            Dimension::Module,
        ]
    }

    pub fn from_label(label: &str) -> Option<Dimension> {
        Dimension::all().into_iter().find(|d| d.label() == label)
    }
}

/// Nearest-rank percentile (docs/0054 D12): sorted samples, rank
/// `ceil(p·n/100)` clamped to `[1, n]`, value at that 1-based rank.
fn percentile_of(mut vals: Vec<u32>, p: u64) -> u32 {
    vals.sort_unstable();
    let n = vals.len();
    if n == 0 {
        return 0;
    }
    let rank = (p * n as u64).div_ceil(100).clamp(1, n as u64) as usize;
    vals[rank - 1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_records::Transport;

    #[test]
    fn percentiles_are_nearest_rank() {
        // 10..=19 (n=10): p50 rank 5 → 14, p90 rank 9 → 18.
        let vals: Vec<u32> = (10..20).collect();
        assert_eq!(percentile_of(vals.clone(), 50), 14);
        assert_eq!(percentile_of(vals.clone(), 90), 18);
        // n=2: p50 rank 1 → lower; p90 rank 2 → higher.
        assert_eq!(percentile_of(vals[..2].to_vec(), 50), 10);
        assert_eq!(percentile_of(vals[..2].to_vec(), 90), 11);
        assert_eq!(percentile_of(Vec::new(), 90), 0);
    }

    #[test]
    fn errors_are_network_failures_or_failed_statuses() {
        let mut r = UsageRecord::new("github", "pr-detail", "pr-page", "view-shown");
        r.status = 200;
        assert!(!is_error(&r));
        r.status = 404;
        assert!(is_error(&r));
        r.status = 200;
        r.network_error = true;
        assert!(is_error(&r));
    }

    #[test]
    fn target_dimension_renders_a_missing_ident_as_dash() {
        let mut r = UsageRecord::new("github", "pr-detail", "pr-page", "view-shown");
        r.transport = Transport::Graphql;
        assert_eq!(Dimension::Target.value(&r), "—");
        assert_eq!(Dimension::Transport.value(&r), Transport::Graphql.as_str());
        for d in Dimension::all() {
            assert_eq!(Dimension::from_label(d.label()), Some(d));
        }
    }
}
